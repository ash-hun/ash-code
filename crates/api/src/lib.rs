//! ash-api — Rust-side `QueryHost` gRPC server.
//!
//! This is the "turn engine host" that the Python FastAPI layer calls
//! from its `/v1/chat` handler. See `docs/comparison_api_structure.md`
//! for the full architecture story.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use ash_core::storage::{
    self, MemoryStore, SessionRecord, SessionStore, StoredMessage,
};
use ash_bus::BusEvent;
use ash_ipc::{pb, SidecarClient};
use ash_query::{
    CancellationToken, ChatMessage, QueryBackend, QueryEngine, Session, SidecarBackend, TurnSink,
};
use ash_tools::{ToolRegistry, ToolResult};
use async_trait::async_trait;
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tonic::{transport::Server, Request, Response, Status};

pub const DEFAULT_PORT: u16 = 8080;
pub const DEFAULT_QUERY_HOST_PORT: u16 = 50052;

// ---------------------------------------------------------------------------
// ChannelSink — converts TurnSink callbacks into gRPC TurnDelta messages
// ---------------------------------------------------------------------------

struct ChannelSink {
    tx: mpsc::UnboundedSender<Result<pb::TurnDelta, Status>>,
}

impl ChannelSink {
    fn new(tx: mpsc::UnboundedSender<Result<pb::TurnDelta, Status>>) -> Self {
        Self { tx }
    }

    fn send(&self, delta: pb::TurnDelta) {
        let _ = self.tx.send(Ok(delta));
    }
}

impl TurnSink for ChannelSink {
    fn on_text(&mut self, text: &str) {
        self.send(pb::TurnDelta {
            kind: Some(pb::turn_delta::Kind::Text(text.to_string())),
        });
    }

    fn on_tool_call(&mut self, name: &str, args: &str) {
        self.send(pb::TurnDelta {
            kind: Some(pb::turn_delta::Kind::ToolCall(pb::ToolCall {
                id: String::new(),
                name: name.to_string(),
                arguments: args.as_bytes().to_vec(),
            })),
        });
    }

    fn on_tool_result(&mut self, name: &str, result: &ToolResult) {
        self.send(pb::TurnDelta {
            kind: Some(pb::turn_delta::Kind::ToolResult(pb::ToolResultDelta {
                name: name.to_string(),
                ok: result.ok,
                stdout: result.stdout.clone(),
                stderr: result.stderr.clone(),
                exit_code: result.exit_code,
            })),
        });
    }

    fn on_finish(&mut self, stop_reason: &str, input_tokens: i32, output_tokens: i32) {
        self.send(pb::TurnDelta {
            kind: Some(pb::turn_delta::Kind::Finish(pb::TurnFinish {
                stop_reason: stop_reason.to_string(),
                input_tokens,
                output_tokens,
            })),
        });
    }

    fn on_error(&mut self, message: &str) {
        self.send(pb::TurnDelta {
            kind: Some(pb::turn_delta::Kind::Error(message.to_string())),
        });
    }
}

// ---------------------------------------------------------------------------
// BusEvent → WatchEvent conversion
// ---------------------------------------------------------------------------

