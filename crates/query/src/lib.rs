//! ash-query — the agent turn loop.
//!
//! # Flow (M3)
//!
//! ```text
//! run_turn(session, user_input) loop {
//!   Harness.OnTurnStart(ctx)  → ALLOW | DENY
//!   backend.chat_stream(ChatRequest)
//!   for delta in stream:
//!     text      → sink.on_text + Harness.OnStreamDelta (fire-and-forget)
//!     tool_call → Harness.OnToolCall → ALLOW | DENY | REWRITE
//!                 → ToolRegistry.invoke → append tool_result → next turn
//!     finish    → break delta loop
//!   Harness.OnTurnEnd(result)
//!   if stop_reason != "tool_use": return
//! }
//! ```

use std::sync::Arc;

use anyhow::{anyhow, Result};
use ash_bus::{BusEvent, SessionBus};
use ash_ipc::pb;
use ash_tools::{ToolRegistry, ToolResult};
use async_trait::async_trait;
use futures::{stream::BoxStream, StreamExt};
use serde::{Deserialize, Serialize};
pub use tokio_util::sync::CancellationToken;

pub const CRATE_NAME: &str = "ash-query";
pub const DEFAULT_MAX_TURNS: usize = 10;

/// Environment variable that overrides [`DEFAULT_MAX_TURNS`].
pub const ENV_MAX_TURNS: &str = "ASH_MAX_TURNS";

/// M8: opt-in toggle for `Harness.OnStreamDelta` per-token call site.
/// Default off — see `docs/harness_onstreamdelta.md`.
pub const ENV_STREAM_DELTA_HOOK: &str = "ASH_HARNESS_STREAM_DELTA";

pub fn configured_max_turns() -> usize {
    std::env::var(ENV_MAX_TURNS)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_TURNS)
}

pub fn stream_delta_hook_enabled() -> bool {
    matches!(
        std::env::var(ENV_STREAM_DELTA_HOOK).as_deref(),
        Ok("on") | Ok("1") | Ok("true") | Ok("yes")
    )
}

// --- Session & messages ----------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub messages: Vec<ChatMessage>,
}

impl Session {
    pub fn new(id: impl Into<String>, provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            provider: provider.into(),
            model: model.into(),
            messages: Vec::new(),
        }
    }

    pub fn push_user(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMessage {
            role: "user".to_string(),
            content: content.into(),
            tool_call_id: String::new(),
        });
    }

    pub fn push_assistant_text(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: content.into(),
            tool_call_id: String::new(),
        });
    }

    pub fn push_tool_result(&mut self, tool_call_id: impl Into<String>, content: impl Into<String>) {
        self.messages.push(ChatMessage {
            role: "tool".to_string(),
            content: content.into(),
            tool_call_id: tool_call_id.into(),
        });
    }

    /// Records the assistant's outgoing tool-use request so providers
    /// that require explicit pairing (Anthropic) can reconstruct it.
    /// Stored as an opaque JSON sentinel inside the `content` field —
    /// providers detect the marker and translate appropriately.
    pub fn push_assistant_tool_use(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        arguments_json: &str,
    ) {
        let id_s = id.into();
        let name_s = name.into();
        let input: serde_json::Value =
            serde_json::from_str(arguments_json).unwrap_or(serde_json::Value::Null);
        let payload = serde_json::json!({
            "__ash_tool_use__": true,
            "id": id_s,
            "name": name_s,
            "input": input,
        });
        self.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: payload.to_string(),
            tool_call_id: id_s,
        });
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub tool_call_id: String,
}

impl From<&ChatMessage> for pb::ChatMessage {
    fn from(m: &ChatMessage) -> Self {
        pb::ChatMessage {
            role: m.role.clone(),
            content: m.content.clone(),
            tool_call_id: m.tool_call_id.clone(),
        }
    }
}

// --- Sink trait (M3: stdout sink; M7: TUI sink) ---------------------------

