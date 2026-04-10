# M3 Task Report — Core Turn Loop + Built-in Tools + Harness Hooks

**Milestone:** M3 (Agent turn loop, 6 built-in tools, Harness middleware chain)
**Date:** 2026-04-09
**Status:** ✅ Completed and awaiting approval for M4

---

## 1. Goals

Weave the pieces from M0–M2 into a real agent turn. After M3 the system can:

- Receive a user prompt, call the LLM through the sidecar,
- Dispatch tool calls the model requests against Rust-native built-in tools,
- Feed tool results back to the next turn until the model returns text,
- Run every hook point through a Python middleware chain that can
  `ALLOW` / `DENY` / `REWRITE` the request,
- Survive infinite loops via a `max_turns` guard,
- Stay safe via a two-layer defense (Rust last-resort blacklist + Python
  flexible policy).

And it must do all of this **without any LLM credentials**, by virtue of the
`fake` provider introduced for E2E tests.

## 2. Deliverables

### 2.1 Rust — `crates/tools`

Six built-in tools, each implementing the async `Tool` trait:

| Tool | File | Behavior |
|---|---|---|
| `bash` | `bash.rs` | `tokio::process::Command` + timeout, catastrophic-pattern blacklist |
| `file_read` | `file_read.rs` | UTF-8 check, 2MB limit, `offset`/`limit` slicing |
| `file_write` | `file_write.rs` | Auto-mkdir, atomic rename |
| `file_edit` | `file_edit.rs` | Exact string replace, refuses ambiguous matches unless `replace_all=true` |
| `grep` | `grep.rs` | Pure-Rust `regex` + `walkdir` (no shell-out to `rg`) |
| `glob` | `glob_tool.rs` | `globset` + `walkdir` |

`ToolRegistry`:
- `with_builtins()` registers all six.
- `list()` returns sorted `ToolSpec` with JSON Schema input.
- `invoke(name, args)` dispatches and returns `ToolResult { ok, stdout, stderr, exit_code }`.
- Unknown tool name → `anyhow!("unknown tool: ...")`.

Constants:
- `CATASTROPHIC_BASH_PATTERNS` — 7 regex patterns (`rm -rf /`, fork bomb,
  `mkfs.*`, `dd if=/dev/zero of=/dev/*`, `> /dev/sda`, `chmod -R 000 /`).
  These are blocked by the `bash` tool itself **regardless of middleware
  state** — last-resort safety net.

### 2.2 Rust — `crates/query`

`QueryEngine::run_turn(&mut Session, &mut dyn TurnSink)` implements the
turn loop:

```text
loop {
  Harness.OnTurnStart(ctx) → DENY halts everything
  backend.chat_stream(ChatRequest)
  for delta in stream:
    text      → sink.on_text + accumulate
    tool_call → (remember for post-stream)
    finish    → record stop_reason / tokens
  session.push_assistant_text(…)
  Harness.OnTurnEnd(result)
  if tool_call:
    Harness.OnToolCall → DENY → append denial as tool_result, loop
    else                  → ToolRegistry.invoke → append result, loop
  else:
    break
}
```

Key types:

- `Session { id, provider, model, messages }` — mutable transcript.
- `ChatMessage { role, content, tool_call_id }` with `role ∈ {"user",
  "assistant", "tool"}`.
- `TurnSink` trait with `on_text / on_tool_call / on_tool_result /
  on_finish / on_error` — M3 ships `NullSink` (tests) and `StdoutSink`
  (temporary CLI).
- `QueryBackend` trait decouples the engine from `ash-ipc::SidecarClient`
  so unit tests can swap in a scripted mock. `SidecarBackend` wraps the
  real client and implements `QueryBackend`.
- `DEFAULT_MAX_TURNS = 10`, overridable via `ASH_MAX_TURNS` env var
  (constant `ENV_MAX_TURNS = "ASH_MAX_TURNS"`).