fn bus_event_to_watch(session_id: &str, event: &BusEvent) -> pb::WatchEvent {
    let (event_type, payload) = match event {
        BusEvent::UserMessage { text } => (
            "user_message",
            serde_json::json!({ "text": text }),
        ),
        BusEvent::AssistantText { text } => (
            "assistant_text",
            serde_json::json!({ "text": text }),
        ),
        BusEvent::ToolCall { id, name, args } => (
            "tool_call",
            serde_json::json!({ "id": id, "name": name, "arguments": args }),
        ),
        BusEvent::ToolResult { name, ok, body } => (
            "tool_result",
            serde_json::json!({ "name": name, "ok": ok, "body": body }),
        ),
        BusEvent::TurnFinish { stop_reason, in_tok, out_tok } => (
            "turn_finish",
            serde_json::json!({
                "stop_reason": stop_reason,
                "input_tokens": in_tok,
                "output_tokens": out_tok,
            }),
        ),
        BusEvent::TurnError { message } => (
            "turn_error",
            serde_json::json!({ "message": message }),
        ),
        BusEvent::Cancelled { reason } => (
            "cancelled",
            serde_json::json!({ "reason": reason }),
        ),
        BusEvent::Outcome { stop_reason, turns_taken, denied } => (
            "outcome",
            serde_json::json!({
                "stop_reason": stop_reason,
                "turns_taken": turns_taken,
                "denied": denied,
            }),
        ),
    };
    pb::WatchEvent {
        event_type: event_type.to_string(),
        session_id: session_id.to_string(),
        payload: serde_json::to_vec(&payload).unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Service implementation
// ---------------------------------------------------------------------------

pub struct QueryHostService {
    engine: Arc<QueryEngine>,
    store: Arc<dyn SessionStore>,
    default_provider: String,
    default_model: String,
    /// Active session → CancellationToken for the in-flight turn.
    active_tokens: Arc<RwLock<HashMap<String, CancellationToken>>>,
}

impl QueryHostService {
    pub fn new(
        engine: Arc<QueryEngine>,
        store: Arc<dyn SessionStore>,
        default_provider: String,
        default_model: String,
    ) -> Self {
        Self {
            engine,
            store,
            default_provider,
            default_model,
            active_tokens: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Convenience constructor for tests / scripts that just want an
    /// in-memory backing store.
    pub fn new_in_memory(
        engine: Arc<QueryEngine>,
        default_provider: String,
        default_model: String,
    ) -> Self {
        Self::new(
            engine,
            Arc::new(MemoryStore::new()),
            default_provider,
            default_model,
        )
    }
}

// --- Session ↔ SessionRecord conversion -----------------------------------

fn session_to_record(session: &Session, created_at_ms: i64) -> SessionRecord {
    SessionRecord {
        id: session.id.clone(),
        provider: session.provider.clone(),
        model: session.model.clone(),
        created_at_ms,
        updated_at_ms: SessionRecord::now_ms(),
        messages: session
            .messages
            .iter()
            .map(|m| StoredMessage {
                role: m.role.clone(),
                content: m.content.clone(),
                tool_call_id: m.tool_call_id.clone(),
            })
            .collect(),
    }
}

fn record_to_session(record: &SessionRecord) -> Session {
    let mut s = Session::new(&record.id, &record.provider, &record.model);
    s.messages = record
        .messages
        .iter()
        .map(|m| ChatMessage {
            role: m.role.clone(),
            content: m.content.clone(),
            tool_call_id: m.tool_call_id.clone(),
        })
        .collect();
    s
}

#[async_trait]
impl pb::query_host_server::QueryHost for QueryHostService {
    type RunTurnStream = UnboundedReceiverStream<Result<pb::TurnDelta, Status>>;
    type WatchSessionStream = UnboundedReceiverStream<Result<pb::WatchEvent, Status>>;

    async fn run_turn(
        &self,
        request: Request<pb::RunTurnRequest>,
    ) -> Result<Response<Self::RunTurnStream>, Status> {
        let req = request.into_inner();

        let session_id = if req.session_id.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            req.session_id.clone()
        };

        let provider = if req.provider.is_empty() {
            self.default_provider.clone()
        } else {
            req.provider.clone()
        };
        let model = if req.model.is_empty() {
            self.default_model.clone()
        } else {
            req.model.clone()
        };

        // Fetch or create the session from the store.
        let existing = self
            .store
            .get(&session_id)
            .await
            .map_err(|e| Status::internal(format!("session store get: {e}")))?;
        let (mut session, created_at_ms) = match (existing, req.reset_session) {
            (Some(rec), false) => {
                let created = rec.created_at_ms;
                (record_to_session(&rec), created)
            }
            _ => (
                Session::new(&session_id, &provider, &model),
                SessionRecord::now_ms(),
            ),
        };
        session.provider = provider;
        session.model = model;
        session.push_user(req.prompt);

        let (tx, rx) = mpsc::unbounded_channel();
        let stream = UnboundedReceiverStream::new(rx);

        let engine = self.engine.clone();
        let store = self.store.clone();
        let tokens = self.active_tokens.clone();

        // Create the cancellation token and register it.
        // If there is already an active turn for this session, cancel it first.
        let cancel = CancellationToken::new();
        {
            let mut map = tokens.write().await;
            if let Some(prev) = map.insert(session_id.clone(), cancel.clone()) {
                prev.cancel();
            }
        }

        let sid = session_id.clone();
        tokio::spawn(async move {
            let mut sink = ChannelSink::new(tx.clone());
            let outcome = match engine.run_turn(&mut session, &mut sink, cancel.clone()).await {
                Ok(o) => o,
                Err(err) => {
                    let _ = tx.send(Ok(pb::TurnDelta {
                        kind: Some(pb::turn_delta::Kind::Error(format!(
                            "engine error: {err:#}"
                        ))),
                    }));
                    let _ = tx.send(Ok(pb::TurnDelta {
                        kind: Some(pb::turn_delta::Kind::Outcome(pb::TurnOutcome {
                            stop_reason: "error".to_string(),
                            turns_taken: 0,
                            denied: false,
                            denial_reason: String::new(),
                        })),
                    }));
                    // Clean up token on error path.
                    tokens.write().await.remove(&sid);
                    return;
                }
            };

            // Remove the token from the active map (only if it's still ours).
            {
                let mut map = tokens.write().await;
                if let Some(existing) = map.get(&sid) {
                    if existing.is_cancelled() == cancel.is_cancelled() {
                        map.remove(&sid);
                    }
                }
            }

            // Persist the (now mutated) session back into the store.
            let record = session_to_record(&session, created_at_ms);
            if let Err(err) = store.put(&record).await {
                tracing::warn!("session store put failed: {err:#}");
            }

            let _ = tx.send(Ok(pb::TurnDelta {
                kind: Some(pb::turn_delta::Kind::Outcome(pb::TurnOutcome {
                    stop_reason: outcome.stop_reason,
                    turns_taken: outcome.turns_taken as i32,
                    denied: outcome.denied,
                    denial_reason: outcome.denial_reason,
                })),
            }));
        });

        Ok(Response::new(stream))
    }

    async fn cancel_turn(
        &self,
        request: Request<pb::CancelTurnRequest>,
    ) -> Result<Response<pb::CancelTurnResponse>, Status> {
        let session_id = request.into_inner().session_id;
        let map = self.active_tokens.read().await;
        if let Some(token) = map.get(&session_id) {
            token.cancel();
            Ok(Response::new(pb::CancelTurnResponse {
                ok: true,
                message: "cancelled".to_string(),
            }))
        } else {
            Ok(Response::new(pb::CancelTurnResponse {
                ok: false,
                message: "no active turn".to_string(),
            }))
        }
    }

    async fn watch_session(
        &self,
        request: Request<pb::WatchSessionRequest>,
    ) -> Result<Response<Self::WatchSessionStream>, Status> {
        let session_id = request.into_inner().session_id;
        let mut rx = self.engine.bus().subscribe(&session_id);
        let (tx, out_rx) = mpsc::unbounded_channel();
        let sid = session_id.clone();

        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let watch = bus_event_to_watch(&sid, &event);
                        if tx.send(Ok(watch)).is_err() {
                            break; // client disconnected
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("watch subscriber lagged by {n} events");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break; // session channel closed
                    }
                }
            }
        });

        Ok(Response::new(UnboundedReceiverStream::new(out_rx)))
    }

    async fn list_sessions(
        &self,
        _request: Request<pb::ListSessionsRequest>,
    ) -> Result<Response<pb::ListSessionsResponse>, Status> {
        let summaries = self
            .store
            .list()
            .await
            .map_err(|e| Status::internal(format!("list: {e}")))?;
        let sessions = summaries
            .into_iter()
            .map(|s| pb::SessionSummary {
                id: s.id,
                provider: s.provider,
                model: s.model,
                message_count: s.message_count,
            })
            .collect();
        Ok(Response::new(pb::ListSessionsResponse { sessions }))
    }

    async fn get_session(
        &self,
        request: Request<pb::GetSessionRequest>,
    ) -> Result<Response<pb::GetSessionResponse>, Status> {
        let id = request.into_inner().id;
        let record = self
            .store
            .get(&id)
            .await
            .map_err(|e| Status::internal(format!("get: {e}")))?
            .ok_or_else(|| Status::not_found(format!("session not found: {id}")))?;
        let summary = pb::SessionSummary {
            id: record.id.clone(),
            provider: record.provider.clone(),
            model: record.model.clone(),
            message_count: record.messages.len() as i32,
        };
        let messages = record
            .messages
            .iter()
            .map(|m| pb::ChatMessage {
                role: m.role.clone(),
                content: m.content.clone(),
                tool_call_id: m.tool_call_id.clone(),
            })
            .collect();
        Ok(Response::new(pb::GetSessionResponse {
            summary: Some(summary),
            messages,
        }))
    }

    async fn delete_session(
        &self,
        request: Request<pb::DeleteSessionRequest>,
    ) -> Result<Response<pb::DeleteSessionResponse>, Status> {
        let id = request.into_inner().id;
        let existed = self
            .store
            .delete(&id)
            .await
            .map_err(|e| Status::internal(format!("delete: {e}")))?;
        Ok(Response::new(pb::DeleteSessionResponse { ok: existed }))
    }
}

