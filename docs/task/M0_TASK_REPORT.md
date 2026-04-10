# M0 Task Report — Scaffold + Extensibility Design

**Milestone:** M0 (Scaffold) + M0.5 (Extensibility Design)
**Date:** 2026-04-09
**Status:** ✅ Completed and approved for M1

---

## 1. Goals

Establish the ash-code monorepo skeleton so that subsequent milestones (M1–M10)
can land incrementally without structural rework. Prove the three runtime
pillars exist and talk to each other at the shell level:

- **Rust workspace** — host process (TUI, core loop, HTTP API, IPC client)
- **Python sidecar (`ashpy`)** — customization surface (skills, commands, LLM
  providers)
- **Docker** — the only supported runtime environment

Additionally (M0.5): lock down the contract for developer-defined customization
before any real code is written, so that extension points are designed in
rather than retrofitted.

## 2. Deliverables

### 2.1 Rust workspace

Root `Cargo.toml` declares an 8-crate workspace:

| Crate | Purpose | M0 state |
|---|---|---|
| `ash-core` | session, storage, settings primitives | stub + 2 tests |
| `ash-tools` | built-in tools (bash, file, grep, …) | stub + 1 test |
| `ash-query` | turn loop, streaming, context compaction | stub + 1 test |
| `ash-tui` | ratatui-based terminal UI (M7) | stub + 1 test |
| `ash-api` | axum + utoipa Swagger UI at :8080 (M4) | stub + 1 test |
| `ash-ipc` | gRPC client to Python sidecar (M1) | stub + 1 test |
| `ash-bus` | in-process event bus shared by TUI and API (M8) | stub + 1 test |
| `ash-cli` | `ash` binary entry (clap subcommands) | `doctor`/`tui`/`serve` surface |

Naming note: the planned `harness` crate was renamed to `bus` to avoid
colliding with the top-level `harness/` directory the user had cleared out at
kickoff.

### 2.2 Python sidecar (`ashpy/`)

- `pyproject.toml`: **uv-managed** via `hatchling` build-backend + `[tool.uv]`
- `uv.lock`: generated inside a `python:3.12` container, 36 packages resolved,
  committed to the repo for reproducible builds
- `src/ashpy/`: `__init__`, `__main__` (argparse CLI: `ashpy serve` / `--version`),
  `server.py` (scaffold loop with SIGTERM handling; real gRPC wiring lands in M1)
- `tests/test_smoke.py`: 4 tests covering `__version__`, default bind loopback,
  `--version` CLI output, help command

### 2.3 gRPC contract draft

`proto/ash.proto` draft with three services scheduled for implementation in
M1–M6:

- `LlmProvider` — `Capabilities`, `ChatStream`, `Switch`
- `SkillRegistry` — `List`, `Invoke`, `Reload`, `Watch` (stream)
- `CommandRegistry` — `List`, `Run`, `Reload`

Additional services (`Harness` hooks, `ToolRegistry`) are scheduled to be added
to this file at the start of M1 per the M0.5 extensibility design.

### 2.4 Docker stack

Three-stage build in `docker/Dockerfile`:

1. **rust-builder** (`rust:1.82-slim-bookworm`) — `cargo build --release -p ash-cli`
2. **py-builder** (`python:3.12-slim-bookworm` + `ghcr.io/astral-sh/uv:0.5.11`) —
   `uv sync --frozen --extra dev` into `/opt/ashpy/venv`
3. **runtime** (`python:3.12-slim-bookworm`) — copies the Rust binary and the
   uv venv; installs `git`, `ripgrep`, `supervisor`

`docker/supervisord.conf` launches two long-running programs inside one
container:

- `ashpy` — `/opt/ashpy/venv/bin/ashpy serve --bind 127.0.0.1:50051`
- `ash-serve` — `/usr/local/bin/ash serve --host 0.0.0.0 --port 8080`

`docker-compose.yml` defines a single `ash-code` service. **No `ollama` or
`vllm` sub-services.** External LLM endpoints are injected via environment
variables only (see §2.5).

