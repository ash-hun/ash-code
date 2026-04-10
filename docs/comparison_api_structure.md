# API Structure Comparison — Rust axum vs Python FastAPI (ash-code M4)

This document records the architectural decision for the ash-code HTTP API
layer introduced in M4. Three options were considered. The chosen path is
**Option B** — FastAPI on the Python sidecar, backed by a new Rust `QueryHost`
gRPC service reached over an internal reverse channel.

## Context — what M4 adds

Up to M3, ash-code was CLI-only. The components were:

- **Rust** — `ash` CLI, TUI (M7 scheduled), `crates/query` turn loop,
  `crates/tools` built-ins, `crates/ipc` gRPC **client** talking to Python.
- **Python sidecar (`ashpy`)** — `grpc.aio` server on `127.0.0.1:50051`
  hosting `LlmProvider`, `SkillRegistry`, `CommandRegistry`, `Harness`,
  `ToolRegistry`. M3 added the middleware chain.

M4 must expose a **public HTTP API** on `0.0.0.0:8080` with Swagger UI so
that (a) browsers can drive the agent, (b) external scripts can integrate,
(c) future CI/tooling can call endpoints without going through the TUI.

The pivotal constraint: the **turn loop lives in Rust** (`QueryEngine` in
`crates/query`). Any HTTP endpoint that runs a turn must reach into that
loop. The three options differ in *how* HTTP reaches the turn loop.

---

## Option A — Rust axum + utoipa (rejected)

### Shape

```
Host :8080  ──►  Rust axum  ──►  QueryEngine::run_turn (in-process)
                    │
                    └──►  Swagger UI via utoipa-swagger-ui
                    │
                    └──►  gRPC client → Python sidecar :50051
                          (LlmProvider, Harness, etc.)
```

One Rust process serves HTTP. `crates/api` would have been expanded into
a full axum stack with route handlers, utoipa `#[openapi]` derives,
utoipa-swagger-ui for the `/docs` page, and SSE streams for `/v1/chat`.

### Pros

- No architectural change. Current process layout (Rust HTTP + Python gRPC)
  preserved.
- `QueryEngine` called in-process — zero extra IPC hops.
- Single Rust binary still handles everything.
- axum is fast and type-safe.

### Cons

- **Swagger auto-generation is manual.** `utoipa` requires
  `#[utoipa::path(...)]` annotations on every handler and
  `#[derive(ToSchema)]` on every request/response struct. Every field
  change ripples into docstring updates. FastAPI gets the same result
  from Pydantic models for free.
- **New dependencies in Rust:** `axum`, `utoipa`, `utoipa-swagger-ui`,
  `tower`, `tower-http`. Compile time grows meaningfully.
- **Every new user-facing surface lives in Rust.** M5 adds skill
  endpoints, M6 adds command endpoints — each one is a new Rust handler
  plus a utoipa schema plus a test. The customization layer (M0.5 says
  it is Python) would still be Python, but the **doors** to it would be
  in Rust.
- **Iteration speed.** Every API tweak requires a Rust rebuild inside
  Docker. That is slower than touching Python and restarting a uvicorn
  worker.
- **Test surface duplicates M3.** `crates/query` tests already exercise
  the turn loop; axum integration tests would re-exercise it over HTTP.

### Rejected because

The M0.5 extensibility design put customization on the Python side
precisely because Python is the faster, more flexible layer for user-
facing work. Putting HTTP — arguably the most customization-prone layer
of all — on the Rust side contradicts that principle, and the Swagger
ergonomics are noticeably worse.

---

## Option B — FastAPI on Python sidecar + new Rust `QueryHost` gRPC (chosen)

### Shape

```
Host :8080  ──►  Python uvicorn / FastAPI
                     │   (Swagger UI auto-generated at /docs)
                     │
                     ├──►  in-process: providers, skills, commands, middleware
                     │     (no IPC — same Python process as the gRPC server)
                     │
                     └──►  /v1/chat handler:
                           gRPC client → 127.0.0.1:50052
                                              │
                                              ▼
                                     Rust ash serve (QueryHost gRPC)
                                              │
                                              ├──►  QueryEngine::run_turn
                                              │     ├──►  ToolRegistry (Rust, in-process)
                                              │     └──►  SidecarBackend (gRPC client)
                                              │                 │
                                              │                 ▼
                                              │        Python sidecar :50051
                                              │        (LlmProvider.ChatStream
                                              │         + Harness hooks)
                                              └──►  stream TurnDelta back
```

### Process layout inside the container (single Python process = b1)

`supervisord` runs **two** programs:

1. **`ashpy serve`** — one Python process that owns, in a single
   `asyncio` event loop:
   - `grpc.aio` server on `127.0.0.1:50051` (existing M2/M3 services)
   - `uvicorn` FastAPI app on `0.0.0.0:8080` (new in M4)
   - Both share the same provider registry, middleware chain, and
     `asyncio` runtime. The FastAPI handlers that list providers / skills
     / commands call the Python modules **directly, no gRPC hop**.
2. **`ash serve`** — one Rust process hosting a tonic gRPC server on
   `127.0.0.1:50052` that implements the new `QueryHost` service
   (chosen in Q3 "c1").

