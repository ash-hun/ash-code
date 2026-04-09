# ash-code

Containerized coding harness. Rust TUI + core loop + HTTP/Swagger API, with a Python sidecar (`ashpy`) that owns the three customization surfaces: **skills**, **commands**, **LLM providers** (Anthropic / OpenAI-compatible / vLLM / Ollama).

> Structural skeleton is modeled on [`reference/claurst`](reference/claurst).

## Status

**M0 — scaffold.** Workspace, crate stubs, Python sidecar stub, Dockerfile + compose, proto draft. No runtime functionality yet.

## Quickstart (after M3/M4)

```bash
docker compose up -d ash-code
open http://localhost:8080/docs         # Swagger UI
docker exec -it ash-code ash tui        # interactive TUI
```

## Layout

```
crates/        # Rust workspace (core, tools, query, tui, api, ipc, bus, cli)
ashpy/         # Python sidecar: skills, commands, LLM providers (M1+)
proto/         # gRPC contract between Rust host and Python sidecar
docker/        # Dockerfile, supervisord, entrypoint
skills/        # user-provided SKILL.md (hot-reloaded by ashpy)
commands/      # user-provided command definitions (TOML + Jinja2)
providers/     # user-provided LLM provider configs
```

## Milestones

M0 scaffold → **M0.5 extensibility design** → M1 gRPC → M2 providers → M3 core loop → M4 API+Swagger → M5 skills → M6 commands → M7 TUI → M8 event bus → M9 docs/samples → M10 CI/E2E.
Each milestone lands with unit tests and a written completion report before the next begins. See [`docs/extensibility.md`](docs/extensibility.md) for the customization contract.
