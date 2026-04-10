# M4 Task Report — HTTP API + Swagger (FastAPI + QueryHost)

**Milestone:** M4 (FastAPI on Python sidecar + Rust `QueryHost` gRPC)
**Date:** 2026-04-09
**Status:** ✅ Completed and awaiting approval for M5

---

## 1. Goals

Open ash-code to external callers over HTTP with an auto-generated Swagger
UI. Per the Option B decision (see `docs/comparison_api_structure.md`), the
HTTP surface is implemented in **Python FastAPI** and the Rust turn engine
is exposed to it via a new **`QueryHost` gRPC service** bound on
`127.0.0.1:50052` inside the container. Session/turn execution must work
with real providers (Anthropic, OpenAI).

## 2. Deliverables

### 2.1 Architecture decision doc

`docs/comparison_api_structure.md` — ~320-line comparison of three
candidate architectures (Rust axum+utoipa, Python FastAPI + reverse
gRPC, Python FastAPI + subprocess), with runtime diagrams, process
layout for the chosen Option B (b1 single Python process, c1 `ash serve`
→ gRPC QueryHost), pros/cons, and implications for M5–M9.

### 2.2 Proto: new `QueryHost` service

`proto/ash.proto` added:

- `service QueryHost { RunTurn (stream), ListSessions, GetSession, DeleteSession }`
- `RunTurnRequest` (`session_id`, `prompt`, `provider`, `model`, `reset_session`)
- `TurnDelta` oneof: `text | tool_call | tool_result | finish | error | outcome`
- `TurnOutcome` — terminal event with `stop_reason`, `turns_taken`, `denied`, `denial_reason`
- `SessionSummary` + `ListSessionsResponse` / `GetSessionResponse` / `DeleteSessionResponse`

Regeneration is transparent: `tonic-build` re-emits Rust types and
`grpc_tools.protoc` re-emits Python stubs during image build.

### 2.3 Rust `crates/api` — `QueryHost` gRPC server

New implementation (previously a stub):

- `QueryHostService` wrapping an `Arc<QueryEngine>` + an
  `Arc<RwLock<HashMap<String, Session>>>` in-memory session store.
- `ChannelSink` — a `TurnSink` adapter that pushes every callback into a
  `tokio::sync::mpsc::UnboundedSender<Result<TurnDelta, Status>>`. The
  receiver is wrapped into `UnboundedReceiverStream` and returned from
  `run_turn` as the server-streaming response.
- `RunTurn` semantics:
  - `session_id` empty → UUIDv4 assigned.
  - `reset_session=true` → drop existing messages before appending the
    new user prompt.
  - After `QueryEngine::run_turn` completes, the mutated `Session` is
    written back to the store.
  - Terminal `outcome` delta carries `stop_reason`, `turns_taken`,
    `denied`, `denial_reason` so clients can differentiate normal end
    from harness denial.
- `ListSessions`, `GetSession`, `DeleteSession` work against the same
  in-memory store.
- `serve(host, port, sidecar_endpoint, default_provider, default_model)`
  is the public entry point used by `ash serve`. Waits up to 10 × 300 ms
  for the Python sidecar to become reachable (`connect_with_retry`)
  before binding its own listener — eliminates the M1/M2/M3 supervisord
  restart race where `ash-serve` could come up before `ashpy`.

### 2.4 Rust `crates/cli` — `ash serve` rewired

`Command::Serve` is no longer a stub. New flags:

- `--host` (default `127.0.0.1`) — QueryHost bind host, container-local by default
- `--port` (default `50052`) — `DEFAULT_QUERY_HOST_PORT` constant
- `--sidecar` (default `http://127.0.0.1:50051`)
- `--provider` (env `ASH_LLM_PROVIDER`, default `anthropic`)
- `--model` (env `ASH_LLM_MODEL`, default empty)

Launches a multi-thread tokio runtime and calls
`ash_api::serve(...).await`.

