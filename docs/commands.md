# Writing ash-code Commands

A **command** is a reusable agent prompt with optional tool whitelisting
and model override. Unlike skills (which only render a prompt), commands
can be **executed end-to-end** via the HTTP API — the FastAPI layer
renders the template, forwards it to the Rust `QueryHost.RunTurn` gRPC,
and streams the turn back as Server-Sent Events.

## File layout

```
commands/
├── review.toml
├── summarize.toml
└── test.toml
```

One TOML file per command. Subdirectories are not walked — commands are
meant to be lightweight. Hidden files (starting with `.`) and non-`.toml`
files are ignored.

The file **stem** is the default command name if the TOML omits `name`.

## TOML schema

```toml
name        = "review"                            # optional; defaults to file stem
description = "Review the staged git diff"        # optional; shown in listings
allowed_tools = ["bash", "file_read", "grep"]     # optional; surfaced in responses
model       = "claude-opus-4-5"                   # optional; overrides session model

prompt = """
Focus: {{ args.focus | default('overall quality') }}
Branch: {{ context.git_branch | default('?') }}
"""
```

| Field | Type | Meaning |
|---|---|---|
| `name` | string | Stable identifier. Falls back to the file stem if omitted. |
| `description` | string | One-liner shown in `GET /v1/commands`. |
| `allowed_tools` | list[string] | Tool whitelist echoed back in `render`/`run` responses. Enforcement in the turn loop lands in a later milestone (see §Limits). |
| `model` | string | Per-command model override. Empty → session/default model. |
| `prompt` | string (Jinja2) | **Required.** Template body rendered at invoke time. |

The prompt is a **Jinja2 template** rendered inside a
`jinja2.sandbox.SandboxedEnvironment` — the same sandbox skills use.
Attribute-escape attempts (`__class__`, `__mro__`, ...) raise
`jinja2.exceptions.SecurityError`.

### Template context

| Variable | Description |
|---|---|
| `args` | dict — caller-supplied, taken from the request body `args` field. |
| `context` | dict — caller-supplied. Convention is to put per-session data here (`cwd`, `git_branch`, `user`). |

Typical usage:

```jinja
Focus: {{ args.focus | default('overall') }}
Working directory: {{ context.cwd | default('?') }}
```

## HTTP API

```
GET    /v1/commands                      → list all commands
GET    /v1/commands/{name}                → full detail incl. prompt body
POST   /v1/commands/{name}/render         → render the template, return the prompt
POST   /v1/commands/{name}/run            → render AND execute a real turn (SSE)
POST   /v1/commands/reload                → force rescan of commands/
```

### `render` vs `run`

| Endpoint | Does it call the LLM? | Response type |
|---|---|---|
| `render` | No | JSON (single response) |
| `run` | Yes — calls `QueryHost.RunTurn` on the Rust side | Server-Sent Events |

`render` is useful for previewing, debugging, or piping the prompt into
your own orchestration. `run` is the end-to-end path a TUI or external
tool would use.

### `render` example

```bash
curl -s -X POST http://localhost:8080/v1/commands/review/render \
  -H 'Content-Type: application/json' \
  -d '{"args": {"focus": "auth and SQL injection"}}'
```

Response:

```json
{
  "rendered_prompt": "You are reviewing the current staged git diff.\n\nSteps:\n...",
  "allowed_tools": ["bash", "file_read", "grep"],
  "model": "claude-opus-4-5"
}
```

### `run` example

```bash
curl -N -X POST http://localhost:8080/v1/commands/review/run \
  -H 'Content-Type: application/json' \
  -d '{
        "args": {"focus": "auth"},
        "session_id": "s-m6",
        "provider": "anthropic"
      }'
```

Response is a Server-Sent Event stream:

```
event: text
data: {"type": "text", "text": "I'll run the git diff command..."}

event: text
data: {"type": "text", "text": " The diff is empty."}

event: finish
data: {"type": "finish", "stop_reason": "end_turn", "input_tokens": 107, "output_tokens": 41}

event: outcome
data: {"type": "outcome", "stop_reason": "end_turn", "turns_taken": 1, "denied": false, "denial_reason": ""}

event: done
data: [DONE]
```

### Model precedence (for `run`)

```
request body `model`  >  command's `model` field  >  session default
```

If the body sets `"model": "claude-sonnet-4-5"`, it wins. Otherwise the
command's own `model` is used. Otherwise the session inherits whatever
the sidecar's active provider has as its default (`ASH_LLM_MODEL` env
or provider built-in default).

## gRPC API

Same surface on `127.0.0.1:50051`:

```
CommandRegistry.List    → ListCommandsResponse
CommandRegistry.Run     → RunCommandResponse   (render-only; no turn)
CommandRegistry.Reload  → ReloadResponse
```

The gRPC `Run` is **render-only** on purpose. Actually driving a turn
belongs on the HTTP side where SSE and session state are already wired.

## Hot-reload

Commands share the same watchdog + debounce plumbing as skills:

- Linux hosts: native inotify picks up any `*.toml` change in the mounted
  `commands/` directory and rescans within ~300 ms.
- macOS / Windows Docker Desktop: set `ASH_SKILLS_POLLING=1` in your
  `.env` file. The same env var flips **both** skills and commands to
  `PollingObserver` — they share the knob on purpose.

### Manual reload

```bash
curl -X POST http://localhost:8080/v1/commands/reload
```

Always works, regardless of platform.

## Errors

- **Malformed TOML** (missing `name` or `prompt`, parse error) → the
  offending file is skipped. Other commands still load. Errors surface
  in the `errors` list of `POST /v1/commands/reload`.
- **Template render error** → `POST /v1/commands/{name}/render` returns
  HTTP 400 with the Jinja2 traceback in the `detail` field. `run` will
  also 400 before starting the SSE stream if the template cannot render.
- **Duplicate command name** → warning logged; the later-scanned file
  wins.

## Limits (M6)

- **`allowed_tools` is surface-only.** The list is returned from `render`
  and `run`, but the turn loop does not yet enforce it — a command that
  declares `allowed_tools = ["bash"]` can still see the model invoke any
  tool the provider exposes. Enforcement is scheduled with the session
  metadata pipeline in M7/M8.
- **Sessions are in-memory.** A `run` call persists the session only
  inside the Rust QueryHost's process-local `HashMap`. Container restart
  loses history. Persistent storage is M9.
- **Commands cannot register new tools.** Use the built-in Rust tools
  (`bash`, `file_*`, `grep`, `glob`). Python-side tool plugins are on the
  M3+ backlog.
- **No command → skill chaining.** A command template cannot
  transparently include a skill body. You can compose at the call site
  by rendering the skill separately and passing the result into the
  command's `args`.
