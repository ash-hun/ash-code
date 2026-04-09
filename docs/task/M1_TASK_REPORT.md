# M1 Task Report — gRPC Wiring (Rust ↔ Python sidecar)

**Milestone:** M1 (gRPC IPC Wiring)
**Date:** 2026-04-09
**Status:** ✅ Completed and approved for M2

---

## 1. Goals

Connect the Rust host process (`ash`) and the Python sidecar (`ashpy`) with a
typed, streaming IPC channel so that every customization surface defined in
M0.5 (providers / skills / commands / harness hooks / tool plugins) lands on
the same well-defined transport. The quality of M1 is the foundation for M2–M8.

- Finalize `proto/ash.proto` with six services.
- Wire `tonic` on the Rust side with build-time codegen.
- Wire `grpcio` on the Python side with runtime (and image-time) codegen.
- Prove the round-trip end-to-end inside the built Docker image.

## 2. Deliverables

### 2.1 `proto/ash.proto` — finalized

Six services declared and approved by the user:

| Service | Purpose | Implementation milestone |
|---|---|---|
| `Health` | Liveness + version handshake | **M1 (now)** |
| `LlmProvider` | `ListProviders`, `Capabilities`, `ChatStream` (server-streaming), `Switch` | M2 |
| `SkillRegistry` | `List`, `Invoke`, `Reload`, `Watch` (server-streaming) | M5 |
| `CommandRegistry` | `List`, `Run`, `Reload` | M6 |
| `Harness` | `OnTurnStart`, `OnToolCall`, `OnStreamDelta`, `OnTurnEnd`, `HookDecision{ALLOW\|DENY\|REWRITE}` | M3 |
| `ToolRegistry` | `List`, `Invoke` | M3+ |

Key design points:
- `ChatDelta` uses a `oneof { text, tool_call, finish }` so text, tool-use, and
  the terminal delta all interleave on a single stream.
- `Health.Ping.PingResponse.features` is a string→string map; clients use it to
  distinguish "this service is live" from "this service exists but is
  `UNIMPLEMENTED`" without issuing every RPC.
- `Skill.model` allows per-skill model overrides (e.g. reviews on opus, simple
  answers on sonnet).
- `ToolSpec.input_schema`, `ToolInfo.input_schema`, `ToolCall.arguments` are
  carried as `bytes` (UTF-8 JSON) instead of being forced into a protobuf
  structure — JSON Schema already has a spec, no point restating it.
- `HookDecision.rewritten_payload` is `bytes` (JSON) with per-hook semantics
  documented at the call site in M3.

### 2.2 Rust — `crates/ipc`

- `Cargo.toml`: added `tonic = "0.12"`, `prost = "0.13"`, `tokio-stream`,
  `async-stream` at the workspace level; `tonic-build` as a build-dependency of
  `ash-ipc`.
- `build.rs`: resolves `<workspace>/proto/ash.proto`, emits
  `cargo:rerun-if-changed`, calls `tonic_build::configure().build_client(true)
  .build_server(true).compile_protos(...)`.
- `src/lib.rs`:
  - `pub mod pb { tonic::include_proto!("ash.v1"); }` — re-exports the full
    generated type surface.
  - `SidecarClient` — small wrapper around a `tonic::transport::Channel` that
    exposes `connect(endpoint, timeout)` and `ping()` for M1. Future milestones
    add `chat_stream`, `invoke_skill`, etc. on the same struct.
  - `DEFAULT_SIDECAR_ENDPOINT = "http://127.0.0.1:50051"` and
    `CLIENT_IDENTITY = "ash-cli/0.0.1"` constants.
  - In-process integration test: binds an ephemeral `TcpListener`, runs a
    minimal `Health` servicer on it with `tokio::spawn`, connects a
    `SidecarClient`, and asserts the `Ping` round-trip works and `features`
    contains the expected keys.

### 2.3 Rust — `crates/cli`

Added `--check-sidecar` / `--sidecar <url>` flags to `ash doctor`. When enabled,
the command builds a single-threaded tokio runtime, issues a real `Health.Ping`
through `SidecarClient`, and prints either
`sidecar: OK — ashpy/0.0.1 api=v1 features=6 (1.2 ms)` or a failure line with
`exit 2`. This is the one command users run to verify the sidecar is alive.

### 2.4 Python — `ashpy/src/ashpy/_codegen.py`

New module that compiles `ash.proto` into `ashpy/_generated/` on demand.

- Resolves the proto directory in priority order:
  1. `ASH_PROTO_DIR` env var (wins when set — used by the Docker image)
  2. `<repo>/proto` (local dev)
  3. `/build/proto`, `/opt/ashpy/proto` (Docker layouts)