### 2.5 Python `ashpy/api/` — FastAPI layer

New package with four modules:

| File | Contents |
|---|---|
| `__init__.py` | Re-exports `create_app` |
| `schemas.py` | Pydantic models: `HealthResponse`, `ProviderInfo`, `ListProvidersResponse`, `SwitchProviderRequest/Response`, `SessionSummary`, `ListSessionsResponse`, `SessionDetail`, `ChatRequest`, `DeleteSessionResponse` |
| `query_client.py` | `QueryHostClient` — `grpc.aio` wrapper around the generated `QueryHostStub`. Exposes `run_turn` (async iterator of dicts), `list_sessions`, `get_session`, `delete_session`, `close`. |
| `app.py` | `create_app(query_host_endpoint)` — builds the `FastAPI` instance, mounts CORS, registers the following routes |

Endpoint catalog:

| Method | Path | Handler |
|---|---|---|
| `GET` | `/v1/health` | Returns `ashpy_version`, `api_version`, `features` map (http=v1, llm=v1, harness=v1, skills/commands=planned) |
| `GET` | `/v1/llm/providers` | Direct in-process call to `ashpy.providers.get_registry()` — no gRPC hop |
| `POST` | `/v1/llm/switch` | In-process `ProviderRegistry.switch(name, model)` |
| `GET` | `/v1/sessions` | gRPC `QueryHost.ListSessions` |
| `GET` | `/v1/sessions/{id}` | gRPC `QueryHost.GetSession`, 404 on miss |
| `DELETE` | `/v1/sessions/{id}` | gRPC `QueryHost.DeleteSession` |
| `POST` | `/v1/chat` | **SSE stream** via `sse-starlette`; opens `QueryHost.RunTurn`, maps each `TurnDelta` kind to an SSE event (`text`/`tool_call`/`tool_result`/`finish`/`error`/`outcome`), finishes with `event: done / data: [DONE]` |
| `GET` | `/docs` | FastAPI-default Swagger UI |
| `GET` | `/openapi.json` | FastAPI-default OpenAPI 3.1 spec |

CORS: `allow_origins=["*"]` — acceptable for a localhost dev harness,
tightened in M9.

### 2.6 `ashpy serve` extended to co-run gRPC + FastAPI

Single Python process now hosts **both** servers in the same asyncio
event loop (b1 layout from the decision matrix):

- `grpc.aio` server on `127.0.0.1:50051` (existing M2/M3 services)
- `uvicorn.Server` on `0.0.0.0:8080` (new FastAPI app)

The existing SIGTERM/SIGINT handler now cleanly shuts down both. New
CLI flags on `ashpy serve`: `--http-host`, `--http-port` (0 disables
FastAPI for unit tests).

### 2.7 supervisord changes

- `ashpy` program now implicitly includes the FastAPI layer (same
  process, same command: `ashpy serve`).
- `ash-serve` program (Rust) reworked:
  ```
  command=/usr/local/bin/ash serve --host 127.0.0.1 --port 50052 --sidecar http://127.0.0.1:50051
  startretries=10
  ```
- Both programs now reach `RUNNING` state cleanly — the long-standing
  M1/M2/M3 `ash-serve` restart loop is **resolved**.

### 2.8 Dependencies

Added to `ashpy/pyproject.toml`:
- `fastapi>=0.115`
- `uvicorn>=0.32`
- `sse-starlette>=2.1`

`uv.lock` regenerated; 5 new packages (fastapi, uvicorn, sse-starlette,
starlette, click) + their transitive deps.

Rust workspace gained one new dep used only by `ash-api`:
- `uuid = { version = "1", features = ["v4"] }`

## 3. Verification