### The new `QueryHost` proto service

Added to `proto/ash.proto`:

```proto
service QueryHost {
  rpc RunTurn(RunTurnRequest) returns (stream TurnDelta);
  rpc ListSessions(ListSessionsRequest) returns (ListSessionsResponse);
  rpc GetSession(GetSessionRequest) returns (GetSessionResponse);
  rpc DeleteSession(DeleteSessionRequest) returns (DeleteSessionResponse);
}

message RunTurnRequest {
  string session_id = 1;
  string prompt = 2;
  string provider = 3;   // optional — empty = sidecar default
  string model = 4;      // optional
  bool reset_session = 5;
}

message TurnDelta {
  oneof kind {
    string text = 1;
    ToolCall tool_call = 2;
    ToolResult tool_result = 3;
    TurnFinish finish = 4;
    string error = 5;
  }
}
```

The Rust side implements `QueryHost`; the Python side calls it as a
client from the FastAPI `/v1/chat` handler and converts each `TurnDelta`
into a Server-Sent Event line.

### Pros

- **Swagger is free.** FastAPI reflects Pydantic models into a full
  OpenAPI 3.1 spec at `/openapi.json` and serves Swagger UI at `/docs`
  with zero handler-level annotations beyond the route decorators.
- **Python owns the user-facing surface.** Consistent with M0.5: every
  door the developer might want to customize is in Python. M5/M6 add
  new HTTP endpoints by dropping a FastAPI router file — no Rust touch
  required.
- **In-process wins for Python-owned endpoints.** `GET /v1/llm/providers`
  is a direct function call on the provider registry; no gRPC round-trip
  at all. Same for skills (M5) and commands (M6).
- **Rust stays focused.** `crates/api` becomes a small tonic server that
  wires `QueryHost` to the existing `QueryEngine`. No new web framework
  inside the Rust tree.
- **Fast iteration.** Touching the API shape means editing Python and
  restarting one process; no Rust rebuild inside Docker.
- **`ash llm chat` removal (M7) is clean.** The SSE streaming path is
  the FastAPI handler calling gRPC, which is exactly what the TUI will
  use — no transitional shim to tear down.
- **Testability.** FastAPI's `TestClient` is synchronous and
  one-line-per-test. Rust gRPC server tested with in-process mock
  backends (M3 pattern reused).

### Cons

- **Bidirectional gRPC.** Python→Rust (new QueryHost) and Rust→Python
  (existing LlmProvider/Harness/etc.) coexist. Slight cognitive overhead;
  mitigated by documenting "Python owns HTTP, Rust owns turn engine" as
  the mental model.
- **Extra IPC hop on `/v1/chat`.** Browser → uvicorn → gRPC(:50052) →
  `QueryEngine` → gRPC(:50051) → provider SDK. Four processes along the
  critical path for a chat request. In practice each hop is sub-millisecond
  on loopback; the network latency to the real LLM API dominates.
- **Two supervisord programs instead of one.** Slightly more operational
  surface for logging / restart policy. Negligible.
- **Python must link `grpcio` + `fastapi` + `uvicorn`.** Adds ~20 MB to
  the sidecar venv. Acceptable for a dev harness.

### Why this was chosen

1. FastAPI's Swagger ergonomics beat utoipa by a wide margin for a
   small team iterating quickly on endpoint shapes.
2. It matches the M0.5 architectural split — Python for anything the
   developer will customize, Rust for the core engine.
3. The proto/gRPC delta is one new service (`QueryHost`) with four RPCs.
   The Rust implementation is a ~200 LOC tonic server that delegates
   straight into `QueryEngine`. The Python side gets `sse_starlette` and
   a direct gRPC client.
4. It unblocks M5/M6: skill and command endpoints can be added by
   dropping a Python file, without touching Rust or the proto.

---

## Option C — FastAPI + subprocess wrap of `ash llm chat` (rejected)

### Shape

```
Host :8080  ──►  Python FastAPI
                     │
                     └──►  /v1/chat: subprocess "ash llm chat <prompt>"
                                       │
                                       └──► parse stdout → SSE events
```

### Pros

- Absolute smallest change — no proto edits, no Rust server.
- Nothing new to learn; everything is Python.

### Cons

- **stdout parsing is fragile.** `ash llm chat`'s current output format
  is `"text"` + `"[tool_call …]"` + `"[finish …]"` lines meant for human
  reading, not machine parsing. Any format change breaks the API.
- **No structured errors.** Exit code is the only signal for "turn
  failed"; error reasons get mashed into stderr.
- **`ash llm chat` removal (M7) creates ~2× rework.** The M7 TUI will
  replace the CLI, and this subprocess-based API would then need to be
  rewritten to call something else. Option B builds the right thing once.
- **Security.** Shelling out with arguments composed from HTTP input is
  an injection attack surface unless carefully escaped. Option B carries
  typed fields over gRPC.
- **Cancellation / back-pressure.** Killing a subprocess mid-turn leaves
  the Rust process in an indeterminate state; Option B uses gRPC stream
  cancellation which is clean.