pub trait TurnSink: Send {
    fn on_text(&mut self, text: &str);
    fn on_tool_call(&mut self, name: &str, args: &str);
    fn on_tool_result(&mut self, name: &str, result: &ToolResult);
    fn on_finish(&mut self, stop_reason: &str, input_tokens: i32, output_tokens: i32);
    fn on_error(&mut self, message: &str);
}

pub struct NullSink;

impl TurnSink for NullSink {
    fn on_text(&mut self, _: &str) {}
    fn on_tool_call(&mut self, _: &str, _: &str) {}
    fn on_tool_result(&mut self, _: &str, _: &ToolResult) {}
    fn on_finish(&mut self, _: &str, _: i32, _: i32) {}
    fn on_error(&mut self, _: &str) {}
}

// --- Backend trait (abstracted for testability) ---------------------------

/// What the query loop needs from "whatever talks to the sidecar".
/// Decoupled from [`ash_ipc::SidecarClient`] so tests can swap in a mock.
#[async_trait]
pub trait QueryBackend: Send + Sync {
    async fn chat_stream(
        &self,
        req: pb::ChatRequest,
    ) -> Result<BoxStream<'static, Result<pb::ChatDelta, tonic::Status>>>;

    async fn on_turn_start(&self, ctx: pb::TurnContext) -> Result<pb::HookDecision>;
    async fn on_tool_call(&self, event: pb::ToolCallEvent) -> Result<pb::HookDecision>;
    async fn on_turn_end(&self, result: pb::TurnResult) -> Result<()>;

    /// Optional fire-and-forget hook called once per streaming chunk
    /// when `ASH_HARNESS_STREAM_DELTA=on`. Default impl is a no-op so
    /// existing backends compile unchanged.
    async fn on_stream_delta(&self, _event: pb::DeltaEvent) -> Result<()> {
        Ok(())
    }
}

/// Blanket impl for the real `SidecarClient`. Keeps the query crate
/// untied to a specific transport.
pub struct SidecarBackend(pub ash_ipc::SidecarClient);

#[async_trait]
impl QueryBackend for SidecarBackend {
    async fn chat_stream(
        &self,
        req: pb::ChatRequest,
    ) -> Result<BoxStream<'static, Result<pb::ChatDelta, tonic::Status>>> {
        let streaming = self.0.chat_stream(req).await?;
        Ok(Box::pin(streaming))
    }

    async fn on_turn_start(&self, ctx: pb::TurnContext) -> Result<pb::HookDecision> {
        self.0.on_turn_start(ctx).await
    }

    async fn on_tool_call(&self, event: pb::ToolCallEvent) -> Result<pb::HookDecision> {
        self.0.on_tool_call(event).await
    }

    async fn on_turn_end(&self, result: pb::TurnResult) -> Result<()> {
        self.0.on_turn_end(result).await
    }

    async fn on_stream_delta(&self, event: pb::DeltaEvent) -> Result<()> {
        self.0.on_stream_delta(event).await
    }
}

// --- Engine ----------------------------------------------------------------

pub struct QueryEngine {
    backend: Arc<dyn QueryBackend>,
    tools: Arc<ToolRegistry>,
    bus: Arc<SessionBus>,
    max_turns: usize,
    stream_delta_hook: bool,
}

pub struct TurnOutcome {
    pub stop_reason: String,
    pub turns_taken: usize,
    pub denied: bool,
    pub denial_reason: String,
}

impl QueryEngine {
    pub fn new(backend: Arc<dyn QueryBackend>, tools: Arc<ToolRegistry>) -> Self {
        Self {
            backend,
            tools,
            bus: Arc::new(SessionBus::new()),
            max_turns: configured_max_turns(),
            stream_delta_hook: stream_delta_hook_enabled(),
        }
    }

    pub fn with_max_turns(mut self, n: usize) -> Self {
        self.max_turns = n;
        self
    }

    pub fn with_bus(mut self, bus: Arc<SessionBus>) -> Self {
        self.bus = bus;
        self
    }