// ---------------------------------------------------------------------------
// Entry point used by `ash serve`
// ---------------------------------------------------------------------------

pub async fn serve(
    host: String,
    port: u16,
    sidecar_endpoint: String,
    default_provider: String,
    default_model: String,
) -> Result<()> {
    // Wait for the Python sidecar to be reachable.
    let client = connect_with_retry(&sidecar_endpoint, 10, Duration::from_millis(300)).await?;
    let backend: Arc<dyn QueryBackend> = Arc::new(SidecarBackend(client));
    let tools = Arc::new(ToolRegistry::with_builtins());
    let engine = Arc::new(QueryEngine::new(backend, tools));

    // Build the session store from `ASH_SESSION_STORE` (default: postgres).
    // M9.1: postgres backend is dev-default via the compose `ash-postgres`
    // service, but the URL can point at any external database.
    let store = storage::build_default()
        .await
        .map_err(|e| anyhow::anyhow!("session store init failed: {e:#}"))?;
    println!("[ash] session store ready");

    let service = QueryHostService::new(engine, store, default_provider, default_model);
    let addr: std::net::SocketAddr = format!("{host}:{port}").parse()?;
    tracing::info!("ash QueryHost gRPC listening on {addr}");
    println!("[ash] QueryHost gRPC listening on {addr}");

    Server::builder()
        .add_service(pb::query_host_server::QueryHostServer::new(service))
        .serve(addr)
        .await?;
    Ok(())
}