### Rejected because

Technical debt accumulates faster than the "speed to first working
endpoint" saves. The rewrite cost at M7 is the deciding factor.

---

## Decision summary

| Axis | Option A (axum) | **Option B (FastAPI + QueryHost)** | Option C (subprocess) |
|---|---|---|---|
| Swagger quality | manual, medium | **auto, high** | auto, high |
| Architectural fit with M0.5 | misaligned (HTTP in Rust) | **aligned (HTTP in Python)** | aligned |
| New Rust code | ~800 LOC | **~250 LOC (QueryHost server)** | 0 |
| New Python code | 0 | ~600 LOC (FastAPI app) | ~300 LOC (parser) |
| New proto services | 0 | **1 (QueryHost)** | 0 |
| IPC hops for `/v1/chat` | 2 | 4 | 3 + subprocess |
| Iteration speed | slow (Rust rebuild) | **fast (Python restart)** | fast |
| Extensibility for M5/M6 | Rust handler/route per feature | **Python router per feature** | fragile parser edits |
| `ash llm chat` removal impact | none | none | full rewrite |
| Fragility | low | low | high |

**Chosen:** Option B, b1 process layout, c1 `ash serve` naming.

---

## How the pieces fit together at runtime

```
┌───────────────────── ash-code container ────────────────────────────┐
│                                                                      │
│  supervisord                                                         │
│  ├──► ashpy serve                                                    │
│  │     (single Python process, one asyncio event loop)               │
│  │     ├──► grpc.aio server  on 127.0.0.1:50051                      │
│  │     │    (LlmProvider, SkillRegistry, CommandRegistry,            │
│  │     │     Harness, ToolRegistry placeholder)                      │
│  │     └──► uvicorn FastAPI on 0.0.0.0:8080                          │
│  │          ├── /docs, /openapi.json  (Swagger UI)                   │
│  │          ├── /v1/health                                           │
│  │          ├── /v1/llm/providers     → in-process registry call     │
│  │          ├── /v1/llm/switch        → in-process registry call     │
│  │          ├── /v1/sessions (GET/POST/DELETE)                       │
│  │          └── /v1/chat (POST, SSE stream)                          │
│  │                 │                                                 │
│  │                 └─► grpc.aio client → 127.0.0.1:50052             │
│  │                                                                  │
│  └──► ash serve                                                      │
│        (Rust tonic server on 127.0.0.1:50052)                        │
│        └──► QueryHost impl                                           │
│             └──► QueryEngine::run_turn                               │
│                  ├──► ToolRegistry (bash, file_*, grep, glob)        │
│                  └──► SidecarBackend ── gRPC ──► 127.0.0.1:50051     │
│                                                   (back to ashpy)   │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
        ▲                             ▲
        │ port 8080                   │ (not published; container-local)
        │                             │
        ▼                             ▼
     host browser                  (internal only)
     curl / external
     clients
```

- Only `:8080` is published to the host.
- `:50051` and `:50052` are strictly container-local loopback.
- The `/v1/chat` flow ultimately visits **four processes** on the critical
  path: browser → uvicorn → Rust tonic server → Python gRPC server →
  provider SDK. Each hop is loopback and dominated by the real LLM API
  network latency.

## Trade-offs accepted

1. **Extra IPC hop on chat requests.** Measured: ~1–3 ms per hop on
   loopback. Provider network latency (Anthropic/OpenAI round trip) is
   typically 100–1000 ms. The hop is noise.
2. **Two-way gRPC learning curve.** Mitigated by documentation
   (`docs/role_proto.md`, this file). The mental model is
   "Python owns the user surface, Rust owns the turn engine; they meet
   over gRPC in both directions."
3. **Session state still in-memory on the Rust side.** The Rust
   `QueryHost` keeps `HashMap<session_id, Session>`. Persistent storage
   is M9's problem. Documented in the M4 task report.
4. **Supervisord runs two programs instead of one.** Each is
   independently restartable; failures are isolated.
5. **FastAPI adds ~20 MB to the venv** (`fastapi`, `uvicorn`,
   `sse-starlette`, `starlette`). Acceptable for a dev tool.

## Implications for future milestones

- **M5 (skills)** adds a FastAPI router file under `ashpy/api/` with
  `GET /v1/skills`, `POST /v1/skills/reload`, `POST /v1/skills/{name}/invoke`.
  No Rust changes.
- **M6 (commands)** adds a FastAPI router file with
  `GET /v1/commands`, `POST /v1/commands/{name}/run`. No Rust changes.
- **M7 (TUI)** the TUI will call the same `QueryHost` gRPC directly
  (not the FastAPI HTTP layer), because it is a Rust process and the
  gRPC is one process away. The FastAPI layer is for *external*
  consumers.
- **M8 (event bus)** can route both HTTP `/v1/chat` and TUI chat through
  the same `QueryHost` session store, so a message typed in the TUI
  shows up in an HTTP client watching the same `session_id`.
- **M9 (persistence / security)** moves the Rust session store from
  in-memory to SQLite, tightens CORS, and adds authentication on the
  FastAPI layer.
