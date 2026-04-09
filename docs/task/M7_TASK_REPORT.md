# M7 Task Report — TUI (Terminal UI) + HITL Approval

**Milestone:** M7 (ratatui-based TUI + HITL bash approval dialog)
**Date:** 2026-04-10
**Status:** ✅ Code + tests green, awaiting user manual smoke test

---

## 1. Goals

Ship the interactive terminal UI promised since M0: `ash tui` opens a
ratatui-powered chat interface that drives the Rust `QueryEngine`
directly, streams responses as they arrive, and **blocks bash tool
calls behind a human-in-the-loop approval dialog** matching the
reference screenshot the user attached ("Allow this bash command?
[1] Yes [2] No [3] Tell ash-code what to do instead"). Remove the
temporary `ash llm chat` / `ash llm list` subcommands that M2 added as
a stopgap.

## 2. Deliverables

### 2.1 New Rust crate: `ash-tui`

`crates/tui/src/` — 5 modules, ~1 000 LoC:

| Module | Responsibility |
|---|---|
| `lib.rs` | `TuiConfig`, `run()` entry point, `connect_with_retry` to the sidecar |
| `app.rs` | Pure state + logic: `AppState`, `ChatLine`, `Mode`, `ApprovalState`. Zero I/O — fully unit-testable. |
| `backend.rs` | `TuiBackend` — `QueryBackend` decorator that wraps `SidecarBackend` and inserts the HITL approval channel |
| `ui.rs` | ratatui frame rendering: header (banner + tips + recent activity), chat log, input box, status bar, approval modal overlay |
| `event.rs` | Async event loop: `tokio::select!` between crossterm key events, turn-execution events, and approval requests. Owns terminal setup/teardown via an RAII guard. |

### 2.2 HITL design (the headline M7 feature)

Tool-call interception lives in `TuiBackend::on_tool_call`:

```text
TuiBackend.on_tool_call
  │
  ├─► self.inner.on_tool_call(event)      [consult Python middleware]
  │        │
  │        ├─► if DENY/REWRITE → return immediately (bash_guard wins)
  │        └─► if ALLOW → fall through
  │
  ├─► if requires_approval(tool.name) && !auto_approve
  │        │
  │        ├─► send ApprovalEnvelope over mpsc to the UI thread
  │        └─► await oneshot::Receiver<ApprovalDecision>
  │
  └─► otherwise → ALLOW
```

- **Approval channel**: `mpsc::UnboundedSender<ApprovalEnvelope>` from
  `TuiBackend` to the UI loop. Each envelope carries a one-shot reply
  channel the UI fills once the user picks an option.
- **`requires_approval`** — starts as `tool_name == "bash"` only.
  Scope is a single-line change away from including other tools.
- **`ASH_TUI_AUTO_APPROVE=1`** — environment knob to skip the dialog
  entirely, for CI/automation contexts.
- **Approval modal (`ui.rs::render_approval_modal`)**: centered overlay,
  title "Allow this bash command?", command preview box, three
  selectable options (`1 Yes`, `2 No`, `3 Tell ash-code what to do
  instead`), keyboard hint line. Option 3 reveals a feedback input box
  inside the modal. Feedback text becomes the `HookDecision.reason`
  and is fed back as a `tool_result` message so the model can read
  why it was rejected and what to do next.

This reuses the existing `Harness.OnToolCall` contract from M3 — no
proto changes needed. The TUI is just another middleware-shaped
consumer, injected on the Rust side instead of inside the Python
chain.

### 2.3 UI structure

Four vertical regions (see `docs/tui.md` for an ASCII mockup):

1. **Header (7 rows)** — claurst-style banner (Q3=b): version, welcome
   line, "Tips for getting started" with two bullets, "Recent activity"
   line that counts the current session's messages.
2. **Chat body (flex)** — scrollable `Paragraph`. Role-coloured:
   - User → cyan bold `You ›`
   - Assistant → magenta bold `ash ›` + white text
   - Tool call → yellow `⚙ tool_call · <name>` with dim-grey args
   - Tool result → green/red `↳ <name> ok/fail` + dim-grey body
   - Finish → dim-grey `[finish stop_reason=…]`
   - Error → red bold `✖`
   - Denial → red `⊘ <tool> denied: <reason>`
3. **Input (3 rows)** — bordered box, locked indicator when a turn is
   running or the approval modal is open.
4. **Status bar (1 row)** — reverse-video cyan background with
   `provider · model · session=… · turns=N · Ctrl-C quit`.

### 2.4 Streaming text coalescing

`AppState::push_text_delta` appends to the last `ChatLine::Assistant`
in the buffer when possible, so the assistant response grows in place
instead of producing one chat row per token. Unit test
`text_delta_coalesces_into_last_assistant_line` locks this in.

### 2.5 Key bindings (Q2=a — minimal)

| Normal mode | |
|---|---|
| printable / Backspace | input editing |
| `Enter` | submit turn |
| `Page Up` / `Page Down` / `End` | scroll chat |
| `Esc` | quit when input empty and no turn running |
| `Ctrl-C` | always quit |

| Approval mode | |
|---|---|
| `1` / `2` / `3` | jump + confirm |
| `↑` / `↓` | move selection |
| `Enter` | confirm highlighted option |
| `Esc` | cancel = deny with reason "user cancelled" |
| printable / Backspace | edit custom feedback (option 3 only) |

Intentionally **not** shipped in M7: left/right cursor movement in
input, up/down input history, multi-line prompts, runtime
`/provider` or `/model` commands. All slated for M8.

### 2.6 `ash llm chat` / `ash llm list` removed

`crates/cli/src/main.rs` rewrite:
- `Command::Llm` variant deleted.
- `LlmAction` enum deleted.
- `run_llm()` and `StdoutSink` helpers deleted.
- `Command::Tui` variant added, wires through `ash_tui::run(config)`.
- `tokio-stream` dependency removed from `crates/cli/Cargo.toml` — cli
  no longer streams protobuf deltas directly.

Verified removal:

```
$ docker run --rm ash-code:dev ash llm --help
error: unrecognized subcommand 'llm'
```

Persistent memory entry `project_m7_cleanup_llm_chat.md` updated from
"TODO for M7" to "completed 2026-04-10".

### 2.7 Toolchain bump

`ratatui 0.29` pulled in new transitive deps (`darling 0.23`,
`instability 0.3.12`) that require **Rust 1.88**. `docker/Dockerfile`
`rust-builder` base bumped `rust:1.85-slim-bookworm` →
`rust:1.88-slim-bookworm`. No code-level MSRV assertions elsewhere, so
no downstream fallout.

### 2.8 Docs

- `docs/tui.md` — full end-user guide: launching, flags, environment,
  layout, key bindings, approval dialog walkthrough, defence-in-depth
  explanation, wiring diagram, shutdown safety notes, known limits.
- `docs/task/M7_TASK_REPORT.md` — this file.

## 3. Verification

### 3.1 Rust tests

| Scope | Result |
|---|---|
| `cargo test --workspace` (rust:1.88 + protoc) | **51 passed, 0 failed** |
| Breakdown | ash-tools 17 · ash-query 4 · ash-api 3 · ash-ipc 3 · ash-core 2 · **ash-tui 16 (new)** · ash-bus/api/cli/tui stub tests · 6 misc |

The 16 new tests in `ash-tui`:
- `app.rs` (13) — input editing, prompt trimming, coalescing deltas,
  scroll, approval cycles (Yes / No / Custom feedback / Cancel),
  mode gating while running.
- `backend.rs` (3) — `requires_approval(bash)`, non-bash tools pass
  through, decision-to-proto conversion.
- `ui.rs` (2 inline via `ratatui::backend::TestBackend`) — header and
  chat render without panic and contain expected text; approval modal
  renders with title + feedback hint.

### 3.2 Python tests

Unchanged: **113 passed** (M6 numbers). TUI is Rust-only.

### 3.3 Docker image

```
$ docker compose build ash-code
... Image ash-code:dev Built
```

### 3.4 CLI surface smoke

```
$ docker run --rm ash-code:dev ash --help
ash-code — containerized coding harness

Usage: ash [COMMAND]

Commands:
  tui     Launch the interactive TUI
  serve   Run the Rust `QueryHost` gRPC server...
  doctor  Print component versions...

$ docker run --rm ash-code:dev ash tui --help
Usage: ash tui [OPTIONS]

Options:
      --sidecar <SIDECAR>    [default: http://127.0.0.1:50051]
      --provider <PROVIDER>  [env: ASH_LLM_PROVIDER=] [default: anthropic]
      --model <MODEL>        [env: ASH_LLM_MODEL=] [default: ]

$ docker run --rm ash-code:dev ash llm --help
error: unrecognized subcommand 'llm'
```

### 3.5 Manual smoke (requires user action)

TUIs cannot be driven by `docker run --rm` non-interactively —
crossterm's raw mode needs a real TTY. The recommended test for the
user is:

```bash
docker compose up -d ash-code
docker exec -it ash-code ash tui
# type:  list files in /workspace
# enter
# … response should stream live
# type:  run git status
# enter
# … modal appears: "Allow this bash command?"
# press 3 → type "tell me what files changed without running anything" → Enter
# … model gets your feedback as a tool_result
# Ctrl-C to quit
```

Per Q1=a: the automated test coverage stops at the Rust unit layer.
Interactive smoke is on the user to run and report back.

## 4. Issues encountered and resolved

1. **Rust 1.85 MSRV blocked by ratatui 0.29.** `darling@0.23` and
   `instability@0.3.12` both require Rust 1.88. Fix: bump Dockerfile
   rust-builder to `rust:1.88-slim-bookworm`. Clean upgrade, no other
   crate affected.
2. **`ToolResult` export.** Initial import tried `ash_query::ToolResult`
   — the type is re-exported from `ash_tools`. Fix: `use
   ash_tools::ToolResult as RsToolResult;`.
3. **Missing `tonic` dependency** for `ash-tui::backend` — the
   `QueryBackend` trait signature returns `tonic::Status` via its
   stream associated type. Added `tonic.workspace = true` to
   `crates/tui/Cargo.toml`.
4. **Oneshot receiver mutability in tests.** `rx.try_recv()` requires
   `&mut self`; four test functions needed `let (tx, mut rx) = ...`.
   Trivial fix.

## 5. Decisions carried forward

1. **Minimal editing, intentionally.** No left/right, no history, no
   multi-line. Anything beyond append/backspace is M8.
2. **Approval scope = bash only by default.** File tools and search
   tools are scoped enough to run unattended in the container's
   workspace. Users who want tighter policy edit
   `TuiBackend::requires_approval` or their Python middleware.
3. **TUI calls the sidecar directly.** It does **not** go through the
   HTTP `/v1/chat` layer. The HTTP layer is for external consumers
   (browsers, curl, external tools); the TUI lives in the same host
   as the turn engine and uses gRPC directly for lower latency and no
   serialisation through SSE.
4. **RAII terminal teardown.** A `TerminalGuard` struct's `Drop` impl
   releases raw mode and leaves the alternate screen even on panic.
   Tested indirectly by the normal event loop path; no explicit panic
   test because panics in tests are a pain to isolate from real
   failures.
5. **No mid-turn cancellation in M7.** `Ctrl-C` quits the whole TUI.
   Cancelling a live turn requires threading a `CancellationToken`
   through `QueryEngine::run_turn`, which touches M3 core code —
   deferred to M8 to keep M7 scope tight.

## 6. Exit criteria — met

- [x] `crates/tui` implemented with 5 modules
- [x] ratatui rendering with 4-region layout
- [x] Real `QueryEngine::run_turn` integration via channel-based
      `TurnSink`
- [x] HITL approval dialog for `bash` with Yes / No / Custom feedback
- [x] RAII-safe terminal teardown
- [x] `crates/cli` has `ash tui` and has removed `ash llm chat` /
      `ash llm list`
- [x] Rust tests: 51/51 (16 new in ash-tui)
- [x] Python tests unchanged: 113/113
- [x] Docker image rebuilds with Rust 1.88
- [x] `docs/tui.md` user guide
- [x] `docs/task/M7_TASK_REPORT.md` (this file)
- [ ] **Manual interactive smoke by the user** — see §3.5

## 7. Changed files

**Added**
- `crates/tui/src/{lib, app, backend, ui, event}.rs`
- `docs/tui.md`
- `docs/task/M7_TASK_REPORT.md`

**Modified**
- `Cargo.toml` — workspace deps `ratatui`, `crossterm`, `unicode-width`
- `crates/tui/Cargo.toml` — full dependency set (ratatui/crossterm/
  tonic/…)
- `crates/cli/src/main.rs` — `ash tui` subcommand, `ash llm` fully
  removed
- `crates/cli/Cargo.toml` — `tokio-stream` dep removed
- `docker/Dockerfile` — Rust 1.85 → 1.88 for ratatui transitive
  requirements

**Memory**
- `project_m7_cleanup_llm_chat.md` — marked completed

## 8. Next: M8 — Event bus + mid-turn cancellation

- Implement `crates/bus` in-process event bus so multiple consumers
  (HTTP `/v1/chat`, TUI, potential future subscribers) can observe the
  same session's turns.
- Add a cancellation token to `QueryEngine::run_turn` so `Ctrl-C`
  inside the TUI can stop a turn mid-stream without killing the
  process.
- Wire `Harness.OnStreamDelta` into the Rust loop (currently Python
  has the plumbing but no caller on the Rust side).
- Optional: basic input editing improvements (left/right, history) in
  the TUI. Low priority vs. the cancellation story.
