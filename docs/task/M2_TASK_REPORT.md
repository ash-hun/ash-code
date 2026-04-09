# M2 Task Report — LLM Providers

**Milestone:** M2 (LLM Provider plugin contract + 4 built-in adapters)
**Date:** 2026-04-09
**Status:** ✅ Completed and awaiting approval for M3

---

## 1. Goals

Turn the `LlmProvider.ChatStream` gRPC from a `UNIMPLEMENTED` placeholder into
a real streaming path driven by pluggable provider adapters. Deliver the four
built-in providers (`anthropic`, `openai`, `vllm`, `ollama`) through the same
plugin contract a third party would use (per M0.5 extensibility design).
Model selection must be fully externalized — env vars and `providers/*.toml`
drops, no code edits required to swap models. The sidecar must stay healthy
even when none of the provider credentials are set.

## 2. Deliverables

### 2.1 Provider contract — `ashpy/src/ashpy/providers/base.py`

- `LlmProvider` ABC with `capabilities() / chat_stream() / health()`.
- Dataclasses: `ProviderCaps`, `ProviderConfig`, `ChatMessage`, `ChatRequest`,
  `ChatDelta`, `HealthStatus`, `HealthState`.
- `ProviderNotConfigured` error + `unconfigured_stream()` helper so every
  adapter degrades the same way when credentials are missing.
- `ChatDelta` carries `text`, `finish_reason/input_tokens/output_tokens`, or
  `error` — `tool_call` translation is deferred to M3 per the M2 plan.

### 2.2 Registry + loader — `ashpy/src/ashpy/providers/loader.py`

Discovery order (later wins):
1. Built-in module specs in `_BUILTIN_SPECS` (`ashpy.providers.*_p`).
2. `providers/*.toml` in the mounted volume. TOML files may override the
   built-in `[defaults]`/`[auth]` blocks or point `[provider].module` at a
   completely new Python module for third-party plugins.

Selection order for the active provider:
1. `ProviderRegistry.switch(name, model)` (runtime — API/TUI will call this).
2. `ASH_LLM_PROVIDER` env var, with optional `ASH_LLM_MODEL` overriding the
   default model on the active provider.
3. Hard-coded fallback `anthropic` — deliberately never crashes if nothing
   is configured.

`get_registry()` is a process-wide singleton; `reset_registry_for_tests()`
is the test entry point.

### 2.3 Built-in provider adapters

| File | Class | SDK | Auth envs |
|---|---|---|---|
| `providers/anthropic_p.py` | `AnthropicProvider` | `anthropic.AsyncAnthropic` + `messages.stream()` | `ANTHROPIC_API_KEY`, `ANTHROPIC_BASE_URL` |
| `providers/openai_p.py` | `OpenAIProvider` | `openai.AsyncOpenAI` + `chat.completions.create(stream=True)` | `OPENAI_API_KEY`, `OPENAI_BASE_URL` |
| `providers/vllm_p.py` | `VllmProvider` (subclasses `OpenAIProvider`) | same OpenAI SDK against `VLLM_BASE_URL` | `VLLM_API_KEY`, `VLLM_BASE_URL` |
| `providers/ollama_p.py` | `OllamaProvider` | `ollama.AsyncClient.chat(stream=True)` | `OLLAMA_BASE_URL` |

Each adapter lazily imports its SDK so the cold-start cost stays low, and each
reads its auth envs via `config.auth` (populated either from built-in defaults
or the volume TOML). **Graceful degradation is uniform**: missing envs →
`health()` returns `HealthState.UNCONFIGURED` → `chat_stream()` yields a
single error delta plus a `finish_reason="error"` delta.

### 2.4 Sidecar migration to `grpc.aio`

- `ashpy/src/ashpy/server.py` rewritten around `grpc.aio.server()`.
- All servicer methods are now `async def` and use `await context.abort(...)`
  for `UNIMPLEMENTED` returns.
- `serve()` drives a single event loop via `asyncio.run(_serve_async(bind))`.
- SIGTERM/SIGINT handlers set an `asyncio.Event`; the main coroutine awaits
  it and then awaits `server.stop(grace=2.0)` for a clean shutdown.