- Calls `grpc_tools.protoc` to emit `ash_pb2.py` and `ash_pb2_grpc.py`.
- Post-processes the generated `ash_pb2_grpc.py` to rewrite
  `import ash_pb2 as ash__pb2` → `from . import ash_pb2 as ash__pb2` so the
  package is importable as `ashpy._generated.*` without manipulating
  `sys.path`.
- `ensure_generated()` is a cheap idempotent entry point used by the server
  module. `generate(force=True)` is used by the Dockerfile during image
  assembly so runtime start-up does not touch the filesystem.

### 2.5 Python — `ashpy/src/ashpy/server.py`

Replaced the M0 scaffold loop with a real gRPC server:

- `build_server(bind)` creates a `grpc.server(ThreadPoolExecutor(max_workers=8))`,
  registers servicers for all six services, and returns `(server, effective_bind)`.
  If the caller passes `127.0.0.1:0`, the effective bind reflects the OS-assigned
  port so tests can use it.
- `serve(bind)` starts the server, installs SIGTERM/SIGINT handlers, and calls
  `wait_for_termination()`. `supervisord` drives the real process.
- `HealthServicer.Ping` is **fully implemented** — it returns
  `ashpy/{__version__}`, `api_version = "v1"`, `received_unix_ms`, and a
  `features` map advertising `{"health": "v1", "llm": "planned",
  "skills": "planned", "commands": "planned", "harness": "planned",
  "tools": "planned"}`.
- All other services (`LlmProvider`, `SkillRegistry`, `CommandRegistry`,
  `Harness`, `ToolRegistry`) have placeholder servicers that return
  `grpc.StatusCode.UNIMPLEMENTED` with a message naming the owning milestone
  (e.g. "LlmProvider.ChatStream lands in M2"). Connection itself always
  succeeds, only the RPC body is gated.

### 2.6 Python — `ashpy/tests/test_grpc.py`

Four new tests covering the M1 surface:

1. `test_codegen_produces_stubs` — calls `_codegen.generate(force=True)` and
   asserts the output files exist.
2. `test_health_ping` — stands up an in-process server on an ephemeral port and
   validates every field of the `PingResponse` (server, api_version, features).
3. `test_llm_chat_stream_is_unimplemented` — iterates the `ChatStream` stream
   and asserts the client raises `RpcError` with
   `code() == UNIMPLEMENTED` and `details()` mentioning `"M2"`.
4. `test_harness_hook_is_unimplemented` — calls `Harness.OnTurnStart` and
   asserts the same `UNIMPLEMENTED` contract, confirming the six-service
   registration is wired correctly.

### 2.7 Docker

- `rust-builder`:
  - Bumped base image `rust:1.82-slim-bookworm` → `rust:1.85-slim-bookworm`
    (see §4.1).
  - Installed `protobuf-compiler` via apt (required by `tonic-build`).
  - Added `COPY proto ./proto` so the `build.rs` resolver finds the file.
- `py-builder`:
  - Added `COPY proto /opt/ashpy/proto` and set
    `ENV ASH_PROTO_DIR=/opt/ashpy/proto`.
  - After `uv sync --frozen --extra dev`, runs
    `python -c "from ashpy import _codegen; _codegen.generate(force=True)"`
    so generated stubs are baked into the venv and runtime startup is
    filesystem-free.
- `runtime`:
  - Inherits the pre-generated stubs + proto directory.
  - Sets `ASH_PROTO_DIR=/opt/ashpy/proto` so ad-hoc regeneration remains
    possible inside a running container.

## 3. Verification

| Check | Environment | Result |
|---|---|---|
| Rust `cargo test --workspace` | `rust:1.85-slim-bookworm` + `protobuf-compiler` | **10 passed, 0 failed** — previous 7 tests from M0 + 3 new ipc tests (`default_endpoint_loopback`, `generated_types_present`, `health_ping_roundtrip`) |
| Python `uv run pytest -q` | `python:3.12-slim-bookworm` + `uv 0.11.6` | **8 passed in 0.15s** — M0 smoke 4 + M1 gRPC 4 |
| `docker compose build ash-code` | host Docker + buildx | ✅ rust-builder, py-builder, runtime all succeed; `ash-code:dev` published |
| `docker compose up -d ash-code` + `docker exec ash-code ash doctor --check-sidecar` | built image | ✅ `sidecar: OK — ashpy/0.0.1 api=v1 features=6 (1.2 ms)`, exit = 0 |

Sample E2E output:

