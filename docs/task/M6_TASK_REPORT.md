# M6 Task Report — Commands System

**Milestone:** M6 (Commands loader + hot-reload + gRPC + FastAPI render/run)
**Date:** 2026-04-10
**Status:** ✅ Completed and awaiting approval for M7

---

## 1. Goals

Complete the **third and final pillar of the M0.5 customization
triangle**: commands. Drop a `commands/<name>.toml` file on the mounted
volume and the agent gains a named, parameterizable prompt that can be
both previewed (`render`) and executed end-to-end against the real LLM
(`run`, SSE). Promote `features.commands` from `"planned"` to `"v1"`.

## 2. Deliverables

### 2.1 Python `ashpy/src/ashpy/commands/` — 5 modules

| File | Contents |
|---|---|
| `__init__.py` | Re-exports of the public surface |
| `schema.py` | `CommandFile` (pydantic), `Command` (dataclass), `CommandEventKind` enum, `CommandEventPayload` |
| `loader.py` | `load_command_file(path)` + `load_command_dir(root)`. Uses `tomllib`. Enforces single-file layout; subdirectories and non-`.toml`/hidden files are ignored. Errors collected per-file so one broken file does not hide the rest. |
| `registry.py` | `CommandRegistry` — thread-safe, Jinja2 `SandboxedEnvironment` rendering, pub/sub subscriber queues (parity with skills, no Watch RPC yet). Singleton via `get_registry()`. |
| `watcher.py` | `CommandWatcher` — watchdog observer with 200 ms asyncio debounce. Shares the `ASH_SKILLS_POLLING` env var with the skills watcher so both subsystems flip to `PollingObserver` in the same breath. |

Layout (Q1=a): a command is a single TOML file at
`<root>/<name>.toml`. No recursion, no `command.toml`-in-dir.

### 2.2 `CommandRegistryServicer` — three RPCs live

`ashpy/src/ashpy/server.py`:

- `List` → `ListCommandsResponse` with full fields from the `Command`
  dataclass.
- `Run` → **render-only** (Q2=a says actual execution lives on the HTTP
  side). Returns `RunCommandResponse{rendered_prompt, allowed_tools}`.
  Unknown → `NOT_FOUND`; render error → `INVALID_ARGUMENT`.
- `Reload` → `ReloadResponse{loaded, errors}`.

`features.commands` in `Health.Ping` promoted from `"planned"` →
**`"v1"`** (gRPC). Same flip in `GET /v1/health`.

### 2.3 Command watcher wired into the sidecar lifecycle

`ashpy/src/ashpy/server.py::_serve_async` now instantiates both
`SkillWatcher` and `CommandWatcher`, starts them after the gRPC server,
and stops them in the SIGTERM path alongside `uvicorn`. A failed watcher
startup is logged but never crashes the sidecar.

### 2.4 FastAPI router `ashpy/src/ashpy/api/routes_commands.py`

Five endpoints under `/v1/commands`:

| Method | Path | Behavior |
|---|---|---|
| `GET` | `/v1/commands` | List commands |
| `GET` | `/v1/commands/{name}` | Full detail including the raw prompt body |
| `POST` | `/v1/commands/{name}/render` | Render template, return JSON |
| `POST` | `/v1/commands/{name}/run` | **Render + execute a turn**, stream as SSE |
| `POST` | `/v1/commands/reload` | Force rescan |

Pydantic models drive the schema: `CommandInfo`, `ListCommandsResponse`,
`CommandDetail`, `RenderCommandRequest/Response`, `RunCommandRequest`,
`ReloadCommandsResponse`. Registered in `/openapi.json` and `/docs`.

`POST /v1/commands/{name}/run` is the M6 headliner:

1. Look up the command, render the template.
2. Resolve the effective model:
   **request body `model` > command's `model` field > session default**.
3. Call `app.state.query_client.run_turn(...)` — the same
   `QueryHostClient` M4 introduced.
4. Wrap the async event iterator in `EventSourceResponse` and stream
   `text / tool_call / tool_result / finish / error / outcome / done`
   events to the caller.

### 2.5 Sample commands

Committed under `commands/`:

- `commands/review.toml` — staged-diff reviewer with `focus` arg and
  `claude-opus-4-5` override.