async fn connect_with_retry(
    endpoint: &str,
    attempts: usize,
    delay: Duration,
) -> Result<SidecarClient> {
    let mut last_err: Option<anyhow::Error> = None;
    for i in 0..attempts {
        match SidecarClient::connect(endpoint.to_string(), Duration::from_secs(2)).await {
            Ok(c) => return Ok(c),
            Err(err) => {
                tracing::warn!("sidecar connect attempt {}/{} failed: {err:#}", i + 1, attempts);
                last_err = Some(err);
                tokio::time::sleep(delay).await;
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("sidecar unreachable")))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ash_ipc::pb::query_host_server::QueryHost as _;
    use futures::{stream, StreamExt};
    use std::sync::Mutex;
    use tonic::Status;

    // Minimal mock QueryBackend reused from the M3 test pattern.
    #[derive(Default)]
    struct MockBackend {
        turns: Mutex<Vec<Vec<pb::ChatDelta>>>,
    }

    impl MockBackend {
        fn push_turn(&self, deltas: Vec<pb::ChatDelta>) {
            self.turns.lock().unwrap().push(deltas);
        }
    }

    #[async_trait]
    impl QueryBackend for MockBackend {
        async fn chat_stream(
            &self,
            _req: pb::ChatRequest,
        ) -> Result<futures::stream::BoxStream<'static, Result<pb::ChatDelta, Status>>> {
            let mut queue = self.turns.lock().unwrap();
            let deltas = if queue.is_empty() {
                Vec::new()
            } else {
                queue.remove(0)
            };
            Ok(Box::pin(stream::iter(
                deltas.into_iter().map(Ok::<_, Status>).collect::<Vec<_>>(),
            )))
        }

        async fn on_turn_start(&self, _ctx: pb::TurnContext) -> Result<pb::HookDecision> {
            Ok(pb::HookDecision {
                kind: pb::hook_decision::Kind::Allow as i32,
                reason: String::new(),
                rewritten_payload: Vec::new(),
            })
        }

        async fn on_tool_call(&self, _event: pb::ToolCallEvent) -> Result<pb::HookDecision> {
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

    fn finish_delta() -> pb::ChatDelta {
        pb::ChatDelta {
            kind: Some(pb::chat_delta::Kind::Finish(pb::TurnFinish {
                stop_reason: "end_turn".to_string(),
                input_tokens: 1,
                output_tokens: 5,
            })),
        }
    }

    fn build_service(backend: Arc<MockBackend>) -> QueryHostService {
        let backend: Arc<dyn QueryBackend> = backend;
        let engine = Arc::new(QueryEngine::new(
            backend,
            Arc::new(ToolRegistry::with_builtins()),
        ));
        QueryHostService::new_in_memory(engine, "mock".to_string(), "mock-1".to_string())
    }

    #[tokio::test]
    async fn run_turn_streams_text_and_outcome() {
        let backend = Arc::new(MockBackend::default());
        backend.push_turn(vec![text_delta("hel"), text_delta("lo"), finish_delta()]);
        let svc = build_service(backend);

        let resp = svc
            .run_turn(Request::new(pb::RunTurnRequest {
                session_id: "s1".to_string(),
                prompt: "hi".to_string(),
                provider: String::new(),
                model: String::new(),
                reset_session: false,
            }))
            .await
            .unwrap();
        let mut stream = resp.into_inner();

        let mut text = String::new();
        let mut saw_outcome = false;
        while let Some(item) = stream.next().await {
            let delta = item.unwrap();
            match delta.kind {
                Some(pb::turn_delta::Kind::Text(t)) => text.push_str(&t),
                Some(pb::turn_delta::Kind::Outcome(o)) => {
                    saw_outcome = true;
                    assert_eq!(o.stop_reason, "end_turn");
                }
                _ => {}
            }
        }
        assert_eq!(text, "hello");
        assert!(saw_outcome);
    }

    #[tokio::test]
    async fn list_and_get_session_after_run_turn() {
        let backend = Arc::new(MockBackend::default());
        backend.push_turn(vec![text_delta("ok"), finish_delta()]);
        let svc = build_service(backend);

        // Drive one turn so the session gets persisted.
        let resp = svc
            .run_turn(Request::new(pb::RunTurnRequest {
                session_id: "s-abc".to_string(),
                prompt: "hello there".to_string(),
                provider: String::new(),
                model: String::new(),
                reset_session: false,
            }))
            .await
            .unwrap();
        let mut stream = resp.into_inner();
        while stream.next().await.is_some() {}

        let list = svc
            .list_sessions(Request::new(pb::ListSessionsRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(list.sessions.len(), 1);
        assert_eq!(list.sessions[0].id, "s-abc");

        let get = svc
            .get_session(Request::new(pb::GetSessionRequest {
                id: "s-abc".to_string(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(get.summary.is_some());
        assert!(get
            .messages
            .iter()
            .any(|m| m.role == "user" && m.content == "hello there"));
    }

    // SlowBackend — introduces a delay so we can cancel mid-turn.
    struct SlowBackend {
        delay: Duration,
    }

    impl SlowBackend {
        fn new(delay: Duration) -> Self {
            Self { delay }
        }
    }

    #[async_trait]
    impl QueryBackend for SlowBackend {
        async fn chat_stream(
            &self,
            _req: pb::ChatRequest,
        ) -> Result<futures::stream::BoxStream<'static, Result<pb::ChatDelta, Status>>> {
            let delay = self.delay;
            let stream = async_stream::stream! {
                tokio::time::sleep(delay).await;
                yield Ok(pb::ChatDelta {
                    kind: Some(pb::chat_delta::Kind::Finish(pb::TurnFinish {
                        stop_reason: "end_turn".to_string(),
                        input_tokens: 0,
                        output_tokens: 0,
                    })),
                });
            };
            Ok(Box::pin(stream))
        }

        async fn on_turn_start(&self, _ctx: pb::TurnContext) -> Result<pb::HookDecision> {
            Ok(pb::HookDecision {
                kind: pb::hook_decision::Kind::Allow as i32,
                reason: String::new(),
                rewritten_payload: Vec::new(),
            })
        }

        async fn on_tool_call(&self, _event: pb::ToolCallEvent) -> Result<pb::HookDecision> {
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

    fn build_slow_service(delay: Duration) -> QueryHostService {
        let backend: Arc<dyn QueryBackend> = Arc::new(SlowBackend::new(delay));
        let engine = Arc::new(QueryEngine::new(
            backend,
            Arc::new(ToolRegistry::with_builtins()),
        ));
        QueryHostService::new_in_memory(engine, "mock".to_string(), "mock-1".to_string())
    }

    #[tokio::test]
    async fn cancel_turn_cancels_active_turn() {
        let svc = build_slow_service(Duration::from_secs(10));

        // Start a long-running turn.
        let _resp = svc
            .run_turn(Request::new(pb::RunTurnRequest {
                session_id: "cancel-me".to_string(),
                prompt: "slow".to_string(),
                provider: String::new(),
                model: String::new(),
                reset_session: false,
            }))
            .await
            .unwrap();

        // Give the spawn a moment to register the token.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Cancel it.
        let cancel_resp = svc
            .cancel_turn(Request::new(pb::CancelTurnRequest {
                session_id: "cancel-me".to_string(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(cancel_resp.ok);
        assert_eq!(cancel_resp.message, "cancelled");
    }

    #[tokio::test]
    async fn cancel_turn_no_active_turn() {
        let svc = build_slow_service(Duration::from_secs(1));

        let resp = svc
            .cancel_turn(Request::new(pb::CancelTurnRequest {
                session_id: "nonexistent".to_string(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!resp.ok);
        assert_eq!(resp.message, "no active turn");
    }

    #[tokio::test]
    async fn concurrent_run_turn_cancels_previous() {
        let svc = build_slow_service(Duration::from_secs(10));

        // Start first turn.
        let resp1 = svc
            .run_turn(Request::new(pb::RunTurnRequest {
                session_id: "dup".to_string(),
                prompt: "first".to_string(),
                provider: String::new(),
                model: String::new(),
                reset_session: false,
            }))
            .await
            .unwrap();
        let mut stream1 = resp1.into_inner();

        // Give the first spawn a moment.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Start second turn on the same session — should auto-cancel the first.
        let _resp2 = svc
            .run_turn(Request::new(pb::RunTurnRequest {
                session_id: "dup".to_string(),
                prompt: "second".to_string(),
                provider: String::new(),
                model: String::new(),
                reset_session: false,
            }))
            .await
            .unwrap();

        // The first stream should receive a cancelled outcome.
        let mut saw_cancelled = false;
        while let Some(item) = stream1.next().await {
            if let Ok(delta) = item {
                match delta.kind {
                    Some(pb::turn_delta::Kind::Outcome(o)) => {
                        if o.stop_reason == "cancelled" {
                            saw_cancelled = true;
                        }
                    }
                    Some(pb::turn_delta::Kind::Error(e)) => {
                        if e.contains("cancel") {
                            saw_cancelled = true;
                        }
                    }
                    _ => {}
                }
            }
        }
        assert!(saw_cancelled, "first turn should have been cancelled");
    }

    #[tokio::test]
    async fn watch_session_receives_bus_events() {
        let backend = Arc::new(MockBackend::default());
        backend.push_turn(vec![text_delta("hi"), finish_delta()]);
        let svc = build_service(backend);

        // Subscribe BEFORE the turn starts (broadcast does not replay).
        let watch_resp = svc
            .watch_session(Request::new(pb::WatchSessionRequest {
                session_id: "w1".to_string(),
            }))
            .await
            .unwrap();
        let mut watch_stream = watch_resp.into_inner();

        // Run a turn that will publish events to the bus.
        let turn_resp = svc
            .run_turn(Request::new(pb::RunTurnRequest {
                session_id: "w1".to_string(),
                prompt: "hello".to_string(),
                provider: String::new(),
                model: String::new(),
                reset_session: false,
            }))
            .await
            .unwrap();
        // Drain the turn stream to completion.
        let mut turn_stream = turn_resp.into_inner();
        while turn_stream.next().await.is_some() {}

        // Collect watch events (with a timeout to avoid hanging).
        let mut event_types = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(2), watch_stream.next()).await {
                Ok(Some(Ok(event))) => {
                    event_types.push(event.event_type.clone());
                    if event.event_type == "outcome" {
                        break;
                    }
                }
                _ => break,
            }
        }

        assert!(
            event_types.contains(&"assistant_text".to_string()),
            "should contain assistant_text, got: {event_types:?}"
        );
        assert!(
            event_types.contains(&"outcome".to_string()),
            "should contain outcome, got: {event_types:?}"
        );
    }

    #[tokio::test]
    async fn watch_session_two_subscribers() {
        let backend = Arc::new(MockBackend::default());
        backend.push_turn(vec![text_delta("x"), finish_delta()]);
        let svc = build_service(backend);

        // Two subscribers on the same session.
        let resp_a = svc
            .watch_session(Request::new(pb::WatchSessionRequest {
                session_id: "w2".to_string(),
            }))
            .await
            .unwrap();
        let mut stream_a = resp_a.into_inner();

        let resp_b = svc
            .watch_session(Request::new(pb::WatchSessionRequest {
                session_id: "w2".to_string(),
            }))
            .await
            .unwrap();
        let mut stream_b = resp_b.into_inner();

        // Run a turn.
        let turn_resp = svc
            .run_turn(Request::new(pb::RunTurnRequest {
                session_id: "w2".to_string(),
                prompt: "hi".to_string(),
                provider: String::new(),
                model: String::new(),
                reset_session: false,
            }))
            .await
            .unwrap();
        let mut turn_stream = turn_resp.into_inner();
        while turn_stream.next().await.is_some() {}

        // Both subscribers should receive the outcome event.
        let mut a_got_outcome = false;
        let mut b_got_outcome = false;
        loop {
            match tokio::time::timeout(Duration::from_secs(2), stream_a.next()).await {
                Ok(Some(Ok(e))) if e.event_type == "outcome" => { a_got_outcome = true; break; }
                Ok(Some(Ok(_))) => continue,
                _ => break,
            }
        }
        loop {
            match tokio::time::timeout(Duration::from_secs(2), stream_b.next()).await {
                Ok(Some(Ok(e))) if e.event_type == "outcome" => { b_got_outcome = true; break; }
                Ok(Some(Ok(_))) => continue,
                _ => break,
            }
        }
        assert!(a_got_outcome, "subscriber A should get outcome");
        assert!(b_got_outcome, "subscriber B should get outcome");
    }

    #[tokio::test]
    async fn delete_session() {
        let backend = Arc::new(MockBackend::default());
        backend.push_turn(vec![finish_delta()]);
        let svc = build_service(backend);

        svc.run_turn(Request::new(pb::RunTurnRequest {
            session_id: "doomed".to_string(),
            prompt: "x".to_string(),
            provider: String::new(),
            model: String::new(),
            reset_session: false,
        }))
        .await
        .unwrap()
        .into_inner()
        .next()
        .await;

        let resp = svc
            .delete_session(Request::new(pb::DeleteSessionRequest {
                id: "doomed".to_string(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.ok);

        let list = svc
            .list_sessions(Request::new(pb::ListSessionsRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(list.sessions.len(), 0);
    }
}
