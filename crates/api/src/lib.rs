//! ash-api — Rust-side `QueryHost` gRPC server.
//!
//! This is the "turn engine host" that the Python FastAPI layer calls
//! from its `/v1/chat` handler. See `docs/comparison_api_structure.md`
//! for the full architecture story.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use ash_ipc::{pb, SidecarClient};
use ash_query::{QueryBackend, QueryEngine, Session, SidecarBackend, TurnSink};
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
// Service implementation
// ---------------------------------------------------------------------------

pub struct QueryHostService {
    engine: Arc<QueryEngine>,
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    default_provider: String,
    default_model: String,
}

impl QueryHostService {
    pub fn new(
        engine: Arc<QueryEngine>,
        default_provider: String,
        default_model: String,
    ) -> Self {
        Self {
            engine,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            default_provider,
            default_model,
        }
    }
}

#[async_trait]
impl pb::query_host_server::QueryHost for QueryHostService {
    type RunTurnStream = UnboundedReceiverStream<Result<pb::TurnDelta, Status>>;

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

        // Fetch or create the session.
        let mut session = {
            let map = self.sessions.read().await;
            match map.get(&session_id) {
                Some(s) if !req.reset_session => s.clone(),
                _ => Session::new(&session_id, &provider, &model),
            }
        };
        if req.reset_session {
            session = Session::new(&session_id, &provider, &model);
        }
        session.provider = provider;
        session.model = model;
        session.push_user(req.prompt);

        let (tx, rx) = mpsc::unbounded_channel();
        let stream = UnboundedReceiverStream::new(rx);

        let engine = self.engine.clone();
        let sessions = self.sessions.clone();
        let sid = session_id.clone();

        tokio::spawn(async move {
            let mut sink = ChannelSink::new(tx.clone());
            let outcome = match engine.run_turn(&mut session, &mut sink).await {
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
                    return;
                }
            };

            // Persist the (now mutated) session back into the store.
            {
                let mut map = sessions.write().await;
                map.insert(sid, session);
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

    async fn list_sessions(
        &self,
        _request: Request<pb::ListSessionsRequest>,
    ) -> Result<Response<pb::ListSessionsResponse>, Status> {
        let map = self.sessions.read().await;
        let sessions = map
            .values()
            .map(|s| pb::SessionSummary {
                id: s.id.clone(),
                provider: s.provider.clone(),
                model: s.model.clone(),
                message_count: s.messages.len() as i32,
            })
            .collect();
        Ok(Response::new(pb::ListSessionsResponse { sessions }))
    }

    async fn get_session(
        &self,
        request: Request<pb::GetSessionRequest>,
    ) -> Result<Response<pb::GetSessionResponse>, Status> {
        let id = request.into_inner().id;
        let map = self.sessions.read().await;
        let session = map
            .get(&id)
            .ok_or_else(|| Status::not_found(format!("session not found: {id}")))?;
        let summary = pb::SessionSummary {
            id: session.id.clone(),
            provider: session.provider.clone(),
            model: session.model.clone(),
            message_count: session.messages.len() as i32,
        };
        let messages = session
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
        let mut map = self.sessions.write().await;
        let existed = map.remove(&id).is_some();
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

    let service = QueryHostService::new(engine, default_provider, default_model);
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
        QueryHostService::new(engine, "mock".to_string(), "mock-1".to_string())
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