- `commands/summarize.toml` — structured file summarizer (requires
  `args.path`).
- `commands/test.toml` — detects the stack, runs tests, analyzes output.

All three load automatically at container startup.

### 2.6 `docs/commands.md`

End-user guide covering layout, TOML schema, template context, HTTP and
gRPC APIs, the `render` vs `run` distinction, model precedence,
hot-reload semantics (incl. the shared `ASH_SKILLS_POLLING` knob), error
handling, and M6 limits.

## 3. Verification

| Check | Result |
|---|---|
| Python `uv run pytest -q` | **113 passed in 0.64 s** (M5 83 + M6 30) |
| Rust `cargo test --workspace` | **35 passed** (unchanged; M6 is Python-only) |
| Docker image rebuild | ✅ |
| Container boots — both programs RUNNING, both registries loaded | ✅ startup log shows `skill registry loaded 2 skill(s)` + `command registry loaded 3 command(s)` |
| `GET /v1/commands` | 3 builtins (`review`, `summarize`, `test`) with descriptions |
| `GET /v1/commands/review` | model `claude-opus-4-5`, tools `[bash, file_read, grep]`, full prompt body |
| `POST /v1/commands/review/render` with `{"focus":"auth and SQL injection"}` | `rendered_prompt` contains the focus, `model` echoed |
| `features.commands` (HTTP + gRPC) | `"v1"` |
| **Hot-reload** (polling) | Drop `hotcmd.toml` → count 3 → 4 within 4 s. Delete → 4 → 3 within 4 s. |
| **`POST /v1/commands/review/run` real Anthropic SSE** | Streams real Anthropic response text + `finish` + `outcome` + `done` events |

### 3.1 Test counts

- `tests/test_commands.py`: 13 tests (loader, registry scan, sandbox,
  Jinja render, subscribe, non-TOML / hidden file filtering, pub/sub).
- `tests/test_commands_grpc.py`: 5 tests (features flag, List, Run
  render, NOT_FOUND, Reload).
- `tests/test_fastapi_commands.py`: 9 tests (list, detail, 404, render,
  sandbox 400, reload, OpenAPI path presence, health flag, unknown 404).
- `tests/test_fastapi_commands_run.py`: 3 tests using a fake
  `QueryHostClient` — streams SSE events, verifies rendered prompt flows
  into the client, model precedence picks the command's own model.
- `tests/test_grpc.py::test_commands_still_unimplemented` retargeted to
  `ToolRegistry.List` — that's now the only service still returning
  `UNIMPLEMENTED` (M3+ third-party tool plugin backlog).

### 3.2 Real-provider SSE run (E2E)

```
$ curl -N -X POST http://localhost:8080/v1/commands/review/run \
    -d '{"args":{"focus":"this test has no staged diff"},
         "session_id":"m6-run-test","provider":"anthropic",
         "model":"claude-opus-4-5"}'

event: text
data: {"type": "text", "text": "I'll run"}

event: text
data: {"type": "text", "text": " the git diff command to review the staged changes.\n\n<bash>git diff --staged</bash>\n\nThe diff"}

event: text
data: {"type": "text", "text": " is empty - there are no staged changes to review."}

event: finish
data: {"type": "finish", "stop_reason": "end_turn", "input_tokens": 107, "output_tokens": 41}

event: outcome
data: {"type": "outcome", "stop_reason": "end_turn", "turns_taken": 1, "denied": false, "denial_reason": ""}

event: done
data: [DONE]
```

Full path proven: curl → uvicorn → FastAPI `run_command` handler →
`CommandRegistry.render` → `QueryHostClient.run_turn` → gRPC `:50052` →
Rust `QueryHostService.run_turn` → `QueryEngine::run_turn` →
`SidecarBackend` → gRPC `:50051` → `LlmProviderServicer.ChatStream` →
`anthropic` SDK → Anthropic API → back as SSE events.

Note the model's attempt to call `bash` inline via text tags (`<bash>`)
rather than via the real tool-use path — this is the expected M3 state:
tool-use mapping for anthropic/openai was explicitly deferred (Q1 on
M3 said "필요시 채워넣기"). The command subsystem itself passes the
prompt through correctly; the loop just does not interpret the bash tag
as a real tool invocation yet.

### 3.3 Startup log

