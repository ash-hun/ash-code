# ash-code TUI

The terminal UI exposed by `ash tui`. Built on
[`ratatui`](https://ratatui.rs/) + [`crossterm`](https://docs.rs/crossterm),
runs directly against the Rust `QueryEngine` (same process space as the
`ash serve` binary) and talks to the Python sidecar over gRPC on
`127.0.0.1:50051` for LLM calls and middleware hooks.

## Launching

The TUI runs **inside the container**, never on the host:

```bash
# the container must already be up (docker compose up -d ash-code)
docker exec -it ash-code ash tui
```

The `-it` flags are required — ratatui needs a real TTY.

### Flags

| Flag | Default | Meaning |
|---|---|---|
| `--sidecar <URL>` | `http://127.0.0.1:50051` | Python sidecar gRPC endpoint |
| `--provider <NAME>` | `$ASH_LLM_PROVIDER` or `anthropic` | Active provider |
| `--model <NAME>` | `$ASH_LLM_MODEL` or `(provider default)` | Override model |

### Environment variables

| Env | Purpose |
|---|---|
| `ASH_LLM_PROVIDER` | Default provider when `--provider` is omitted |
| `ASH_LLM_MODEL` | Default model when `--model` is omitted |
| `ASH_TUI_AUTO_APPROVE` | Set to `1` to skip the HITL approval dialog for `bash`. Use **only** in CI/automation. |

## Layout

```
┌─ ash-code v0.0.1 ─────────────────────────────────────────┐
│ Welcome back!   Start with small features or bug fixes... │
│ Tips for getting started                                  │
│   · Type a prompt below and press Enter to send.          │
│   · bash commands trigger an approval dialog — …          │
│ Recent activity   No recent activity                      │
└───────────────────────────────────────────────────────────┘
┌─ chat ────────────────────────────────────────────────────┐
│ You › list rust files in /workspace/crates                │
│ ash ›                                                     │
│   I'll look for Rust sources.                             │
│ ⚙  tool_call · glob  {"pattern":"**/*.rs"}                │
│   ↳ glob ok                                               │
│     crates/tools/src/lib.rs                               │
│     crates/query/src/lib.rs                               │
│     ...                                                   │
│ ash ›                                                     │
│   Found 8 source files across four crates.                │
│   [finish stop_reason=end_turn in=47 out=102]             │
└───────────────────────────────────────────────────────────┘
┌─ input ───────────────────────────────────────────────────┐
│ > summarize crates/query/src/lib.rs▏                      │
└───────────────────────────────────────────────────────────┘
 anthropic · claude-opus-4-5 · session=tui-abc123 · turns=1 · Ctrl-C quit
```

Four regions from top to bottom:

1. **Header** — version banner, welcome line, tips, "recent activity"
   (counts the current session's messages).
2. **Chat log** — coloured by role. User (cyan), assistant (white),
   tool calls (yellow), tool results (green/red), errors (red bold).
3. **Input field** — the current prompt being composed. Locked while a
   turn is running or the approval modal is open.
4. **Status bar** — active provider, model, session id, turn count,
   quit hint.

## Key bindings

### Normal mode

| Key | Action |
|---|---|
| `Enter` | Submit the current input as a new turn |
| printable chars | Append to the input buffer |
| `Backspace` | Delete last char |
| `Page Up` / `Page Down` | Scroll the chat log by 5 lines |
| `End` | Jump to the bottom of the chat log |
| `Esc` | Quit (only when input is empty and no turn is running) |
| `Ctrl-C` | Quit immediately |

M7 intentionally ships **minimal editing**: no left/right cursor
movement, no history recall, no multi-line input. These land with M8.

### Approval dialog

When the model requests a `bash` tool call, a modal overlays the chat:

```
┌─ Allow this bash command? ─────────────────────────┐
│ ┌─ command ────────────────────────────────────┐   │
│ │ {"command": "git diff --staged"}              │   │
│ └──────────────────────────────────────────────┘   │
│ ▶ 1  Yes                                          │
│   2  No                                           │
│   3  Tell ash-code what to do instead             │
│                                                   │
│ ↑↓ select · 1/2/3 jump · Enter confirm · Esc cancel│
└────────────────────────────────────────────────────┘
```

| Key | Action |
|---|---|
| `1` | Allow — execute the bash command as requested |
| `2` | Deny with reason "user denied" |
| `3` | Open a feedback box to tell the model what to do instead |
| `↑` / `↓` | Move the selection marker |
| `Enter` | Confirm the highlighted option |
| `Esc` | Cancel (equivalent to "No") |

When option **3** is chosen, the modal reveals a text input. Type your
feedback, press `Enter` to submit. The feedback becomes the deny reason
— it is written back as a `tool_result` message so the model can see
why you rejected and what to do instead.

### Why only `bash`?

The other built-in tools (`file_read`, `file_write`, `file_edit`,
`grep`, `glob`) are scoped enough that unattended use is safe inside
the container's workspace. `bash` runs arbitrary shell commands and
deserves a human gate. The list lives in
`crates/tui/src/backend.rs::requires_approval` — extend it if your
deployment wants stricter policy.

## Defence in depth

The `bash` tool has **three layers** of protection when you type a
prompt:

1. **Rust last-resort blacklist** (`crates/tools/src/bash.rs`) —
   unconditional. Blocks `rm -rf /`, fork bombs, `mkfs`, `dd` to raw
   devices, etc. Fires even if the middleware chain is empty.
2. **Python `bash_guard` middleware** — configurable. Blocks `sudo`,
   `su -`, `curl|sh`, writes to `/etc|/boot|/sys|/proc`. Users replace
   this file (`ashpy/src/ashpy/middleware/bash_guard_middleware.py`)
   to tighten or relax policy.
3. **TUI HITL approval dialog** — interactive. You get the final say
   on each individual command.

If layer 1 or 2 denies, the TUI never sees an approval prompt — the
tool call is already blocked, and the chat log shows a red `⊘` denial
line with the reason. If both layers allow, the dialog pops up and
waits for you.

## How it all wires together

```
ash tui
  ↓
TuiConfig (--sidecar, --provider, --model)
  ↓
SidecarClient.connect(:50051)   # Python sidecar
  ↓
SidecarBackend  ──►  TuiBackend (HITL decorator)
  ↓
QueryEngine::run_turn
  ├─► SidecarBackend.chat_stream       (LlmProvider.ChatStream)
  ├─► TuiBackend.on_tool_call
  │     ├─► SidecarBackend.on_tool_call (Harness → bash_guard)
  │     └─► approval dialog (via channel to UI thread)
  ├─► ToolRegistry::invoke             (Rust built-in tools)
  └─► SidecarBackend.on_turn_end
```

The TUI never touches HTTP; it calls the sidecar directly. The FastAPI
layer on `:8080` is for external consumers (browsers, curl, external
tools) — the TUI has its own in-process path.

## Shutdown and panic safety

Terminal raw mode and the alternate screen are released via an RAII
guard in `crates/tui/src/event.rs`. Even if the event loop panics, the
`Drop` impl runs and restores your terminal. If ratatui ever leaves
your terminal broken (unlikely), run:

```
stty sane
reset
```

## Known limits (M7)

- **No mid-turn cancellation.** `Ctrl-C` quits the whole TUI, it does
  not cancel an in-flight turn mid-stream. Scheduled for M8 with the
  session event bus.
- **No runtime provider/model switch.** You pick at startup via flags
  or env. Runtime switch lands with M8 slash-commands (`/model`,
  `/provider`).
- **Text-mode tool-use only.** Anthropic/OpenAI tool-use JSON
  translation is still the M3 deferred item — the model will often
  emit `<bash>…</bash>` as text rather than a real protobuf tool call.
  When it does emit a real tool call (e.g. for file operations), the
  turn loop handles it correctly.
- **No input history.** Typed prompts are not persisted across
  sessions. Persistent session storage lands in M9.
- **macOS/Windows Docker Desktop** users who rely on volume-mounted
  skills/commands still need `ASH_SKILLS_POLLING=1` in `.env` (shared
  with M5/M6 watchers).
