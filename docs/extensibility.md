# ash-code Extensibility Design (M0.5)

This document defines every surface where a developer can customize the ash-code
harness **without forking the repository**. It is the contract that M1–M8
implementations must honor. Anything outside these surfaces is internal and may
change between releases.

## Guiding principles

1. **Drop-in over rebuild.** Every customization lands as a file in a volume-mounted
   directory or an environment variable. No recompilation required.
2. **Built-ins use the same contract as user extensions.** The four bundled LLM
   providers (`anthropic`, `openai`, `vllm`, `ollama`) are loaded through the same
   plugin mechanism a third party would use. If the built-in path works, the
   user path works.
3. **Python owns customization, Rust owns the core loop.** All extension surfaces
   live in `ashpy/` (Python). Rust `crates/query` exposes typed hook callbacks to
   the sidecar over gRPC. The Rust side never imports user code.
4. **External LLM APIs only.** The ash-code container does not host vLLM or
   Ollama; endpoints are supplied via environment variables or
   `providers/*.toml`.

## Extension surfaces

### 1. LLM Provider plugins — `providers/<name>.toml` + `ashpy/src/ashpy/providers/<name>_p.py`

**Selection (external, no code edits):**

```bash
# .env
ASH_LLM_PROVIDER=anthropic          # plugin name
ASH_LLM_MODEL=claude-opus-4-6       # optional override
```

Or at runtime:

- TUI: `/model anthropic claude-opus-4-6`
- API: `POST /v1/llm/switch { "provider": "anthropic", "model": "claude-opus-4-6" }`

**Config file example — `providers/anthropic.toml`:**

```toml
[provider]
name = "anthropic"
module = "ashpy.providers.anthropic_p"   # built-in; user plugins use their own module path
class = "AnthropicProvider"

[defaults]
model = "claude-opus-4-6"
temperature = 0.2
max_tokens = 8192

[auth]
api_key_env = "ANTHROPIC_API_KEY"
base_url_env = "ANTHROPIC_BASE_URL"      # optional
```

**Python contract — `ashpy/providers/base.py`:**

```python
class LlmProvider(ABC):
    name: str                             # plugin identifier
    def __init__(self, config: ProviderConfig): ...
    def capabilities(self) -> ProviderCaps: ...          # tool_use / vision / ctx
    async def chat_stream(self, req: ChatRequest) -> AsyncIterator[ChatDelta]: ...
    async def health(self) -> HealthStatus: ...
```

**Discovery order:**
1. Built-in modules under `ashpy.providers.*`.
2. Any `providers/*.toml` in the mounted volume — the `module` field may point at
   a user Python module on `PYTHONPATH` (e.g. a user-supplied wheel or a file in
   `providers/plugins/`).
3. On conflict, volume-mounted config wins (enables overriding the built-in
   `claude-opus-4-6` default with a different model).

**How model switching is externalized (answers Q3):**

The plugin class is fixed per provider, but the **model name is data**. Users
override it via:
- `providers/<name>.toml` `[defaults].model`
- `ASH_LLM_MODEL` environment variable
- Runtime command `/model` / `POST /v1/llm/switch`

This mirrors how Claude Code lets you pick `opus-4.6` vs `sonnet-4.6` without
changing the agent loop.

### 2. Skills — `skills/<name>/SKILL.md`

```markdown
---
name: review-diff
description: Review staged git diff
triggers: ["review", "리뷰"]
allowed_tools: [bash, file_read, grep]
model: claude-opus-4-6   # optional per-skill model override
---
You are reviewing a staged diff. Steps:
1. Run `git diff --staged`
2. ...
```

- Loader: `python-frontmatter` + `pydantic` schema.
- Hot-reload: `watchdog` → `SkillRegistry.reload()` → gRPC `Watch` stream to Rust.
- Invocation: TUI `/skill review-diff` or API `POST /v1/skills/invoke`.

### 3. Commands — `commands/<name>.toml`

```toml
name = "review"
description = "Review staged diff"
prompt = """
Read `git diff --staged` and review.
Focus: {{ args | default('overall quality') }}
Branch: {{ git_branch }}
"""
allowed_tools = ["bash", "file_read", "grep"]
```

- Jinja2 context: `args`, `cwd`, `git_branch`, `env`, `user`.
- TUI `/review security` → `CommandRegistry.Run("review", {"args": "security"})`.

