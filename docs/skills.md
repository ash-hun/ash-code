# Writing ash-code Skills

A **skill** is a reusable instruction packet that the agent can pull in
on demand. Skills live as `SKILL.md` files under the mounted
`skills/` directory and are hot-reloaded while the container runs.

## File layout

```
skills/
├── review-diff/
│   └── SKILL.md
├── summarize-file/
│   └── SKILL.md
└── my-custom/
    ├── SKILL.md        ← required
    ├── reference.md    ← optional resources next to it (future-friendly)
    └── assets/
        └── example.png
```

- **One directory per skill**, and the main file **must** be named
  `SKILL.md`. Bare `my-skill.md` files at the root are ignored by design
  so resources can grow without layout churn.
- The directory name doubles as the skill's default `name` if
  frontmatter omits it.

## `SKILL.md` schema

```markdown
---
name: review-diff                                        # required (or dir name)
description: Review the staged git diff with a focus    # required-ish
triggers: ["review", "리뷰", "diff"]                    # optional keywords
allowed_tools: ["bash", "file_read", "grep"]            # optional whitelist
model: claude-opus-4-5                                  # optional override
---
You are reviewing a staged git diff.

Steps:
1. Run `git diff --staged` via the `bash` tool.
2. Focus: {{ args.focus | default("overall quality") }}
3. Use `path/to/file.py:LINE` references.
```

| Field | Type | Meaning |
|---|---|---|
| `name` | string | Stable identifier used by `SkillRegistry.Invoke` and the HTTP API. Falls back to the directory name if omitted. |
| `description` | string | Shown in listings and Swagger docs. Keep it one line. |
| `triggers` | list[string] | Keywords the TUI/commands layer can match against user input (M6+). No effect in M5 beyond being exposed via the API. |
| `allowed_tools` | list[string] | Tool whitelist surfaced via `InvokeSkillResponse.allowed_tools`. The turn loop is expected to honor it (wiring lands with commands in M6). |
| `model` | string | Per-skill model override. Empty → inherit session model. |

The body is a **Jinja2 template** rendered inside a
`jinja2.sandbox.SandboxedEnvironment`. Attempts to reach `__class__`,
`__mro__`, `__globals__`, and similar escape hatches raise
`jinja2.exceptions.SecurityError`.

### Template context

| Variable | Description |
|---|---|
| `args` | dict — caller-supplied. Populated from `POST /v1/skills/{name}/invoke` body `args` field or the gRPC `InvokeSkillRequest.args` map. |
| `cwd`, `git_branch`, `user`, ... | dict — caller-supplied `context` map. Currently unused by the core engine; TUI and HTTP callers can pass anything. |

Typical usage:

```jinja
Focus: {{ args.focus | default("overall quality") }}
Working directory: {{ cwd | default("?") }}
```

## HTTP API

```
GET    /v1/skills                      → list all skills
GET    /v1/skills/{name}                → full detail incl. body
POST   /v1/skills/{name}/invoke         → render and return the prompt
POST   /v1/skills/reload                → force rescan of skills/
```

Example invoke:

```bash
curl -s -X POST http://localhost:8080/v1/skills/review-diff/invoke \
  -H 'Content-Type: application/json' \
  -d '{"args": {"focus": "auth and SQL injection"}}'
```

Response:

```json
{
  "rendered_prompt": "You are reviewing a staged git diff.\n\nSteps:\n1. Run `git diff --staged` via the `bash` tool...\n3. Focus: auth and SQL injection\n...",
  "allowed_tools": ["bash", "file_read", "grep"],
  "model": "claude-opus-4-5"
}
```

Swagger UI at [`http://localhost:8080/docs`](http://localhost:8080/docs)
shows every endpoint with schemas.

## gRPC API

Same surface on `127.0.0.1:50051`:

```
SkillRegistry.List     → ListSkillsResponse
SkillRegistry.Invoke   → InvokeSkillResponse
SkillRegistry.Reload   → ReloadResponse
SkillRegistry.Watch    → stream SkillEvent  (ADDED | MODIFIED | REMOVED)
```

`Watch` is a server-streaming RPC. Clients that want real-time "a skill
just landed" notifications can subscribe; M5 ships the plumbing but no
in-tree subscriber yet (the M7 TUI is the expected first consumer).

## Hot-reload

ash-code watches `skills/` for changes and rescans automatically:

1. `watchdog.observers.Observer` runs in a background thread.
2. File-system events are debounced for 200 ms in the asyncio loop.
3. After the debounce window the registry calls
   `SkillRegistry.reload()`, which:
   - rescans every `<name>/SKILL.md`,
   - diffs the new skill set against the old one,
   - pushes `ADDED` / `MODIFIED` / `REMOVED` events to every connected
     `Watch` subscriber.

### Platform note: inotify vs polling

Native inotify works inside Linux containers on Linux hosts. It does
**not** reliably cross the macOS / Windows Docker Desktop VM boundary
for bind-mounted host paths — the kernel event is raised on the host,
not inside the Linux VM where ashpy runs.

The `ASH_SKILLS_POLLING` environment variable switches the watcher to
`watchdog.observers.polling.PollingObserver`, which polls the directory
every ~1 second:

```bash
# in .env
ASH_SKILLS_POLLING=1
```

Costs:
- Slight CPU overhead (typically <0.5%).
- ~1 second detection latency instead of <100 ms.

Recommended for **any** host that bind-mounts `skills/` from a non-Linux
machine. Harmless on Linux hosts.

### Fallback: manual reload

If you prefer never to poll, you can skip the watcher entirely and call
`POST /v1/skills/reload` from your editor's save hook:

```bash
curl -X POST http://localhost:8080/v1/skills/reload
```

## Errors

- **Malformed frontmatter** (missing `name`, YAML parse error) → the
  offending file is skipped, other skills still load, the error is
  surfaced in the `errors` list of `POST /v1/skills/reload`.
- **Template render error** (missing variable, sandbox violation) →
  `POST /v1/skills/{name}/invoke` returns HTTP 400 with the Jinja2
  traceback in the `detail` field.
- **Duplicate skill name** → a warning is logged and the file that
  appears later in a sorted directory walk wins.

## Example workflow

```bash
# 1. Create a new skill directory + file.
mkdir -p skills/explain-function
cat > skills/explain-function/SKILL.md <<'EOF'
---
name: explain-function
description: Read a function and explain it in one paragraph
allowed_tools: ["file_read"]
---
Open `{{ args.path }}` and summarise the function named
`{{ args.symbol }}`. Include:
- what it does in one sentence
- the inputs it expects
- any non-obvious side effects
EOF

# 2. (polling mode only) wait ~2 seconds for the watcher.
sleep 2

# 3. Confirm it loaded.
curl -s http://localhost:8080/v1/skills | jq '.skills[] | .name'
# "explain-function"
# "review-diff"
# "summarize-file"

# 4. Render it.
curl -s -X POST http://localhost:8080/v1/skills/explain-function/invoke \
  -H 'Content-Type: application/json' \
  -d '{"args":{"path":"crates/query/src/lib.rs","symbol":"run_turn"}}' | jq .rendered_prompt
```

## Current limits (M5)

- Skills are read-only once rendered; the returned text is not piped
  into a turn automatically. That integration (skill → session →
  `QueryHost.RunTurn`) lands with commands in M6 and the TUI in M7.
- No skill marketplace or remote fetching. M9 considers this.
- Skill plugins cannot register their own tools — use the built-in
  Rust tools (`bash`, `file_*`, `grep`, `glob`) for now. Python-side
  tool plugins are on the M3+ backlog.