### 2.3 Rust — `crates/ipc`

Added four `SidecarClient` methods wrapping the `Harness` service:

- `on_turn_start(TurnContext) -> HookDecision`
- `on_tool_call(ToolCallEvent) -> HookDecision`
- `on_stream_delta(DeltaEvent) -> ()`
- `on_turn_end(TurnResult) -> ()`

No new trait; they're concrete methods `SidecarBackend` delegates to.

### 2.4 Python — `ashpy/src/ashpy/middleware/`

Eight new files implementing the harness control plane:

| File | Contents |
|---|---|
| `__init__.py` | Re-exports of the public surface |
| `base.py` | `Middleware` ABC, `DecisionKind{ALLOW, DENY, REWRITE}`, `HookDecision`, `TurnContext`, `ToolCallEvent`, `TurnResult`, `MiddlewareChain` |
| `loader.py` | `MiddlewareLoader.discover()` walks `/root/.ash/middleware/*.py` + `build_default_chain()` convenience |
| `logging_middleware.py` | Built-in `priority=10` observability middleware — emits every hook as JSON on stderr |
| `bash_guard_middleware.py` | Built-in `priority=50` policy middleware — blocks `sudo`, `su -`, `curl|sh`, writes to `/etc|/boot|/sys|/proc`, `rm -rf /etc`, plus the catastrophic list (defensive duplicate of the Rust layer) |

`MiddlewareChain`:
- `add(mw)` inserts and re-sorts by `(priority, name)`.
- `on_turn_start` / `on_tool_call` — walk the chain, **first non-ALLOW short-circuits**.
- `on_stream_delta` / `on_turn_end` — iterate every middleware (observability, no short-circuit).

### 2.5 Python — `ashpy/src/ashpy/server.py`

- `HarnessServicer` is **fully live**: all four RPCs call the global
  `MiddlewareChain` (built once via `build_default_chain()`), convert
  results via `_decision_to_pb`, and return real `HookDecision` /
  `Empty` responses.
- `get_middleware_chain()` / `reset_middleware_chain_for_tests()` singleton
  helpers.
- `features.harness` flipped from `"planned"` → `"v1"` in `Health.Ping`.
- `LlmProviderServicer.ChatStream` detects the `fake` provider's
  `_fake_tool_call` sentinel attribute and emits a real protobuf
  `ToolCall` delta — no code path changes for other providers.
- `build_server` writes a startup log line with the loaded middleware names.

### 2.6 Python — `ashpy/src/ashpy/providers/fake_p.py`

In-process deterministic test provider. Supports two modes:

- **Echo**: any text prompt is streamed back character-by-character.
- **Tool-use**: a prompt beginning with `tool:<name> key=val key2="val"`
  emits a single `ToolCall(name, args)` delta followed by
  `finish(stop_reason=tool_use)`.
- **Second turn**: when the last message role is `tool`, the provider
  summarizes the tool result as text and finishes.

Registered as a first-class built-in in `providers/loader.py` with the
same plugin contract as anthropic/openai/vllm/ollama.

### 2.7 Rust — `crates/cli`

`ash llm chat "<prompt>"` now routes through `QueryEngine::run_turn`
instead of calling `SidecarClient.chat_stream` directly. A small
`StdoutSink` struct implements `TurnSink` for incremental terminal
output. **This subcommand is still M2/M3 scaffolding and must be removed
in M7** (tracked in persistent memory).

## 3. Verification