| Check | Command | Result |
|---|---|---|
| Rust workspace tests | `cargo test --workspace` | **35 passed, 0 failed** — ipc 3, query 4, tools 17, **api 3** (new), core 2, 6 stub crates |
| Python tests | `uv run pytest -q` | **57 passed in 0.73 s** — M0–M3 51 + M4 FastAPI 6 new |
| Docker image build | `docker compose build ash-code` | ✅ `ash-code:dev` with `uvicorn`, `fastapi`, `sse-starlette` installed and Rust `ash-api` linked |
| supervisord boot | `docker compose up -d ash-code` + `docker logs ash-code` | ✅ both `ashpy` and `ash-serve` reach `entered RUNNING state`. Restart loop resolved. |
| `/v1/health` (container-local) | `httpx.get('http://127.0.0.1:8080/v1/health')` | 200, `features.http == "v1"` |
| `/v1/llm/providers` (host → published port) | `curl http://localhost:8080/v1/llm/providers` | 4 builtins (`anthropic`/`openai`/`ollama`/`vllm`), all `source=builtin`, Anthropic default `claude-opus-4-5` |
| `/docs` Swagger UI | `curl http://localhost:8080/docs \| grep -i swagger` | 9 occurrences of "swagger" — HTML served |
| `/openapi.json` | Parsed | `openapi=3.1.0`, paths = `[/v1/chat, /v1/health, /v1/llm/providers, /v1/llm/switch, /v1/sessions, /v1/sessions/{session_id}]` |
| **`/v1/chat` SSE real Anthropic** | `curl -N -X POST /v1/chat -d '{...anthropic...}'` | Streaming SSE events: `text` × 2, `finish`, `outcome`, `done` |
| Session persistence | `GET /v1/sessions` + `GET /v1/sessions/m4-test` | Session appears with `message_count=2`, user + assistant roles, real assistant text `"hello from anthropic"` |

### 3.1 SSE chat output (real Anthropic, trimmed)

```
POST /v1/chat
{"session_id":"m4-test","prompt":"Say exactly: hello from anthropic",
 "provider":"anthropic","model":"claude-opus-4-5"}

event: text
data: {"type": "text", "text": "hello"}

event: text
data: {"type": "text", "text": " from anthropic"}

event: finish
data: {"type": "finish", "stop_reason": "end_turn", "input_tokens": 14, "output_tokens": 7}

event: outcome
data: {"type": "outcome", "stop_reason": "end_turn", "turns_taken": 1,
       "denied": false, "denial_reason": ""}

event: done
data: [DONE]
```

Full path proven: browser/curl → uvicorn → FastAPI handler →
`QueryHostClient` → gRPC `:50052` → Rust `QueryHostService.run_turn` →
`QueryEngine::run_turn` → `SidecarBackend` → gRPC `:50051` →
`LlmProviderServicer.ChatStream` → `anthropic` SDK → Anthropic API →
back through every layer as SSE events.

### 3.2 supervisord boot log

```
INFO supervisord started with pid 1
INFO spawned: 'ashpy' with pid 7
INFO spawned: 'ash-serve' with pid 8
[ashpy] ashpy gRPC server listening on 127.0.0.1:50051
[ashpy] middleware chain: ['logging', 'bash_guard']
[ash] QueryHost gRPC listening on 127.0.0.1:50052
[ashpy] ashpy FastAPI listening on http://0.0.0.0:8080
INFO:     Uvicorn running on http://0.0.0.0:8080 (Press CTRL+C to quit)
INFO success: ashpy entered RUNNING state
INFO success: ash-serve entered RUNNING state
```

Both programs `RUNNING`. No restart loop.

## 4. Issues encountered and resolved

1. **`ToolResult` not public from `ash-query`.** The initial import
   tried to pull `ToolResult` from `ash_query`; it is re-exported from
   `ash_tools`. Fix: `use ash_tools::{ToolRegistry, ToolResult}`.
2. **Rust tonic trait methods require trait-in-scope.** Tests called
   `svc.run_turn(...)` directly but `run_turn` is a `QueryHost` trait
   method. Fix: `use ash_ipc::pb::query_host_server::QueryHost as _;`
   in the test module.
