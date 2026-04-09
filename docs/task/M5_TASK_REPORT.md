# M5 Task Report — Skills System

**Milestone:** M5 (Skills loader + hot-reload + gRPC + FastAPI)
**Date:** 2026-04-10
**Status:** ✅ Completed and awaiting approval for M6

---

## 1. Goals

Complete the **second pillar of the M0.5 customization triangle**: skills.
Users drop `SKILL.md` files into `skills/<name>/SKILL.md` and the agent
gains new "instruction packets" it can render and inject into turns. The
registry must hot-reload on file-system changes, expose itself over both
gRPC (for the Rust turn loop) and HTTP (for browsers), and degrade
gracefully on malformed files. `features.skills` must flip from
`"planned"` to `"v1"` in the health report.

## 2. Deliverables

### 2.1 Python `ashpy/src/ashpy/skills/` — 5 modules

| File | Purpose |
|---|---|
| `__init__.py` | Re-exports of the public surface |
| `schema.py` | `SkillFrontmatter` (pydantic), `Skill` (dataclass), `SkillEventKind` enum, `SkillEventPayload` |
| `loader.py` | `load_skill_file(path)` + `load_skill_dir(root)`. Uses `python-frontmatter` + `pydantic`. Enforces `<name>/SKILL.md` layout; bare `.md` files are ignored (Q1=a). Non-fatal errors collected and returned alongside successfully-loaded skills. |
| `registry.py` | `SkillRegistry` — thread-safe registry, `jinja2.sandbox.SandboxedEnvironment` for template rendering, pub/sub subscriber queues for the Watch RPC. Singleton via `get_registry()`. |
| `watcher.py` | `SkillWatcher` — `watchdog.observers.Observer` (native inotify) or `PollingObserver` (when `ASH_SKILLS_POLLING=1`). 200 ms asyncio debounce. Bridges watchdog threads to the asyncio loop via `asyncio.run_coroutine_threadsafe`. |

Discovery layout is strict (Q1=a): only `<root>/<name>/SKILL.md` is
loaded. This reserves each skill's directory for future resources
(reference markdown, images, sample data).

Rendering uses `SandboxedEnvironment` — attempts to reach `__class__`,
`__mro__`, `__globals__`, etc. raise `jinja2.exceptions.SecurityError`.

### 2.2 `SkillRegistryServicer` — four RPCs live

`ashpy/src/ashpy/server.py`:

- `List` → `ListSkillsResponse` with every field from the loaded `Skill`
  dataclass.
- `Invoke` → renders the template, returns
  `InvokeSkillResponse{rendered_prompt, allowed_tools, model}`. Unknown
  name → `NOT_FOUND`. Render error → `INVALID_ARGUMENT`.
- `Reload` → `ReloadResponse{loaded, errors}`.
- `Watch` → server-streaming RPC. Subscribes via
  `registry.subscribe()`, awaits events, emits `SkillEvent{kind, name,
  source_path}` for each, cleans up in `finally:` even on client
  disconnect.

`features.skills` in `Health.Ping` promoted from `"planned"` →
**`"v1"`**.

### 2.3 Skill watcher co-located with the sidecar loop

`ashpy/src/ashpy/server.py::_serve_async` now instantiates a
`SkillWatcher`, starts it before the FastAPI/gRPC serving tasks, and
stops it during the SIGTERM shutdown path. Reload failures never crash
the sidecar — they are logged and the previous skill set stays live.

### 2.4 FastAPI router `ashpy/src/ashpy/api/routes_skills.py`

Four endpoints under `/v1/skills`:

| Method | Path | Description |
|---|---|---|
| `GET` | `/v1/skills` | List every loaded skill |
| `GET` | `/v1/skills/{name}` | Full skill detail incl. body |
| `POST` | `/v1/skills/{name}/invoke` | Render and return prompt + allowed tools + model |
| `POST` | `/v1/skills/reload` | Force rescan |

Pydantic models drive the schema: `SkillInfo`, `ListSkillsResponse`,
`SkillDetail`, `InvokeSkillRequest`, `InvokeSkillResponse`,
`ReloadSkillsResponse`. FastAPI auto-surfaces them in `/openapi.json`
and the `/docs` Swagger UI.