### 2.5 External LLM API configuration

Per the M0 patch, ash-code never hosts an LLM. All providers are consumed as
external APIs configured by the user:

```yaml
environment:
  - ASH_LLM_PROVIDER=${ASH_LLM_PROVIDER:-anthropic}
  - ASH_LLM_MODEL=${ASH_LLM_MODEL:-}
  - ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY:-}
  - ANTHROPIC_BASE_URL=${ANTHROPIC_BASE_URL:-}
  - OPENAI_API_KEY=${OPENAI_API_KEY:-}
  - OPENAI_BASE_URL=${OPENAI_BASE_URL:-}
  - VLLM_BASE_URL=${VLLM_BASE_URL:-}
  - VLLM_API_KEY=${VLLM_API_KEY:-}
  - OLLAMA_BASE_URL=${OLLAMA_BASE_URL:-}
```

`.env.example` ships alongside to document every knob.

### 2.6 Extensibility design (M0.5)

`docs/extensibility.md` defines seven customization surfaces with their file
locations, schemas, and reload semantics:

| # | Surface | Drop location | Reload |
|---|---|---|---|
| 1 | LLM Provider plugins | `providers/<name>.toml` + Python module | sidecar restart |
| 2 | Skills | `skills/<name>/SKILL.md` | hot (watchdog) |
| 3 | Commands | `commands/<name>.toml` | hot |
| 4 | System prompt overrides | `~/.ash/prompts/*.j2` | restart |
| 5 | Query-loop hooks (Harness service) | `ashpy/middleware/*.py` | sidecar restart |
| 6 | Tool plugins | `tools/*.py` | sidecar restart |
| 7 | Keybindings / theme | `~/.ash/keybindings.json`, `theme.toml` | TUI restart |

**Key decisions captured in the doc:**

- The four built-in providers (`anthropic`, `openai`, `vllm`, `ollama`) load
  through the same plugin contract a third party would use. No special casing.
- Model selection is **data, not code**: `ASH_LLM_MODEL` env var, `[defaults].model`
  in `providers/<name>.toml`, or runtime `/model` switch. This is how the user's
  "pick opus-4.6 externally" requirement is satisfied.
- Python owns all customization. Rust never imports user code. All hooks flow
  over gRPC.
- `Harness` service with `OnTurnStart`/`OnToolCall`/`OnStreamDelta`/`OnTurnEnd`
  and `HookDecision{ALLOW|DENY|REWRITE}` is the control plane; to be added to
  `proto/ash.proto` at the start of M1.

### 2.7 Repository layout (after M0)

```
ash-code/
├── Cargo.toml                         # Rust workspace (8 crates)
├── crates/
│   ├── core/ tools/ query/ tui/
│   ├── api/ ipc/ bus/ cli/
├── ashpy/
│   ├── pyproject.toml                 # hatchling + uv
│   ├── uv.lock                        # committed
│   ├── src/ashpy/
│   │   ├── __init__.py
│   │   ├── __main__.py                # argparse CLI
│   │   └── server.py                  # scaffold loop (M1 replaces)
│   └── tests/test_smoke.py
├── proto/
│   └── ash.proto                      # 3 services draft (Harness added in M1)
├── docker/
│   ├── Dockerfile                     # 3-stage (rust / uv / runtime)
│   ├── supervisord.conf               # ashpy + ash-serve
│   └── entrypoint.sh
├── docker-compose.yml                 # single service, external LLM APIs only
├── .env.example                       # user-facing env var template
├── .dockerignore / .gitignore
├── skills/ commands/ providers/       # volume-mount targets (.gitkeep)
├── workspace/                         # edit target for the agent
├── docs/
│   ├── extensibility.md               # M0.5 design
│   └── task/M0_TASK_REPORT.md         # this file
└── reference/claurst/                 # read-only reference (unchanged)
```

## 3. Verification