| Check | Command | Result |
|---|---|---|
| Rust workspace tests | `cargo test --workspace` | **32 passed, 0 failed** (tools 17 + query 4 + ipc 3 + core 2 + 6 misc stubs) |
| Python tests | `uv run pytest -q` | **52 passed in 0.13s** (M0/1/2: 27, M3 middleware: 14, M3 harness server: 5, updated legacy: 6) |
| Docker image rebuild | `docker compose build ash-code` | ✅ `ash-code:dev` |
| E2E A — doctor | `ash doctor --check-sidecar` | `sidecar: OK — ashpy/0.0.1 api=v1 features=6 (1.3 ms)` |
| E2E B — features.harness | Direct `Health.Ping` | `features.harness == "v1"` |
| E2E C — fake echo | `ash llm chat "hello ash"` (`ASH_LLM_PROVIDER=fake`) | Streams `hello ash` character-by-character then `[finish stop_reason=end_turn]` |
| E2E D — tool_use round-trip | `ash llm chat 'tool:file_write path=/tmp/ash_e2e.txt content=m3-works'` | Turn 1: `[tool_call file_write]` → `[tool_result … ok=true]`. Turn 2: `done: {…}` summary then `[finish stop_reason=end_turn]`. `engine turns=2` |
| E2E E — file actually written | `cat /tmp/ash_e2e.txt` | `m3-works` |
| E2E F — bash_guard denial | `ash llm chat 'tool:bash command="rm -rf /"'` | `[error] tool call 'bash' denied by harness: bash_guard: blocked by policy (\brm\s+-rf\s+/($\|\s))`. Engine continues to next turn, finishes gracefully |

### 3.1 E2E D output (trimmed)

```
[tool_call file_write] {"path": "/tmp/ash_e2e.txt", "content": "m3-works"}
[finish stop_reason=tool_use in=1 out=0]
[tool_result file_write ok=true exit=0] wrote 8 bytes to /tmp/ash_e2e.txt
done: {"ok":true,"stdout":"wrote 8 bytes to /tmp/ash_e2e.txt","stderr":"","exit_code":0}
[finish stop_reason=end_turn in=1 out=88]
[engine turns=2 stop_reason=end_turn]
```

Two-turn loop, real Rust tool executed between turns, second turn driven
back to the LLM with the tool result.

### 3.2 E2E F output (bash_guard)

```
[tool_call bash] {"command": "rm -rf /"}
[finish stop_reason=tool_use in=1 out=0]
[error] tool call 'bash' denied by harness: bash_guard: blocked by policy (\brm\s+-rf\s+/($|\s))
```

The denial message includes the exact regex pattern that matched, which
makes audit and tuning straightforward.

## 4. Issues encountered and resolved

1. **`crates/query` missing `tonic` dependency.** The `QueryBackend`
   trait uses `tonic::Streaming` / `tonic::Status`. Fix: added
   `tonic.workspace = true`.
2. **Scripted-backend test assertion too strict.** The `tool_denied_by_harness`
   test initially expected `stop_reason == "max_turns"`, but the engine
   naturally exits once the scripted queue is drained. Relaxed the assertion
   to check for a `tool` role message containing `"denied"`.
3. **Duplicate `ash-query = { path = ... }` workspace entry** after the
   cli edit appended a second line. Removed the duplicate.
4. **Old tests incompatible with M3 flags.** `test_grpc.py::test_health_ping`
   asserted `features.harness == "planned"`, and
   `test_harness_still_unimplemented` expected `UNIMPLEMENTED`. Updated:
   harness assertion → `"v1"`, second test retargeted to `SkillRegistry`
   (still `UNIMPLEMENTED` until M5).
5. **Provider registry ordering changed** when `fake` joined the built-ins.
   Updated `test_registry_lists_all_builtins_without_env` to expect five
   built-ins in sorted order.
6. **Two `unused_assignments` warnings** on `stop_reason` in the query loop
   — cosmetic, kept as-is (value is reassigned in the next iteration; not
   worth restructuring to silence).

## 5. Decisions carried forward

1. **Two-layer bash defense**. Rust blocks catastrophes unconditionally;
   Python `bash_guard` middleware implements configurable policy. Users
   override by editing/replacing the middleware file, never the Rust tool.
2. **`QueryBackend` trait lives in `crates/query`.** Allows mock backends
   without touching `ash-ipc`. `SidecarBackend` is a thin adapter.