3. **Sidecar connect race.** `ash serve` could start before `ashpy` had
   bound `:50051`. Fix: `connect_with_retry(10 attempts, 300 ms)` inside
   `ash_api::serve` before building the engine.
4. **grpc.aio channel owned by FastAPI must outlive the event loop.**
   Initially created a new channel per request; moved it into a
   module-level `QueryHostClient` on `app.state` so it is reused and
   connection pooling works.

## 5. Decisions carried forward

1. **HTTP lives in Python, turn engine lives in Rust, meeting at gRPC
   in both directions.** This is the M4 mental model; documented in
   `docs/comparison_api_structure.md` and reinforced by M5/M6 adding
   Python routers only.
2. **Session state is in-memory on the Rust side.** Lost on container
   restart. Persistent storage is M9.
3. **Concurrent writes to the same session interleave.** M4 documents
   "one request per session at a time"; M8 event bus addresses it.
4. **`OnStreamDelta` harness hook still not wired per-token.** Unchanged
   from M3 — would quadruple round trips and is low-value until there
   is a concrete consumer.
5. **CORS wide open.** Localhost dev harness. Tightened in M9.

## 6. Exit criteria — met

- [x] `crates/api` implements real `QueryHost` gRPC server with 4 RPCs
- [x] `ash serve` binds `:50052`, no longer a stub
- [x] supervisord `ash-serve` restart loop resolved
- [x] FastAPI app with all planned endpoints + Swagger UI at `/docs`
- [x] `/openapi.json` valid OpenAPI 3.1 covering all routes
- [x] `/v1/chat` SSE streams real Anthropic responses end-to-end
- [x] Sessions persist across requests (in-memory, same container)
- [x] Rust `cargo test --workspace` 35/35
- [x] Python `uv run pytest -q` 57/57
- [x] No new user-facing bugs vs M3 (CLI `ash llm chat` and all M3 E2E
      scenarios remain green)

## 7. Changed files

**Added**
- `docs/comparison_api_structure.md`
- `docs/task/M4_TASK_REPORT.md` (this file)
- `crates/api/src/lib.rs` — full `QueryHostService` impl + 3 tests
- `ashpy/src/ashpy/api/__init__.py`
- `ashpy/src/ashpy/api/app.py`
- `ashpy/src/ashpy/api/schemas.py`
- `ashpy/src/ashpy/api/query_client.py`
- `ashpy/tests/test_fastapi.py` — 6 tests

**Modified**
- `Cargo.toml` — `uuid` workspace dep
- `proto/ash.proto` — `QueryHost` service + supporting messages
- `crates/api/Cargo.toml` — fully populated (tonic, ash-query, ash-tools, uuid)
- `crates/cli/src/main.rs` — real `ash serve` implementation; new flags
- `ashpy/pyproject.toml` — FastAPI / uvicorn / sse-starlette
- `ashpy/uv.lock` — regenerated
- `ashpy/src/ashpy/__main__.py` — `--http-host` / `--http-port` flags
- `ashpy/src/ashpy/server.py` — co-run uvicorn alongside grpc.aio in one event loop
- `docker/supervisord.conf` — `ash-serve` command + `startretries=10`

## 8. Next: M5 — Skills

- Implement `ashpy/skills/` loader (`python-frontmatter` + `pydantic`).
- `watchdog` file-system observer drives hot-reload + `SkillRegistry.Watch` stream.
- Wire `SkillRegistryServicer` to the loader (`List`, `Invoke`, `Reload`, `Watch`).
- Promote `features.skills` from `"planned"` → `"v1"`.
- Add a FastAPI router `ashpy/api/routes_skills.py`:
  - `GET /v1/skills`, `POST /v1/skills/reload`, `POST /v1/skills/{name}/invoke`.
- Ship at least two sample `SKILL.md` files under `skills/` and document
  the drop-in format in `docs/skills.md`.