- `LlmProviderServicer` is **fully implemented** and promotes
  `Health.Ping.features.llm` from `"planned"` to `"v1"`.
- Placeholder servicers (`SkillRegistry`, `CommandRegistry`, `Harness`,
  `ToolRegistry`) migrated to `async def` stubs that `abort` with
  `UNIMPLEMENTED` — connection still succeeds, only the RPC is gated.

### 2.5 Rust `ash-ipc` extensions

New methods on `SidecarClient`:

- `list_providers() -> Vec<ProviderInfo>`
- `switch_provider(provider, model) -> SwitchResponse`
- `chat_stream(ChatRequest) -> tonic::Streaming<ChatDelta>`

### 2.6 Temporary CLI — `ash llm …` (removed in M7)

- `ash llm list` — pretty-prints the provider catalog via `ListProviders`.
- `ash llm chat "<prompt>" [--provider X] [--model Y] [--temperature F]` —
  opens a `ChatStream`, prints text deltas incrementally, then a terminal
  `[finish stop_reason=… in=… out=…]` line.

This subcommand exists **only** to verify the gRPC streaming path end-to-end
without the M7 TUI. **M7 will remove it.** Tracked in persistent memory
(`project_m7_cleanup_llm_chat.md`) so the cleanup is not lost.

### 2.7 Design documentation

`docs/comparison_grpcio_grpcaio.md` compares the two gRPC programming models
side by side and records why ash-code picked `grpc.aio` in M2 (native fit
with async SDKs, single event loop, clean cancellation).

## 3. Verification

| Check | Command | Result |
|---|---|---|
| Rust workspace tests | `cargo test --workspace` (rust:1.85 + protoc) | **10 passed, 0 failed** (unchanged from M1; ipc extensions compile and the existing `health_ping_roundtrip` still passes) |
| Python tests | `uv run pytest -q` | **27 passed in 0.11s** (M0 smoke 4 + M1 gRPC 6 + M2 provider-registry 5 + M2 per-provider unconfigured/capabilities/happy-path 12) |
| Docker image rebuild | `docker compose build ash-code` | ✅ `ash-code:dev` |
| E2E 1 — doctor | `ash doctor --check-sidecar` | ✅ `sidecar: OK — ashpy/0.0.1 api=v1 features=6 (1.0 ms)` |
| E2E 2 — list providers | `ash llm list` | All four built-ins present with correct `default_model` / `tools` / `vision` / `source=builtin` |
| E2E 3 — graceful degradation | `ash llm chat "hello world"` with **no env vars set** | Prints `[error] provider 'anthropic' is not configured; set ANTHROPIC_API_KEY ...` followed by `[finish stop_reason=error in=0 out=0]`. Sidecar stays up. |
| E2E 4 — feature flag promotion | Direct gRPC `Health.Ping` | `features.llm == "v1"` |

### 3.1 `ash llm list` output (no env vars set)

```
name         default_model                    tools    vision   source
anthropic    claude-opus-4-6                  yes      yes      builtin
ollama       llama3.1                         no       no       builtin
openai       gpt-4.1-mini                     yes      yes      builtin
vllm                                          no       no       builtin
```

### 3.2 Graceful degradation proof

```
$ docker exec ash-code ash llm chat "hello world"
[error] provider 'anthropic' is not configured; set ANTHROPIC_API_KEY in the environment or providers/<name>.toml
[finish stop_reason=error in=0 out=0]
```

No crash, no exception, no stream hang. Sidecar keeps serving other RPCs.

## 4. Issues encountered and resolved

1. **`HealthState` missing from `ashpy.providers` re-exports.** Test import
   failed. Added to `__init__.py` `__all__`.
2. **`pytest-asyncio` strict-mode fixture rejection.** The initial
   `@pytest.fixture` on an async server fixture raised
   `PytestRemovedIn9Warning`. Fixed by (a) setting
   `asyncio_mode = "auto"` in `[tool.pytest.ini_options]` and
   (b) switching the async fixture to `@pytest_asyncio.fixture`.