3. **`max_turns = 10`, env-override `ASH_MAX_TURNS`.** Matches the general
   Claude Code / claurst convention.
4. **Tool call → tool_result serialization** uses a JSON dump of
   `ToolResult` as the `content` field of the `tool`-role message. This
   is sufficient for the fake provider roundtrip; anthropic/openai
   mappings in M2 already translate `tool`-role messages through their
   SDKs, and M3+ will refine the shape.
5. **`OnStreamDelta` hook is deliberately not wired into the Rust loop yet.**
   The Python chain has the plumbing, but the Rust side does not yet
   call it per-delta — doing so would quadruple per-turn gRPC round-trips.
   Deferred optimization will decide whether to batch or sample.
6. **`ToolRegistry` is authoritative on the Rust side.** Python
   `ToolRegistry` servicer stays `UNIMPLEMENTED`; it becomes useful only
   when we want to expose *Python-defined* third-party tools (M3+ or later).

## 6. Exit criteria — met

- [x] `crates/query` turn loop implemented with mock-backed unit tests.
- [x] Six built-in tools implemented and unit-tested.
- [x] Harness 4 hooks wired through Python middleware chain with
      ALLOW/DENY support.
- [x] Two built-in middleware (`logging`, `bash_guard`) shipped and tested.
- [x] `features.harness == "v1"`.
- [x] `ash llm chat` fake-provider E2E runs a real tool round-trip.
- [x] `bash_guard` blocks `rm -rf /` end-to-end.
- [x] Rust `cargo test` 32/32, Python `pytest` 52/52.
- [x] Graceful behavior with no LLM credentials set (fake provider works
      standalone; real providers still degrade per M2 contract).

## 7. Changed files

**Added (Rust)**
- `crates/tools/src/bash.rs`
- `crates/tools/src/file_read.rs`
- `crates/tools/src/file_write.rs`
- `crates/tools/src/file_edit.rs`
- `crates/tools/src/grep.rs`
- `crates/tools/src/glob_tool.rs`

**Modified (Rust)**
- `Cargo.toml` — workspace deps: async-trait, futures, schemars, walkdir,
  globset, tempfile, regex
- `crates/tools/{Cargo.toml, src/lib.rs}`
- `crates/query/{Cargo.toml, src/lib.rs}` — turn loop + mock tests
- `crates/ipc/src/lib.rs` — four Harness wrappers
- `crates/cli/{Cargo.toml, src/main.rs}` — route `ash llm chat` through
  `QueryEngine`, add `StdoutSink`

**Added (Python)**
- `ashpy/src/ashpy/middleware/__init__.py`
- `ashpy/src/ashpy/middleware/base.py`
- `ashpy/src/ashpy/middleware/loader.py`
- `ashpy/src/ashpy/middleware/logging_middleware.py`
- `ashpy/src/ashpy/middleware/bash_guard_middleware.py`
- `ashpy/src/ashpy/providers/fake_p.py`
- `ashpy/tests/test_middleware.py`
- `ashpy/tests/test_harness_server.py`

**Modified (Python)**
- `ashpy/src/ashpy/server.py` — real `HarnessServicer`, `features.harness`
  → `"v1"`, fake tool-call sentinel handling
- `ashpy/src/ashpy/providers/loader.py` — `fake` registered as a built-in
  spec
- `ashpy/tests/test_grpc.py` — updated `features.harness` assertion and
  retargeted unimplemented-service check to `SkillRegistry`
- `ashpy/tests/test_providers.py` — registry assertion now expects five
  built-ins

**Docs**
- `docs/task/M3_TASK_REPORT.md` (this file)

## 8. Post-completion revisions (2026-04-09, same day)

After the initial M3 sign-off the user asked to validate the pipeline against
real Anthropic and OpenAI APIs. Several bugs surfaced and were fixed in-place.
The sections above describe the state at initial completion; this section
records what changed afterward so the report reflects the current tree.