| Check | Command | Result |
|---|---|---|
| Rust unit tests | `cargo test --workspace` (rust:1.82 container) | **7 passed, 0 failed** (core 2, tools 1, query 1, tui 1, api 1, ipc 1, bus 1) |
| Rust CLI smoke | `cargo run -p ash-cli -- doctor` | ✅ prints version + endpoints |
| Python unit tests | `uv`-managed venv → `pytest -q` | **4 passed in 0.01s** |
| Docker image build | `docker compose build ash-code` | ✅ `ash-code:dev` |
| compose config parse | `docker compose config --quiet` | ✅ single service `ash-code` |
| Runtime `ash` | `docker run --rm ash-code:dev ash doctor` | ✅ prints version |
| Runtime `ashpy` | `docker run --rm ash-code:dev ashpy --version` | ✅ `ashpy 0.0.1` |

Sample outputs:

```
$ docker run --rm ash-code:dev ash doctor
ash 0.0.1
  ash-core  0.0.1
  ash-api   port=8080
  ash-ipc   sidecar=http://127.0.0.1:50051

$ uv lock        # py:3.12 container
Resolved 36 packages in 926ms

$ pytest -q
....                                                                     [100%]
4 passed in 0.01s
```

## 4. Issues encountered and resolved

| # | Issue | Resolution |
|---|---|---|
| 1 | `clap_derive 4.6` required `edition2024`, unsupported on Rust 1.82 | Pinned `clap = "=4.5.20"` in workspace deps |
| 2 | Initial runtime `debian:bookworm-slim` could not resolve `/opt/ashpy/venv/bin/python3.12` symlink | Switched runtime base to `python:3.12-slim-bookworm` to match py-builder ABI |
| 3 | Initial `pyproject.toml` used setuptools, inconsistent with the stated uv-based design | Rewrote with `hatchling` + `[tool.uv]`, generated and committed `uv.lock`, updated Dockerfile to `uv sync --frozen` |
| 4 | `docker-compose.yml` included `ollama`/`vllm` sub-services; user required external APIs only | Removed both sub-services and the `ollama` volume; exposed provider endpoints as env vars only; added `.env.example` |
| 5 | Extensibility story was vague — crates were empty "slots" with no hook contract | Added M0.5 micro-step: authored `docs/extensibility.md` defining 7 surfaces + plugin loader discipline before touching runtime code |

## 5. Decisions carried forward

1. **MSRV = Rust 1.82** until tonic 0.12 is introduced in M1. If tonic forces an
   upgrade, revisit `clap` pin at the same time.
2. **uv version pin = `ghcr.io/astral-sh/uv:0.5.11`** for reproducible builds.
3. **`Cargo.lock` is gitignored** for now. M9 will flip this and add CI.
4. **No local toolchain assumed.** All Rust builds/tests run in containers.
5. **127.0.0.1 sidecar binding.** The gRPC server is container-local, never
   exposed to the host. Only port 8080 (HTTP API) is published.
6. **Built-in providers obey the plugin contract** — no hardcoded branches in
   the core loop.

## 6. Exit criteria — met

- [x] Rust workspace compiles and tests pass
- [x] Python sidecar installs via uv and tests pass
- [x] Docker image builds and both binaries run
- [x] `docker-compose.yml` validates and defines exactly one service
- [x] External LLM APIs configurable via environment variables only
- [x] Extensibility contract documented before implementation work
- [x] User approved progression to M1

## 7. Next: M1 — gRPC wiring

- Finalize `proto/ash.proto` (add `Harness`, `ToolRegistry`, `Health.Ping`)
- Implement Rust `ash-ipc` with `tonic` + `tonic-build` + `SidecarClient`
- Replace `ashpy/server.py` scaffold with a real `grpc.aio.server()` binding
  `127.0.0.1:50051` and stubbing all five services (UNIMPLEMENTED where not
  yet owned)
- Add `ash doctor --check-sidecar` E2E ping
- Update Dockerfile to install `protoc` in rust-builder and run
  `grpc_tools.protoc` codegen in py-builder
- Tests: `cargo test -p ash-ipc` (mock server), `pytest ashpy/tests/test_grpc.py`
  (in-process server), E2E `docker exec ash-code ash doctor --check-sidecar`
