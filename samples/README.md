# samples/

Working examples of the three ash-code customization surfaces. Copy any
of these into your project to use them.

## Skills

| Sample | Description | Copy to |
|---|---|---|
| `skills/explain-error/SKILL.md` | Paste an error, get a plain-language explanation + fix | `skills/explain-error/SKILL.md` |

Skills are hot-reloaded. After copying, the skill appears in
`GET /v1/skills` within seconds (Linux) or at the next poll interval
(macOS/Windows with `ASH_SKILLS_POLLING=1`).

## Commands

| Sample | Description | Copy to |
|---|---|---|
| `commands/healthcheck.toml` | Check service health, container status, and logs | `commands/healthcheck.toml` |
| `commands/git-summary.toml` | Summarize recent git activity in the workspace | `commands/git-summary.toml` |

Commands are loaded at startup. After copying, restart the container
or call `POST /v1/commands/reload`.

## Middleware

| Sample | Description | Copy to |
|---|---|---|
| `middleware/token_budget_middleware.py` | Per-session token budget enforcement | `ashpy/src/ashpy/middleware/` |

Middleware requires registration in `ashpy/src/ashpy/middleware/loader.py`
and a container rebuild. See the file header for setup instructions.