### 8.1 `fake` provider removed entirely

Per user direction, the built-in `fake` provider was deleted and real
providers became the single source of truth for runtime testing.

- **Deleted**: `ashpy/src/ashpy/providers/fake_p.py`.
- **Removed from** `providers/loader.py::_BUILTIN_SPECS` and
  `_builtin_defaults`. Built-in catalog is back to four: `anthropic`,
  `openai`, `vllm`, `ollama`.
- **Removed from** `ashpy/src/ashpy/server.py`: the
  `_fake_tool_call` sentinel branch in `LlmProviderServicer.ChatStream`
  is now dead code and was stripped.
- **Test refactor**: `ashpy/tests/test_harness_server.py` no longer
  imports `FakeProvider`. An inline `EchoProvider` is defined inside the
  test module (mirroring the existing `FakeEchoProvider` pattern in
  `test_grpc.py`) so harness tests remain hermetic without shipping a
  production-facing fake.
- **Test assertion**: `test_registry_lists_all_builtins_without_env`
  reverted to expecting `["anthropic", "ollama", "openai", "vllm"]`.
- **E2E consequence**: the original "tool-use round-trip" E2E scenario
  (§3.1) relied on the fake provider's `tool:<name>` directive and is no
  longer reproducible without implementing tool-use SDK mapping for a
  real provider. Rust-side tool-use integration is still covered by the
  `ScriptedBackend` unit tests in `crates/query`, which remain green.
  Enabling tool-use against anthropic/openai is the Q1 "필요시 채워넣기"
  deferred work.

### 8.2 OpenAI token usage now surfaced

`openai_p.py` was emitting `input_tokens=0, output_tokens=0` because
streaming responses only include a `usage` payload when
`stream_options={"include_usage": True}` is passed, AND that usage chunk
arrives with an empty `choices` array (so the `if not chunk.choices:
continue` guard was skipping it). Both issues fixed:

```python
stream = await client.chat.completions.create(
    ...
    stream=True,
    stream_options={"include_usage": True},   # (1) request usage
)
async for chunk in stream:
    usage = getattr(chunk, "usage", None)
    if usage:                                 # (2) handle BEFORE the choices guard
        input_tokens = getattr(usage, "prompt_tokens", 0) or input_tokens
        output_tokens = getattr(usage, "completion_tokens", 0) or output_tokens
    if not chunk.choices:
        continue
    ...
```

### 8.3 CLI default provider no longer hard-coded to `fake`

`crates/cli/src/main.rs::run_llm` used
`provider.unwrap_or_else(|| "fake".to_string())`. With the fake provider
gone, and to honor the M0.5 externalization principle, it now resolves
in this order:

1. Explicit `--provider` flag on the CLI.
2. `ASH_LLM_PROVIDER` environment variable.
3. Empty string — the sidecar falls back to its own active provider
   (set by `ASH_LLM_PROVIDER` at sidecar startup, default `anthropic`
   from `docker-compose.yml`).

### 8.4 gRPC per-RPC timeout removed

`ash_ipc::SidecarClient::connect` was setting both
`connect_timeout(3s)` and `timeout(3s)`, so *every* RPC, including the
server-streaming `ChatStream`, inherited a 3-second deadline. Real LLM
streams can take tens of seconds, producing:

```
status: Cancelled, message: "Timeout expired"
```

Fix: split the two concepts. `connect_timeout` stays at 3 s (we should
not wait forever on a missing sidecar), and the per-RPC `timeout()` was
removed entirely. Callers that want a deadline on a specific RPC can
attach one via `tonic::Request::set_timeout`.

### 8.5 Empty-string env scrub in sidecar

`docker-compose.yml` uses `${VAR:-}` which injects **empty-string**
environment variables when the host has not set them. The `anthropic`
and `openai` SDKs read `ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL` directly
from `os.environ` rather than honoring a `None` kwarg, and interpret
`""` as a literal base URL, producing:

