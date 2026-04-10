//! HITL-aware `QueryBackend` decorator.
//!
//! Wraps any inner `QueryBackend` and intercepts `on_tool_call`. For
//! risky tools (currently `bash` only), it first consults the inner
//! backend (which walks the Python middleware chain). If Python allows,
//! it then prompts the user via the UI thread through a one-shot
//! approval channel.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use ash_ipc::pb;
use ash_query::QueryBackend;
use async_trait::async_trait;
use futures::stream::BoxStream;
use tokio::sync::{mpsc, oneshot};

/// The raw data a TUI shows in the approval dialog.
#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub tool_name: String,
    pub arguments: String, // UTF-8 JSON, best-effort
}

/// User's reply. Gets converted into a `pb::HookDecision`.
#[derive(Debug, Clone)]
pub enum ApprovalDecision {
    Allow,
    Deny { reason: String },
}

impl ApprovalDecision {
    pub fn into_hook_decision(self) -> pb::HookDecision {
        match self {
            ApprovalDecision::Allow => pb::HookDecision {
                kind: pb::hook_decision::Kind::Allow as i32,
                reason: String::new(),
                rewritten_payload: Vec::new(),
            },
            ApprovalDecision::Deny { reason } => pb::HookDecision {
                kind: pb::hook_decision::Kind::Deny as i32,
                reason,
                rewritten_payload: Vec::new(),
            },
        }
    }
}

/// Tools that trigger the HITL dialog by default. Everything else is
/// auto-allowed (subject to the Python chain).
pub fn requires_approval(tool_name: &str) -> bool {
    matches!(tool_name, "bash")
}

pub struct TuiBackend {
    inner: Arc<dyn QueryBackend>,
    approval_tx: mpsc::UnboundedSender<ApprovalEnvelope>,
    auto_approve: bool,
}

/// What the UI thread receives on the approval channel.
pub struct ApprovalEnvelope {
    pub request: PendingApproval,
    pub responder: oneshot::Sender<ApprovalDecision>,
}

impl TuiBackend {
    pub fn new(
        inner: Arc<dyn QueryBackend>,
        approval_tx: mpsc::UnboundedSender<ApprovalEnvelope>,
        auto_approve: bool,
    ) -> Self {
        Self {
            inner,
            approval_tx,
            auto_approve,
        }
    }
}

#[async_trait]
impl QueryBackend for TuiBackend {
    async fn chat_stream(
        &self,
        req: pb::ChatRequest,
    ) -> Result<BoxStream<'static, Result<pb::ChatDelta, tonic::Status>>> {
        self.inner.chat_stream(req).await
    }

    async fn on_turn_start(&self, ctx: pb::TurnContext) -> Result<pb::HookDecision> {
        self.inner.on_turn_start(ctx).await
    }

    async fn on_tool_call(&self, event: pb::ToolCallEvent) -> Result<pb::HookDecision> {
        // Let Python (bash_guard, logging, user middleware) speak first.
        let python_decision = self.inner.on_tool_call(event.clone()).await?;
        if python_decision.kind != pb::hook_decision::Kind::Allow as i32 {
            return Ok(python_decision);
        }

        let call = match event.call.as_ref() {
            Some(c) => c,
            None => return Ok(python_decision),
        };

        if !requires_approval(&call.name) || self.auto_approve {
            return Ok(python_decision);
        }

        let (resp_tx, resp_rx) = oneshot::channel();
        let envelope = ApprovalEnvelope {
            request: PendingApproval {
                tool_name: call.name.clone(),
                arguments: String::from_utf8_lossy(&call.arguments).into_owned(),
            },
            responder: resp_tx,
        };
        if self.approval_tx.send(envelope).is_err() {
            // UI thread gone; fall back to allowing — we'd rather complete
            // the turn than hang. Should not happen in practice.
            return Ok(python_decision);
        }
        let decision = resp_rx
            .await
            .map_err(|e| anyhow!("approval channel closed: {e}"))?;
        Ok(decision.into_hook_decision())
    }

    async fn on_turn_end(&self, result: pb::TurnResult) -> Result<()> {
        self.inner.on_turn_end(result).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_requires_approval() {
        assert!(requires_approval("bash"));
    }

    #[test]
    fn file_read_does_not_require_approval() {
        assert!(!requires_approval("file_read"));
        assert!(!requires_approval("grep"));
        assert!(!requires_approval("glob"));
    }

    #[test]
    fn approval_decision_to_hook_decision() {
        let allow = ApprovalDecision::Allow.into_hook_decision();
        assert_eq!(allow.kind, pb::hook_decision::Kind::Allow as i32);

        let deny = ApprovalDecision::Deny {
            reason: "nope".to_string(),
        }
        .into_hook_decision();
        assert_eq!(deny.kind, pb::hook_decision::Kind::Deny as i32);
        assert_eq!(deny.reason, "nope");
    }
}