`features.skills` in `GET /v1/health` also flipped to `"v1"`.

### 2.5 Sample skills

Committed under `skills/`:

- `skills/review-diff/SKILL.md` — staged-diff reviewer with a
  configurable `focus` arg, `claude-opus-4-5` model override, three
  allowed tools.
- `skills/summarize-file/SKILL.md` — structured file summarizer with a
  required `path` arg.

Both load automatically at container startup.

### 2.6 Docker / compose

- `docker-compose.yml` — added `ASH_SKILLS_POLLING=${ASH_SKILLS_POLLING:-}`
  to the `ash-code` service environment so hosts that bind-mount
  `skills/` from a non-Linux filesystem can force the polling observer.
- No new dependencies; `watchdog`, `python-frontmatter`, `pydantic`, and
  `jinja2` were already in `uv.lock` from M0.

### 2.7 `docs/skills.md`

End-user guide: file layout, frontmatter schema, template context, HTTP
and gRPC APIs, hot-reload behavior (incl. the macOS/Windows polling
note), error handling, example workflow, current limits.

## 3. Verification

| Check | Result |
|---|---|
| Python `uv run pytest -q` | **83 passed in 0.51s** (M4 57 + M5 26) |
| Rust `cargo test --workspace` | **35 passed, 0 failed** (unchanged from M4) |
| `docker compose build ash-code` | ✅ |
| Container boots — both programs `RUNNING` | ✅ `ashpy`, `ash-serve` both `entered RUNNING state`; startup log shows `skill registry loaded 2 skill(s) from /root/.ash/skills` |
| `GET /v1/skills` | Returns 2 builtins with full schema (triggers, allowed_tools, model, source_path) |
| `GET /v1/skills/review-diff` | Returns full body, korean trigger `리뷰` round-trips through utf-8 |
| `POST /v1/skills/review-diff/invoke` with `{"focus":"auth and SQL injection"}` | `rendered_prompt` contains the focus string, `model == "claude-opus-4-5"`, `allowed_tools == ["bash","file_read","grep"]` |
| `GET /v1/health` | `features.skills == "v1"` |
| **Hot-reload via polling** (`ASH_SKILLS_POLLING=1`) | Added `auto-reload-test` → count 2 → 3 within 4 s. Deleted → count 3 → 2 within 4 s. |
| **Manual reload** (`POST /v1/skills/reload`) | Works identically with or without polling — used as the Linux fallback when bind-mount inotify is unavailable. |

### 3.1 Test counts

- `ashpy/tests/test_skills.py`: 12 tests (loader, registry scan,
  sandbox, Jinja render, subscribe/reload, directory-layout enforcement)
- `ashpy/tests/test_skills_grpc.py`: 6 tests (features flag, List,
  Invoke, NOT_FOUND, Reload, Watch-receives-event)
- `ashpy/tests/test_fastapi_skills.py`: 8 tests (list, detail, 404,
  invoke, reload, openapi path presence, health flag)
- 2 existing tests updated:
  - `test_grpc.py::test_skills_still_unimplemented` retargeted to
    `CommandRegistry` (now the only remaining `UNIMPLEMENTED` service)
  - (no other M4 test needed changes)

### 3.2 Startup log

```
[ashpy] ashpy gRPC server listening on 127.0.0.1:50051
[ashpy] middleware chain: ['logging', 'bash_guard']
[ashpy] skill registry loaded 2 skill(s) from /root/.ash/skills
[ashpy] ashpy FastAPI listening on http://0.0.0.0:8080
[ash] QueryHost gRPC listening on 127.0.0.1:50052
INFO success: ashpy entered RUNNING state
INFO success: ash-serve entered RUNNING state
```

### 3.3 Hot-reload proof (polling mode)

```
$ curl -s http://localhost:8080/v1/skills | jq '.skills | length'
2

$ cat > skills/auto-reload-test/SKILL.md <<EOF
---
name: auto-reload-test
description: Polling-mode watchdog test
---
auto-body
EOF

$ sleep 4
$ curl -s http://localhost:8080/v1/skills | jq '[.skills[].name] | sort'
[
  "auto-reload-test",
  "review-diff",
  "summarize-file"
]

$ rm -rf skills/auto-reload-test
$ sleep 4
$ curl -s http://localhost:8080/v1/skills | jq '.skills | length'
2
```

