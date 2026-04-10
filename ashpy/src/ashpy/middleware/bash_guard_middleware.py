"""Flexible bash policy middleware (flipside of the Rust last-resort blacklist).

Rust `crates/tools/src/bash.rs` already blocks `rm -rf /`-class catastrophes
unconditionally. This middleware is the *configurable* layer where users
can tighten or relax policy without editing Rust. The default policy denies:

- Anything matching the Rust catastrophic list (defensive duplicate).
- Writes under `/etc`, `/boot`, `/sys`, `/proc`.
- `sudo` / `su -` privilege escalation.
- Outbound network installers (curl|wget piped to `sh`).

Users can override by dropping a different middleware with a lower
`priority` that returns ``allow()`` earlier in the chain, or by removing
this file from the image and substituting their own under
``/root/.ash/middleware/``.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass

from .base import HookDecision, Middleware, ToolCallEvent, allow, deny


_DENY_PATTERNS: list[re.Pattern[str]] = [
    re.compile(r"\brm\s+-rf\s+/($|\s)"),
    re.compile(r"\brm\s+-rf\s+/\*"),
    re.compile(r":\(\)\s*\{\s*:\|:\s*&\s*\}\s*;"),
    re.compile(r"\bmkfs\."),
    re.compile(r"\bdd\s+if=/dev/(zero|random)\s+of=/dev/"),
    re.compile(r">\s*/dev/sda"),
    re.compile(r"\bchmod\s+-R\s+000\s+/"),
    re.compile(r"\bsudo\b"),
    re.compile(r"\bsu\s+-"),
    re.compile(r"\b(curl|wget)\b.*\|\s*(sh|bash)\b"),
    re.compile(r"\brm\s+-rf\s+/etc\b"),
    re.compile(r">\s*/etc/"),
    re.compile(r">\s*/boot/"),
    re.compile(r">\s*/sys/"),
    re.compile(r">\s*/proc/"),
]


@dataclass
class BashGuardMiddleware(Middleware):
    priority: int = 50
    name: str = "bash_guard"

    async def on_tool_call(self, event: ToolCallEvent) -> HookDecision:
        if event.tool_name != "bash":
            return allow()
        try:
            args = json.loads(event.arguments or b"{}")
        except json.JSONDecodeError:
            return deny("bash_guard: unparseable arguments")
        command = str(args.get("command", ""))
        if not command.strip():
            return deny("bash_guard: empty command")
        for pat in _DENY_PATTERNS:
            if pat.search(command):
                return deny(f"bash_guard: blocked by policy ({pat.pattern})")
        return allow()
