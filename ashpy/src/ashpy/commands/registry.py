"""Command registry — mirrors the skill registry but for TOML commands."""

from __future__ import annotations

import asyncio
import logging
import os
import pathlib
import threading
from dataclasses import dataclass
from typing import Optional

from jinja2 import Environment
from jinja2.sandbox import SandboxedEnvironment

from .loader import load_command_dir
from .schema import Command, CommandEventKind, CommandEventPayload

_LOG = logging.getLogger("ashpy.commands.registry")

DEFAULT_COMMANDS_DIR = pathlib.Path("/root/.ash/commands")


@dataclass
class RenderResult:
    rendered_prompt: str
    allowed_tools: list[str]
    model: str


class CommandRegistry:
    """Thread-safe registry with sandboxed Jinja2 rendering + pub/sub."""

    def __init__(self, commands_dir: Optional[pathlib.Path] = None) -> None:
        env_override = os.environ.get("ASH_COMMANDS_DIR")
        self._dir = commands_dir or (
            pathlib.Path(env_override) if env_override else DEFAULT_COMMANDS_DIR
        )
        self._lock = threading.RLock()
        self._commands: dict[str, Command] = {}
        self._errors: list[tuple[str, str]] = []
        self._jinja: Environment = SandboxedEnvironment(
            autoescape=False,
            trim_blocks=True,
            lstrip_blocks=True,
        )
        self._subscribers: list[asyncio.Queue] = []
        self._subscribers_lock = threading.Lock()
        self.reload()

    @property
    def directory(self) -> pathlib.Path:
        return self._dir

    def reload(self) -> tuple[int, list[str]]:
        with self._lock:
            commands, errors = load_command_dir(self._dir)
            old_names = set(self._commands.keys())
            self._commands = {c.name: c for c in commands}
            self._errors = [(str(p), msg) for p, msg in errors]

            new_names = set(self._commands.keys())
            added = new_names - old_names
            removed = old_names - new_names
            modified = new_names & old_names
        for name in added:
            self._emit_event(CommandEventKind.ADDED, name)
        for name in removed:
            self._emit_event(CommandEventKind.REMOVED, name)
        for name in modified:
            self._emit_event(CommandEventKind.MODIFIED, name)
        _LOG.info(
            "commands reload: %d loaded, %d errors", len(self._commands), len(self._errors)
        )
        return len(self._commands), [msg for _, msg in self._errors]

    def list_commands(self) -> list[Command]:
        with self._lock:
            return sorted(self._commands.values(), key=lambda c: c.name)

    def get(self, name: str) -> Optional[Command]:
        with self._lock:
            return self._commands.get(name)

    def errors(self) -> list[tuple[str, str]]:
        with self._lock:
            return list(self._errors)

    def render(
        self,
        name: str,
        args: Optional[dict] = None,
        context: Optional[dict] = None,
    ) -> RenderResult:
        command = self.get(name)
        if command is None:
            raise KeyError(f"unknown command: {name}")
        render_ctx: dict = {"args": args or {}, "context": context or {}}
        template = self._jinja.from_string(command.prompt)
        rendered = template.render(**render_ctx)
        return RenderResult(
            rendered_prompt=rendered,
            allowed_tools=list(command.allowed_tools),
            model=command.model or "",
        )

    # --- pub/sub (kept available for parity with skills; no Watch RPC) ---

    def subscribe(self) -> asyncio.Queue:
        q: asyncio.Queue = asyncio.Queue()
        with self._subscribers_lock:
            self._subscribers.append(q)
        return q

    def unsubscribe(self, q: asyncio.Queue) -> None:
        with self._subscribers_lock:
            try:
                self._subscribers.remove(q)
            except ValueError:
                pass

    def _emit_event(self, kind: CommandEventKind, name: str) -> None:
        with self._lock:
            command = self._commands.get(name)
            source = command.source_path if command else ""
        payload = CommandEventPayload(kind=kind, name=name, source_path=source)
        with self._subscribers_lock:
            subs = list(self._subscribers)
        for q in subs:
            try:
                q.put_nowait(payload)
            except asyncio.QueueFull:  # pragma: no cover
                _LOG.warning("command subscriber queue full; dropping event")


# --- singleton ------------------------------------------------------------


_REGISTRY: Optional[CommandRegistry] = None
_REGISTRY_LOCK = threading.Lock()


def get_registry() -> CommandRegistry:
    global _REGISTRY
    with _REGISTRY_LOCK:
        if _REGISTRY is None:
            _REGISTRY = CommandRegistry()
        return _REGISTRY


def reset_registry_for_tests(commands_dir: Optional[pathlib.Path] = None) -> CommandRegistry:
    global _REGISTRY
    with _REGISTRY_LOCK:
        _REGISTRY = CommandRegistry(commands_dir=commands_dir)
        return _REGISTRY