## 4. Issues encountered and resolved

1. **`test_invoke_sandboxed_jinja_blocks_attribute_access`** initially
   asserted silent-undefined behavior — Jinja2's `SandboxedEnvironment`
   actually raises `jinja2.exceptions.SecurityError`. Test updated.
2. **`test_grpc.py::test_skills_still_unimplemented`** became a false
   negative after `SkillRegistryServicer` went live. Retargeted to
   `CommandRegistry.List` (still `UNIMPLEMENTED` until M6).
3. **Initial hot-reload E2E failed on macOS Docker Desktop.** Root
   cause: inotify events do not cross the Desktop VM ↔ host bind-mount
   boundary reliably. `PollingObserver` from `watchdog` works — but
   `ASH_SKILLS_POLLING=1` had to be threaded through
   `docker-compose.yml`'s `environment` block so the value actually
   reaches the container. Documented in `docs/skills.md`.
4. **Rust test count unchanged** — no Rust file was touched in M5,
   consistent with the M0.5 plan ("skills are fully Python-owned").

## 5. Decisions carried forward

1. **Directory-only skill layout** (`skills/<name>/SKILL.md`). Bare
   markdown files at the root are not loaded.
2. **Hot-reload defaults on** when the watcher can run. Users on
   macOS/Windows hosts set `ASH_SKILLS_POLLING=1`; Linux hosts leave it
   blank for native inotify.
3. **Skill model override > session default** — `Skill.model` field is
   surfaced in every invoke response and the turn loop will prefer it
   in M6/M7. No enforcement in M5 yet.
4. **`Watch` has no in-tree consumer** (Q3=a). The RPC stream works and
   is unit-tested; the first subscriber will be the M7 TUI. Tests
   confirm one-subscriber → one-event delivery.
5. **Jinja2 sandboxing is non-negotiable.** Every template render goes
   through `SandboxedEnvironment`. Attribute-access escapes raise.

## 6. Exit criteria — met

- [x] `ashpy/skills/` 5 modules implemented
- [x] `SkillRegistryServicer` 4 RPCs live (List / Invoke / Reload / Watch)
- [x] `features.skills == "v1"`
- [x] FastAPI `/v1/skills*` router + OpenAPI registration
- [x] Two sample `SKILL.md` files committed
- [x] Hot-reload end-to-end proven in polling mode
- [x] Python tests: 83/83 (26 new)
- [x] Rust tests: 35/35 (unchanged)
- [x] `docs/skills.md` user guide written
- [x] `docs/task/M5_TASK_REPORT.md` (this file)

## 7. Changed files

**Added**
- `ashpy/src/ashpy/skills/{__init__, schema, loader, registry, watcher}.py`
- `ashpy/src/ashpy/api/routes_skills.py`
- `ashpy/tests/test_skills.py`
- `ashpy/tests/test_skills_grpc.py`
- `ashpy/tests/test_fastapi_skills.py`
- `skills/review-diff/SKILL.md`
- `skills/summarize-file/SKILL.md`
- `docs/skills.md`
- `docs/task/M5_TASK_REPORT.md`

**Modified**
- `ashpy/src/ashpy/server.py` — real `SkillRegistryServicer`,
  `features.skills="v1"`, skill watcher lifecycle
- `ashpy/src/ashpy/api/app.py` — include skills router, flip health flag
- `ashpy/tests/test_grpc.py` — retarget unimplemented check to
  `CommandRegistry`
- `docker-compose.yml` — `ASH_SKILLS_POLLING` env pass-through

## 8. Next: M6 — Commands

- Implement `ashpy/commands/` loader for `commands/<name>.toml` with
  Jinja2 rendering (mirror of skills).
- Wire `CommandRegistryServicer` → promote `features.commands` to
  `"v1"`.
- Add `ashpy/api/routes_commands.py` with `GET /v1/commands`,
  `POST /v1/commands/{name}/run`, `POST /v1/commands/reload`.
- Tie `allowed_tools` into the query loop so `/review` etc. honor the
  whitelist.
- Ship 2–3 sample command TOML files and a `docs/commands.md` guide.