```
[ashpy] ashpy gRPC server listening on 127.0.0.1:50051
[ashpy] middleware chain: ['logging', 'bash_guard']
[ashpy] skill registry loaded 2 skill(s) from /root/.ash/skills
[ashpy] command registry loaded 3 command(s) from /root/.ash/commands
[ashpy] ashpy FastAPI listening on http://0.0.0.0:8080
[ash] QueryHost gRPC listening on 127.0.0.1:50052
INFO success: ashpy entered RUNNING state
INFO success: ash-serve entered RUNNING state
```

## 4. Issues encountered and resolved

1. **`test_grpc.py::test_commands_still_unimplemented`** inevitably
   fired a false negative once `CommandRegistryServicer` went live.
   Retargeted to `ToolRegistry.List`, which is the only remaining
   `UNIMPLEMENTED` service (Python-side third-party tool plugin
   backlog).
2. **No Rust changes required** — the gRPC `CommandRegistry` service
   was already in `proto/ash.proto` since M1. M6 is 100% Python, which
   matches the M0.5 "customization = Python" principle.
3. **Watcher duplication kept on purpose.** Skills and commands each
   ship their own ~100-line watcher module. A shared base class would
   obscure the lifecycle without saving meaningful code — measured
   ~20 SLOC of overlap.

## 5. Decisions carried forward

1. **Commands are single-file TOML.** `<root>/<name>.toml` only; no
   subdirectories. Hidden files ignored.
2. **gRPC `Run` is render-only.** Actually driving a turn is HTTP-only.
   This keeps the gRPC surface a "facts and knobs" contract and avoids
   putting an SSE-equivalent stream on Run.
3. **Model precedence is fixed at `body.model > command.model >
   session default`.** Documented in `docs/commands.md`.
4. **`allowed_tools` surfaces without enforcement.** A command's
   whitelist is echoed back in every response but the turn loop does
   not yet reject tool calls outside it. Enforcement is scheduled with
   the session-metadata pipeline in M7/M8.
5. **`ASH_SKILLS_POLLING` is a shared knob** across both watchers. Users
   flip one variable and both subsystems respect it.

## 6. Exit criteria — met

- [x] `ashpy/commands/` 5 modules implemented
- [x] `CommandRegistryServicer` three RPCs live
- [x] `features.commands == "v1"` (gRPC and HTTP)
- [x] FastAPI `/v1/commands*` router with list / detail / render / run
      (SSE) / reload
- [x] Three sample TOML commands committed
- [x] Hot-reload end-to-end proven (polling mode)
- [x] `POST /v1/commands/review/run` end-to-end against real Anthropic
- [x] Python tests: 113/113 (30 new)
- [x] Rust tests: 35/35 (unchanged)
- [x] `docs/commands.md` user guide
- [x] `docs/task/M6_TASK_REPORT.md` (this file)

## 7. Changed files

**Added**
- `ashpy/src/ashpy/commands/{__init__, schema, loader, registry, watcher}.py`
- `ashpy/src/ashpy/api/routes_commands.py`
- `ashpy/tests/test_commands.py`
- `ashpy/tests/test_commands_grpc.py`
- `ashpy/tests/test_fastapi_commands.py`
- `ashpy/tests/test_fastapi_commands_run.py`
- `commands/review.toml`
- `commands/summarize.toml`
- `commands/test.toml`
- `docs/commands.md`
- `docs/task/M6_TASK_REPORT.md`

**Modified**
- `ashpy/src/ashpy/server.py` — real `CommandRegistryServicer`,
  `features.commands = "v1"`, command watcher lifecycle
- `ashpy/src/ashpy/api/app.py` — include commands router, flip health
  flag
- `ashpy/tests/test_grpc.py` — retarget unimplemented check to
  `ToolRegistry`

## 8. Next: M7 — TUI

- Implement `crates/tui` on `ratatui` + `crossterm`: banner + chat log +
  prompt input + status bar matching the claurst reference screenshot.
- Remove the temporary `ash llm chat` subcommand (tracked in persistent
  memory from M2).
- The TUI calls the Rust `QueryHost` gRPC directly — **not** the HTTP
  layer — since it lives in the same process space as `ash serve`.
- Add an integration smoke test via the `bc-browser` skill once the
  initial UI lands.