```
httpx.UnsupportedProtocol: Request URL is missing an 'http://' or 'https://' protocol.
```

Fix: `ashpy/src/ashpy/server.py` now calls `_scrub_empty_env()` at
import time, deleting the following keys from `os.environ` when they
exist with an empty value:

```
ANTHROPIC_BASE_URL, OPENAI_BASE_URL,
VLLM_BASE_URL, VLLM_API_KEY,
OLLAMA_BASE_URL, ASH_LLM_MODEL
```

This is a one-shot startup scrub; later SDK imports then behave as if
the variable was never set.

### 8.6 Real-provider E2E verification (final)

With the above in place, running against the user-supplied real keys:

```
$ docker exec ash-code ash llm chat --model claude-opus-4-5 \
    "Say exactly: hello from anthropic in 5 words"
hello from anthropic in five
[finish stop_reason=end_turn in=19 out=9]
[engine turns=1 stop_reason=end_turn]

$ docker exec ash-code ash llm chat --provider openai \
    "Say exactly: hello from openai in 5 words"
hello from openai in 5 words
[finish stop_reason=stop in=18 out=8]
[engine turns=1 stop_reason=stop]
```

Both providers now stream through the full pipeline — `ash-cli` → tonic
→ `QueryEngine::run_turn` → `SidecarBackend` → gRPC →
`LlmProviderServicer.ChatStream` → provider SDK → real HTTPS → real
tokens surfaced back into `TurnFinish`.

### 8.7 Test counts after revisions

- Python: **51 passed** (was 52 before removing the `test_fake_provider_emits_tool_call_via_chat_stream` test).
- Rust: **32 passed** (unchanged).

### 8.8 Files changed in this revision pass

**Deleted**
- `ashpy/src/ashpy/providers/fake_p.py`

**Modified**
- `ashpy/src/ashpy/providers/loader.py` — remove `fake` from built-in specs and defaults
- `ashpy/src/ashpy/providers/openai_p.py` — `stream_options.include_usage` + usage before choices guard
- `ashpy/src/ashpy/server.py` — `_scrub_empty_env()` at import time; `_fake_tool_call` branch removed
- `ashpy/tests/test_harness_server.py` — inline `EchoProvider`, no `fake_p` import
- `ashpy/tests/test_providers.py` — registry assertion back to 4 built-ins
- `crates/ipc/src/lib.rs` — `connect()` no longer sets per-RPC `timeout`
- `crates/cli/src/main.rs` — `--provider` → `ASH_LLM_PROVIDER` env → empty fallback

### 8.9 Known unresolved user-side items (documented, not fixed)

1. `.env` may carry `ASH_LLM_MODEL=opus-4.6`, which is **not** a valid
   Anthropic model identifier (the correct form is `claude-opus-4-6`).
   This override is the reason `ash llm chat` without `--model` returns
   a 404 against Anthropic. Recommended fix: either unset
   `ASH_LLM_MODEL` in `.env` (let the provider fall back to
   `claude-opus-4-6`) or write the full slug.
2. `COMPOSE_PROJECT_NAME` must be lowercase for docker compose; the
   user's initial `.env` had `Ash-Code` which compose rejects. Current
   `.env` is already `ash-code`, confirmed working.

---

## 9. Next: M4 — HTTP API + Swagger UI

- Implement `crates/api` with `axum` + `utoipa`-generated OpenAPI spec.
- Endpoints: `GET /v1/sessions`, `POST /v1/sessions`, `POST /v1/chat`
  (SSE), `GET /v1/llm/providers`, `POST /v1/llm/switch`, `GET /v1/health`.
- Wire `ash serve` to actually serve traffic on `0.0.0.0:8080`, resolving
  the current supervisord restart-loop on that program.
- Swagger UI at `/docs`, reachable from the host at
  `http://localhost:8080/docs`.
- Tests: axum integration tests + an E2E curl against the running
  container.