    pub fn bus(&self) -> Arc<SessionBus> {
        self.bus.clone()
    }

    pub async fn run_turn(
        &self,
        session: &mut Session,
        sink: &mut dyn TurnSink,
        cancel: CancellationToken,
    ) -> Result<TurnOutcome> {
        let mut turns_taken = 0;
        let mut stop_reason = String::from("end_turn");

        loop {
            if cancel.is_cancelled() {
                stop_reason = "cancelled".to_string();
                sink.on_error("turn cancelled by user");
                self.bus.publish(
                    &session.id,
                    BusEvent::Cancelled {
                        reason: "cancelled before turn started".to_string(),
                    },
                );
                break;
            }
            if turns_taken >= self.max_turns {
                stop_reason = "max_turns".to_string();
                break;
            }
            turns_taken += 1;

            // --- OnTurnStart hook
            let turn_id = format!("{}-{}", session.id, turns_taken);
            let ctx = pb::TurnContext {
                session_id: session.id.clone(),
                turn_id: turn_id.clone(),
                provider: session.provider.clone(),
                model: session.model.clone(),
                messages: session.messages.iter().map(Into::into).collect(),
                metadata: Default::default(),
            };
            let decision = self.backend.on_turn_start(ctx).await?;
            if decision.kind == pb::hook_decision::Kind::Deny as i32 {
                sink.on_error(&format!("turn denied by harness: {}", decision.reason));
                return Ok(TurnOutcome {
                    stop_reason: "denied".to_string(),
                    turns_taken,
                    denied: true,
                    denial_reason: decision.reason,
                });
            }

            // --- build ChatRequest (tools from registry)
            let tool_specs = self.tool_specs_for_request();
            let req = pb::ChatRequest {
                provider: session.provider.clone(),
                model: session.model.clone(),
                messages: session.messages.iter().map(Into::into).collect(),
                temperature: 0.2,
                tools: tool_specs,
            };

            // --- stream consumption (with mid-turn cancellation)
            let mut stream = self.backend.chat_stream(req).await?;
            let mut assistant_text = String::new();
            let mut tool_call: Option<pb::ToolCall> = None;
            let mut finish: Option<pb::TurnFinish> = None;
            let mut cancelled_mid_stream = false;

            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        cancelled_mid_stream = true;
                        // Drop the stream — tonic propagates cancellation
                        // back through the gRPC channel.
                        drop(stream);
                        break;
                    }
                    next = stream.next() => {
                        let Some(delta) = next else { break; };
                        let delta = delta.map_err(|s| anyhow!("chat stream error: {s}"))?;
                        match delta.kind {
                            Some(pb::chat_delta::Kind::Text(t)) => {
                                assistant_text.push_str(&t);
                                sink.on_text(&t);
                                self.bus.publish(
                                    &session.id,
                                    BusEvent::AssistantText { text: t.clone() },
                                );
                                self.maybe_call_stream_delta_hook(
                                    &session.id,
                                    &turn_id,
                                    pb::ChatDelta {
                                        kind: Some(pb::chat_delta::Kind::Text(t)),
                                    },
                                );
                            }
                            Some(pb::chat_delta::Kind::ToolCall(tc)) => {
                                sink.on_tool_call(&tc.name, &String::from_utf8_lossy(&tc.arguments));
                                self.bus.publish(
                                    &session.id,
                                    BusEvent::ToolCall {
                                        id: tc.id.clone(),
                                        name: tc.name.clone(),
                                        args: String::from_utf8_lossy(&tc.arguments).into_owned(),
                                    },
                                );
                                tool_call = Some(tc);
                            }
                            Some(pb::chat_delta::Kind::Finish(f)) => {
                                finish = Some(f);
                            }
                            None => {}
                        }
                    }
                }
            }

            if cancelled_mid_stream {
                stop_reason = "cancelled".to_string();
                sink.on_error("turn cancelled by user");
                if !assistant_text.is_empty() {
                    let mut marked = assistant_text.clone();
                    marked.push_str("\n\n[cancelled by user]");
                    session.push_assistant_text(&marked);
                }
                self.bus.publish(
                    &session.id,
                    BusEvent::Cancelled {
                        reason: format!("cancelled mid-stream after {} chars", assistant_text.len()),
                    },
                );
                self.bus.publish(
                    &session.id,
                    BusEvent::Outcome {
                        stop_reason: stop_reason.clone(),
                        turns_taken,
                        denied: false,
                    },
                );
                return Ok(TurnOutcome {
                    stop_reason,
                    turns_taken,
                    denied: false,
                    denial_reason: String::new(),
                });
            }

            // --- record assistant turn
            if !assistant_text.is_empty() {
                session.push_assistant_text(&assistant_text);
            }
            let finish = finish.unwrap_or(pb::TurnFinish {
                stop_reason: "end_turn".to_string(),
                input_tokens: 0,
                output_tokens: 0,
            });
            stop_reason = finish.stop_reason.clone();
            sink.on_finish(
                &finish.stop_reason,
                finish.input_tokens,
                finish.output_tokens,
            );

            // --- OnTurnEnd hook (fire-and-forget semantics: log but ignore errors)
            let tr = pb::TurnResult {
                session_id: session.id.clone(),
                turn_id: turn_id.clone(),
                finish: Some(finish.clone()),
                assistant_text: assistant_text.clone(),
            };
            if let Err(err) = self.backend.on_turn_end(tr).await {
                tracing::warn!("on_turn_end failed: {err:#}");
            }

            // --- if a tool was requested, run it and loop
            if let Some(tc) = tool_call {
                let event = pb::ToolCallEvent {
                    session_id: session.id.clone(),
                    turn_id: turn_id.clone(),
                    call: Some(tc.clone()),
                };
                let tool_decision = self.backend.on_tool_call(event).await?;
                match tool_decision.kind {
                    k if k == pb::hook_decision::Kind::Deny as i32 => {
                        let msg = format!(
                            "tool call '{}' denied by harness: {}",
                            tc.name, tool_decision.reason
                        );
                        sink.on_error(&msg);
                        session.push_tool_result(&tc.id, &msg);
                        stop_reason = "tool_denied".to_string();
                        continue;
                    }
                    _ => {}
                }

                let args_value: serde_json::Value = serde_json::from_slice(&tc.arguments)
                    .unwrap_or(serde_json::Value::Null);
                // Persist the assistant-side tool_use record so the next
                // turn's request to the provider can reconstruct the
                // pairing. Anthropic strictly requires this.
                let args_str = String::from_utf8_lossy(&tc.arguments).into_owned();
                session.push_assistant_tool_use(&tc.id, &tc.name, &args_str);

                let result = match self.tools.invoke(&tc.name, args_value).await {
                    Ok(r) => r,
                    Err(err) => ToolResult::err_text(format!("tool {} failed: {err}", tc.name)),
                };
                sink.on_tool_result(&tc.name, &result);
                self.bus.publish(
                    &session.id,
                    BusEvent::ToolResult {
                        name: tc.name.clone(),
                        ok: result.ok,
                        body: if result.ok {
                            result.stdout.clone()
                        } else {
                            result.stderr.clone()
                        },
                    },
                );
                let payload = serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_string());
                session.push_tool_result(&tc.id, payload);
                continue;
            }

            // No tool call → we're done.
            break;
        }

        let outcome = TurnOutcome {
            stop_reason: stop_reason.clone(),
            turns_taken,
            denied: false,
            denial_reason: String::new(),
        };
        self.bus.publish(
            &session.id,
            BusEvent::Outcome {
                stop_reason,
                turns_taken,
                denied: false,
            },
        );
        Ok(outcome)
    }

    fn tool_specs_for_request(&self) -> Vec<pb::ToolSpec> {
        self.tools
            .list()
            .into_iter()
            .map(|spec| pb::ToolSpec {
                name: spec.name,
                description: spec.description,
                input_schema: serde_json::to_vec(&spec.input_schema).unwrap_or_default(),
            })
            .collect()
    }

    fn maybe_call_stream_delta_hook(
        &self,
        session_id: &str,
        turn_id: &str,
        delta: pb::ChatDelta,
    ) {
        if !self.stream_delta_hook {
            return;
        }
        let backend = self.backend.clone();
        let event = pb::DeltaEvent {
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            delta: Some(delta),
        };
        // Fire-and-forget: any per-token gRPC stall must NOT block the
        // streaming response. Errors logged at warn! and discarded.
        tokio::spawn(async move {
            if let Err(err) = backend.on_stream_delta(event).await {
                tracing::warn!("on_stream_delta failed: {err:#}");
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream;
    use std::sync::Mutex;

    /// A scripted backend: returns a queue of pre-baked chat streams and
    /// always allows harness hooks.
    #[derive(Default)]
    struct ScriptedBackend {
        turns: Mutex<Vec<Vec<pb::ChatDelta>>>,
        tool_decision: Mutex<Option<pb::HookDecision>>,
    }

    impl ScriptedBackend {
        fn push_turn(&self, deltas: Vec<pb::ChatDelta>) {
            self.turns.lock().unwrap().push(deltas);
        }

        fn deny_tools(&self, reason: &str) {
            *self.tool_decision.lock().unwrap() = Some(pb::HookDecision {
                kind: pb::hook_decision::Kind::Deny as i32,
                reason: reason.to_string(),
                rewritten_payload: Vec::new(),
            });
        }
    }

    #[async_trait]
    impl QueryBackend for ScriptedBackend {
        async fn chat_stream(
            &self,
            _req: pb::ChatRequest,
        ) -> Result<BoxStream<'static, Result<pb::ChatDelta, tonic::Status>>> {
            let mut queue = self.turns.lock().unwrap();
            if queue.is_empty() {
                return Ok(Box::pin(stream::empty()));
            }
            let deltas = queue.remove(0);
            let iter = deltas.into_iter().map(Ok::<_, tonic::Status>);
            Ok(Box::pin(stream::iter(iter.collect::<Vec<_>>())))
        }

        async fn on_turn_start(&self, _ctx: pb::TurnContext) -> Result<pb::HookDecision> {
            Ok(pb::HookDecision {
                kind: pb::hook_decision::Kind::Allow as i32,
                reason: String::new(),
                rewritten_payload: Vec::new(),
            })
        }

        async fn on_tool_call(&self, _event: pb::ToolCallEvent) -> Result<pb::HookDecision> {
            if let Some(d) = self.tool_decision.lock().unwrap().clone() {
                return Ok(d);
            }
            Ok(pb::HookDecision {
                kind: pb::hook_decision::Kind::Allow as i32,
                reason: String::new(),
                rewritten_payload: Vec::new(),
            })
        }

        async fn on_turn_end(&self, _result: pb::TurnResult) -> Result<()> {
            Ok(())
        }
    }

    fn text_delta(s: &str) -> pb::ChatDelta {
        pb::ChatDelta {
            kind: Some(pb::chat_delta::Kind::Text(s.to_string())),
        }
    }

    fn finish_delta(stop: &str) -> pb::ChatDelta {
        pb::ChatDelta {
            kind: Some(pb::chat_delta::Kind::Finish(pb::TurnFinish {
                stop_reason: stop.to_string(),
                input_tokens: 1,
                output_tokens: 2,
            })),
        }
    }

    fn tool_call_delta(name: &str, args: serde_json::Value) -> pb::ChatDelta {
        pb::ChatDelta {
            kind: Some(pb::chat_delta::Kind::ToolCall(pb::ToolCall {
                id: "tc-1".to_string(),
                name: name.to_string(),
                arguments: serde_json::to_vec(&args).unwrap(),
            })),
        }
    }

    #[tokio::test]
    async fn single_turn_text_only() {
        let backend = Arc::new(ScriptedBackend::default());
        backend.push_turn(vec![text_delta("hello "), text_delta("world"), finish_delta("end_turn")]);

        let engine = QueryEngine::new(backend, Arc::new(ToolRegistry::with_builtins()));
        let mut session = Session::new("s1", "fake", "fake-1");
        session.push_user("hi");

        #[derive(Default)]
        struct Collect {
            text: String,
            finish: String,
        }
        impl TurnSink for Collect {
            fn on_text(&mut self, t: &str) {
                self.text.push_str(t);
            }
            fn on_tool_call(&mut self, _: &str, _: &str) {}
            fn on_tool_result(&mut self, _: &str, _: &ToolResult) {}
            fn on_finish(&mut self, sr: &str, _: i32, _: i32) {
                self.finish = sr.to_string();
            }
            fn on_error(&mut self, _: &str) {}
        }

        let mut sink = Collect::default();
        let out = engine
            .run_turn(&mut session, &mut sink, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(sink.text, "hello world");
        assert_eq!(sink.finish, "end_turn");
        assert_eq!(out.turns_taken, 1);
        assert_eq!(out.stop_reason, "end_turn");
    }

    #[tokio::test]
    async fn tool_call_roundtrip() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("hello.txt");

        let backend = Arc::new(ScriptedBackend::default());
        // Turn 1: model requests file_write
        backend.push_turn(vec![
            tool_call_delta(
                "file_write",
                serde_json::json!({
                    "path": target.to_string_lossy(),
                    "content": "ash-code was here"
                }),
            ),
            finish_delta("tool_use"),
        ]);
        // Turn 2: model produces final text
        backend.push_turn(vec![text_delta("done"), finish_delta("end_turn")]);

        let engine = QueryEngine::new(backend, Arc::new(ToolRegistry::with_builtins()));
        let mut session = Session::new("s1", "fake", "fake-1");
        session.push_user("write the file");

        engine
            .run_turn(&mut session, &mut NullSink, CancellationToken::new())
            .await
            .unwrap();

        let written = tokio::fs::read_to_string(&target).await.unwrap();
        assert_eq!(written, "ash-code was here");
        // Session contains: user → assistant(tool_use sentinel) → tool result → assistant text
        let roles: Vec<_> = session.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["user", "assistant", "tool", "assistant"]);
        // First assistant message should carry the tool_use sentinel.
        assert!(session.messages[1].content.contains("__ash_tool_use__"));
        assert_eq!(session.messages[1].content.contains("file_write"), true);
    }

    #[tokio::test]
    async fn tool_denied_by_harness() {
        let backend = Arc::new(ScriptedBackend::default());
        backend.deny_tools("blocked by test middleware");
        backend.push_turn(vec![
            tool_call_delta("bash", serde_json::json!({"command": "echo hi"})),
            finish_delta("tool_use"),
        ]);

        let engine = QueryEngine::new(backend, Arc::new(ToolRegistry::with_builtins()))
            .with_max_turns(3);
        let mut session = Session::new("s1", "fake", "fake-1");
        session.push_user("run a command");

        engine
            .run_turn(&mut session, &mut NullSink, CancellationToken::new())
            .await
            .unwrap();
        // Tool result message should carry the denial text — tool was blocked
        // and the engine continues into the next (empty) turn where it
        // naturally terminates.
        let has_denial = session
            .messages
            .iter()
            .any(|m| m.role == "tool" && m.content.contains("denied"));
        assert!(has_denial, "expected a denied tool message in the session");
    }

    #[tokio::test]
    async fn max_turns_guard() {
        let backend = Arc::new(ScriptedBackend::default());
        // Infinite tool-use loop — engine should stop at max_turns.
        for _ in 0..20 {
            backend.push_turn(vec![
                tool_call_delta("glob", serde_json::json!({"pattern": "*"})),
                finish_delta("tool_use"),
            ]);
        }
        let engine = QueryEngine::new(backend, Arc::new(ToolRegistry::with_builtins()))
            .with_max_turns(3);
        let mut session = Session::new("s1", "fake", "fake-1");
        session.push_user("loop forever");
        let out = engine
            .run_turn(&mut session, &mut NullSink, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.turns_taken, 3);
        assert_eq!(out.stop_reason, "max_turns");
    }

    // --- M8: cancellation tests ------------------------------------------

    #[tokio::test]
    async fn cancel_before_turn_returns_cancelled() {
        let backend = Arc::new(ScriptedBackend::default());
        backend.push_turn(vec![text_delta("never sent"), finish_delta("end_turn")]);
        let engine = QueryEngine::new(backend, Arc::new(ToolRegistry::with_builtins()));
        let mut session = Session::new("s1", "fake", "fake-1");
        session.push_user("hi");

        let cancel = CancellationToken::new();
        cancel.cancel();
        let out = engine.run_turn(&mut session, &mut NullSink, cancel).await.unwrap();
        assert_eq!(out.stop_reason, "cancelled");
        assert_eq!(out.turns_taken, 0);
    }

    #[tokio::test]
    async fn cancel_during_stream_preserves_partial_text() {
        // Backend that yields three text deltas with a small async pause
        // between each so the cancellation has a moment to fire.
        struct SlowBackend;
        #[async_trait]
        impl QueryBackend for SlowBackend {
            async fn chat_stream(
                &self,
                _req: pb::ChatRequest,
            ) -> Result<BoxStream<'static, Result<pb::ChatDelta, tonic::Status>>> {
                use async_stream::stream as build_stream;
                let s = build_stream! {
                    for chunk in ["hel", "lo ", "wor", "ld"] {
                        yield Ok(pb::ChatDelta {
                            kind: Some(pb::chat_delta::Kind::Text(chunk.to_string())),
                        });
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                };
                Ok(Box::pin(s))
            }
            async fn on_turn_start(&self, _ctx: pb::TurnContext) -> Result<pb::HookDecision> {
                Ok(pb::HookDecision { kind: pb::hook_decision::Kind::Allow as i32, ..Default::default() })
            }
            async fn on_tool_call(&self, _e: pb::ToolCallEvent) -> Result<pb::HookDecision> {
                Ok(pb::HookDecision { kind: pb::hook_decision::Kind::Allow as i32, ..Default::default() })
            }
            async fn on_turn_end(&self, _r: pb::TurnResult) -> Result<()> { Ok(()) }
        }

        let engine = QueryEngine::new(Arc::new(SlowBackend), Arc::new(ToolRegistry::with_builtins()));
        let mut session = Session::new("s2", "fake", "fake-1");
        session.push_user("write a long story");
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            cancel_clone.cancel();
        });
        let out = engine.run_turn(&mut session, &mut NullSink, cancel).await.unwrap();
        assert_eq!(out.stop_reason, "cancelled");
        // Some partial text should be persisted with the cancelled marker.
        let last = session.messages.last().unwrap();
        assert_eq!(last.role, "assistant");
        assert!(last.content.contains("[cancelled by user]"));
    }

    #[tokio::test]
    async fn bus_publishes_outcome_event() {
        let backend = Arc::new(ScriptedBackend::default());
        backend.push_turn(vec![text_delta("ok"), finish_delta("end_turn")]);
        let engine = QueryEngine::new(backend, Arc::new(ToolRegistry::with_builtins()));
        let bus = engine.bus();
        let mut rx = bus.subscribe("bus-test");
        let mut session = Session::new("bus-test", "fake", "fake-1");
        session.push_user("hi");
        let _ = engine.run_turn(&mut session, &mut NullSink, CancellationToken::new()).await.unwrap();

        // Drain events until we hit the Outcome marker.
        let mut saw_outcome = false;
        while let Ok(event) = rx.try_recv() {
            if let BusEvent::Outcome { stop_reason, .. } = event {
                assert_eq!(stop_reason, "end_turn");
                saw_outcome = true;
            }
        }
        assert!(saw_outcome, "expected an Outcome event on the bus");
    }
}