### 4. System prompt overrides — `~/.ash/prompts/*.j2`

Three named templates are resolvable:

| Name | Purpose | Fallback |
|---|---|---|
| `system.j2` | Base system prompt for every turn | built-in `core/default_prompts/system.j2` |
| `tool_use.j2` | Tool-use preamble | built-in |
| `compaction.j2` | Summarization prompt for context compaction | built-in |

If the file exists in the mounted `prompts/` directory it wins; otherwise the
built-in baked into the Rust binary is used.

### 5. Query-loop hooks (harness control plane)

Defined in `proto/ash.proto` (to be added in M1):

```
service Harness {
  rpc OnTurnStart(TurnContext) returns (HookDecision);
  rpc OnToolCall(ToolCall)     returns (HookDecision);   // allow | deny | rewrite
  rpc OnStreamDelta(ChatDelta) returns (Empty);
  rpc OnTurnEnd(TurnResult)    returns (Empty);
}

message HookDecision {
  enum Kind { ALLOW = 0; DENY = 1; REWRITE = 2; }
  Kind kind = 1;
  string reason = 2;
  bytes rewritten_payload = 3;   // only for REWRITE
}
```

Rust `crates/query` calls into the sidecar at each hook point. Python-side
middleware chain (`ashpy/middleware/`) sees each call and may short-circuit.
This is where a user wires in: per-tool rate limiting, PII redaction,
secret scanning, audit logging, custom tool-approval policies, prompt injection
defenses, etc.

**Middleware contract — `ashpy/middleware/base.py`:**

```python
class Middleware(ABC):
    priority: int = 100                   # lower runs first
    async def on_turn_start(self, ctx: TurnContext) -> HookDecision: ...
    async def on_tool_call(self, call: ToolCall) -> HookDecision: ...
    async def on_stream_delta(self, delta: ChatDelta) -> None: ...
    async def on_turn_end(self, result: TurnResult) -> None: ...
```

Discovered from `middleware/*.py` in the mounted volume and registered by
priority.

### 6. Tool plugins — `tools/<name>.py`

Built-in tools (`bash`, `file_*`, `grep`, `glob`, ...) live in Rust `crates/tools`
for speed. **Third-party tools** are registered via the sidecar:

```python
# tools/my_tool.py
from ashpy.tools import Tool, ToolResult

class MyTool(Tool):
    name = "my_tool"
    description = "..."
    input_schema = { "type": "object", "properties": {...} }

    async def run(self, args: dict) -> ToolResult:
        ...
```

Registered through gRPC `ToolRegistry.Register`. Rust query loop treats
user tools identically to built-ins — the Python side proxies execution.

### 7. Keybindings & TUI theme — `~/.ash/keybindings.json`, `~/.ash/theme.toml`

TUI-only. Loaded by `crates/tui` at startup. Defaults shipped in the binary;
file overrides merge on top.

## Summary: what a developer can customize without forking

| Surface | Location | Change = |
|---|---|---|
| Which LLM provider | `ASH_LLM_PROVIDER` ENV | instant |
| Which model | `ASH_LLM_MODEL` ENV / `providers/*.toml` / `/model` | instant / hot |
| Add a new provider | `providers/plugins/` Python module + TOML | restart sidecar |
| Add a skill | `skills/<name>/SKILL.md` | hot-reload |
| Add a command | `commands/<name>.toml` | hot-reload |
| Override system prompt | `prompts/*.j2` | restart |
| Intercept turn loop | `middleware/*.py` | restart sidecar |
| Add a tool | `tools/*.py` | restart sidecar |
| Rebind keys | `~/.ash/keybindings.json` | TUI restart |
| Change theme | `~/.ash/theme.toml` | TUI restart |

## Out-of-scope for M0.5

- Skill marketplace / remote plugin fetching.
- Plugin sandboxing (plugins run with full sidecar privileges; document this risk
  clearly in `docs/security.md` at M9).
- Hot-reload for middleware and tools (restart required; skills/commands are hot).

## Downstream impact on milestones

- **M1** adds the `Harness` service + `Tool*`/`Middleware*` messages to the proto.
- **M2** implements the 4 built-in providers using the plugin contract.
- **M3** wires `crates/query` to call the `Harness` hooks.
- **M5/M6** implement skill/command loaders respecting the schemas above.
- **M7** reads keybindings/theme from the paths above.