```
$ docker logs ash-code
2026-04-09 12:23:29 INFO supervisord started with pid 1
2026-04-09 12:23:30 INFO spawned: 'ashpy' with pid 7
[ashpy] ashpy gRPC server listening on 127.0.0.1:50051
2026-04-09 12:23:31 INFO success: ashpy entered RUNNING state

$ docker exec ash-code ash doctor --check-sidecar
ash 0.0.1
  ash-core  0.0.1
  ash-api   port=8080
  ash-ipc   sidecar=http://127.0.0.1:50051
  sidecar: OK — ashpy/0.0.1 api=v1 features=6 (1.2 ms)
```

## 4. Issues encountered and resolved

### 4.1 MSRV bump Rust 1.82 → 1.85
Pulling in `tonic-build 0.12` transitively dragged in `getrandom 0.4.2`, which
requires the `edition2024` Cargo feature. That feature stabilized in Rust 1.85.
Resolution: bumped the `rust-builder` base image and the ad-hoc test container
to `rust:1.85-slim-bookworm`. The `clap = "=4.5.20"` pin from M0 remains in
place for now; revisit at M9 when CI lands.

### 4.2 Python generated-module import path
`grpc_tools.protoc` emits `ash_pb2_grpc.py` with `import ash_pb2 as ash__pb2`,
which only works if the generated directory is itself on `sys.path`. That
conflicts with shipping the stubs as a proper sub-package
(`ashpy._generated`). Resolution: `_codegen.py` post-processes the generated
file, rewriting the import to `from . import ash_pb2 as ash__pb2`. This keeps
the package fully self-contained and import-order independent.

### 4.3 Build-time vs. runtime stub generation
Running `protoc` at process start would leak build-time dependencies into
every fresh container. Resolution: the Dockerfile executes
`_codegen.generate(force=True)` once during the py-builder stage, so the
runtime venv already contains the compiled stubs and the sidecar cold-starts
in <100 ms.

### 4.4 `ash-serve` still stub → supervisord restart loop
`ash-serve` (M4 territory) continues to print "not yet implemented" and exit,
which supervisord interprets as a crash and restarts in a tight loop. This is
expected for M1 and will be resolved naturally when M4 implements the real
axum server. Not in scope here.

## 5. Decisions carried forward

1. **Generated gRPC stubs are not committed.** They are produced at image
   build time (Docker) and test time (local `uv run pytest`). This avoids
   drift between `ash.proto` and the checked-in Python/Rust artifacts.
2. **`PingResponse.features` convention.** `"v1"` = live, `"planned"` =
   contract exists but RPC returns `UNIMPLEMENTED`. M2–M6 each promote their
   feature key from `planned` to `v1` as they light up.
3. **Sidecar bound to `127.0.0.1:50051` only.** Never exposed outside the
   container. Only port 8080 (HTTP API, M4) is published.
4. **`SidecarClient` is the single Rust entry point to the sidecar.** New
   RPC wrappers get added to this struct rather than spreading raw tonic
   clients across crates.

## 6. Exit criteria — met

- [x] `proto/ash.proto` finalized and user-approved
- [x] Rust `tonic` client + in-process server test green
- [x] Python `grpcio` server with real `Health.Ping` implementation
- [x] All six services registered; placeholder RPCs return
      `UNIMPLEMENTED` with owning-milestone messages
- [x] `docker compose build` succeeds with `protobuf-compiler` installed
- [x] E2E: `docker exec ash-code ash doctor --check-sidecar` returns `OK`
- [x] Unit + integration tests: Rust 10/10, Python 8/8

## 7. Changed files

**Modified**
- `Cargo.toml` (workspace deps: tonic, prost, tokio-stream, async-stream)
- `crates/cli/Cargo.toml`, `crates/cli/src/main.rs`
- `crates/ipc/Cargo.toml`, `crates/ipc/src/lib.rs`
- `proto/ash.proto`
- `ashpy/src/ashpy/server.py`
- `docker/Dockerfile`

**Added**
- `crates/ipc/build.rs`
- `ashpy/src/ashpy/_codegen.py`
- `ashpy/tests/test_grpc.py`
- `docs/task/M1_TASK_REPORT.md` (this file)

## 8. Next: M2 — LLM Providers

- Define `LlmProvider` / `ProviderConfig` / `ProviderCaps` ABCs in
  `ashpy/src/ashpy/providers/base.py`.
- Implement the four built-in adapters (`anthropic`, `openai`, `vllm`,
  `ollama`) against the plugin contract defined in `docs/extensibility.md`.
  Built-ins use the same loader path a third-party plugin would.
- Implement a `providers/<name>.toml` loader + `ASH_LLM_PROVIDER`/
  `ASH_LLM_MODEL` env handling.
- Wire the `LlmProvider.ChatStream` gRPC RPC to the selected provider,
  promoting the `features.llm` key from `planned` to `v1`.
- Tests: per-provider unit tests against mock HTTP servers + a streaming
  gRPC integration test that drives `SidecarClient` (once M1's wrapper is
  extended).