3. **`tokio_stream::StreamExt` missing in `ash-cli`.** Adding the
   `chat_stream` CLI required `StreamExt::next()`. Added
   `tokio-stream.workspace = true` to `crates/cli/Cargo.toml`.
4. **vLLM requires base URL but not API key.** The OpenAI SDK rejects an
   empty `api_key`. Resolution: `VllmProvider._ensure_client` substitutes
   `sk-vllm-unauthenticated` when `VLLM_API_KEY` is unset, matching what
   local vLLM deployments typically expect.

## 5. Decisions carried forward

1. **All four built-in providers go through the plugin contract.** No
   special-casing anywhere. A third-party plugin is indistinguishable from
   a built-in at the registry level.
2. **Model is data, not code.** `ASH_LLM_MODEL` env + `providers/<name>.toml`
   `[defaults].model` + runtime `switch(name, model)` — three orthogonal
   paths, all resolved by the registry.
3. **Missing credentials = registered + degraded, never crash.** This is a
   hard rule for every provider. Tests enforce it parametrically.
4. **`tool_use` translation deferred to M3.** M2 ships text streaming only;
   the query loop in M3 will map tool-call events across all four providers
   into the protobuf `ChatDelta.tool_call` oneof. Current `ChatDelta` text
   path is stable enough to build M3 on top of.
5. **`ash llm` subcommand is temporary.** Memory entry saved to enforce
   removal at M7.

## 6. Exit criteria — met

- [x] `LlmProvider.ChatStream` gRPC returns real streaming deltas.
- [x] Four built-in providers load through the plugin contract.
- [x] `Health.Ping.features.llm == "v1"`.
- [x] Sidecar stays healthy with zero env vars set (graceful degradation
      verified E2E).
- [x] `providers/*.toml` override path exists and is unit-tested.
- [x] `ASH_LLM_PROVIDER` / `ASH_LLM_MODEL` env routing unit-tested.
- [x] Rust `cargo test` 10/10, Python `pytest` 27/27.
- [x] Docker image rebuilds and container runs.
- [x] `grpc.aio` vs `grpcio` comparison doc written
      (`docs/comparison_grpcio_grpcaio.md`).

## 7. Changed files

**Added**
- `ashpy/src/ashpy/providers/__init__.py`
- `ashpy/src/ashpy/providers/base.py`
- `ashpy/src/ashpy/providers/loader.py`
- `ashpy/src/ashpy/providers/anthropic_p.py`
- `ashpy/src/ashpy/providers/openai_p.py`
- `ashpy/src/ashpy/providers/vllm_p.py`
- `ashpy/src/ashpy/providers/ollama_p.py`
- `ashpy/tests/test_providers.py`
- `docs/comparison_grpcio_grpcaio.md`
- `docs/task/M2_TASK_REPORT.md` (this file)

**Modified**
- `ashpy/src/ashpy/server.py` — full rewrite on `grpc.aio`, real
  `LlmProviderServicer`, async UNIMPLEMENTED stubs for other services
- `ashpy/tests/test_grpc.py` — async fixtures, fake echo provider, new
  tests for `ListProviders` / `ChatStream` / `Switch`
- `ashpy/pyproject.toml` — `[tool.pytest.ini_options] asyncio_mode = "auto"`
- `crates/ipc/src/lib.rs` — `list_providers`, `switch_provider`,
  `chat_stream`
- `crates/cli/Cargo.toml` — added `tokio-stream`
- `crates/cli/src/main.rs` — `ash llm list` / `ash llm chat` temporary
  subcommands

## 8. Next: M3 — Query loop + built-in tools + Harness hooks

- Implement `crates/query` turn loop that calls
  `SidecarClient.chat_stream` and dispatches tool-use events.
- Land the first six built-in tools in `crates/tools`:
  `bash`, `file_read`, `file_write`, `file_edit`, `grep`, `glob`.
- Wire `Harness` gRPC hooks (`OnTurnStart`/`OnToolCall`/`OnStreamDelta`/
  `OnTurnEnd`) against a Python middleware chain.
- Promote `features.harness` from `"planned"` to `"v1"`.
