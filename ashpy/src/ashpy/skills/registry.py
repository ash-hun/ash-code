"""In-process skill registry with Jinja2 sandboxed rendering + hot-reload pub/sub."""

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

from .loader import load_skill_dir
from .schema import Skill, SkillEventKind, SkillEventPayload

_LOG = logging.getLogger("ashpy.skills.registry")

DEFAULT_SKILLS_DIR = pathlib.Path("/root/.ash/skills")


@dataclass
class InvokeResult:
    rendered_prompt: str
    allowed_tools: list[str]
    model: str


class SkillRegistry:
    """Thread-safe registry with a sandboxed Jinja2 environment."""

    def __init__(self, skills_dir: Optional[pathlib.Path] = None) -> None:
        env_override = os.environ.get("ASH_SKILLS_DIR")
        self._dir = skills_dir or (
            pathlib.Path(env_override) if env_override else DEFAULT_SKILLS_DIR
        )
        self._lock = threading.RLock()
        self._skills: dict[str, Skill] = {}
        self._errors: list[tuple[str, str]] = []
        self._jinja: Environment = SandboxedEnvironment(
            autoescape=False,
            trim_blocks=True,
            lstrip_blocks=True,
        )
        # Async subscribers: each open Watch RPC gets its own queue.
        self._subscribers: list[asyncio.Queue] = []
        self._subscribers_lock = threading.Lock()
        self.reload()

    # --- directory / config -----------------------------------------------

    @property
    def directory(self) -> pathlib.Path:
        return self._dir

    # --- loading ----------------------------------------------------------

    def reload(self) -> tuple[int, list[str]]:
        """Re-scan the skills directory. Returns ``(loaded_count, errors)``."""
        with self._lock:
            skills, errors = load_skill_dir(self._dir)
            old_names = set(self._skills.keys())
            self._skills = {s.name: s for s in skills}
            self._errors = [(str(p), msg) for p, msg in errors]

            new_names = set(self._skills.keys())
            added = new_names - old_names
            removed = old_names - new_names
            modified = {
                name for name in new_names & old_names
            }  # reload treats every surviving file as potentially modified
        for name in added:
            self._emit_event(SkillEventKind.ADDED, name)
        for name in removed:
            self._emit_event(SkillEventKind.REMOVED, name)
        for name in modified:
            self._emit_event(SkillEventKind.MODIFIED, name)
        _LOG.info("skills reload: %d loaded, %d errors", len(self._skills), len(self._errors))
        return len(self._skills), [msg for _, msg in self._errors]

    # --- accessors --------------------------------------------------------

    def list_skills(self) -> list[Skill]:
        with self._lock:
            return sorted(self._skills.values(), key=lambda s: s.name)

    def get(self, name: str) -> Optional[Skill]:
        with self._lock:
            return self._skills.get(name)

    def errors(self) -> list[tuple[str, str]]:
        with self._lock:
            return list(self._errors)

    # --- invoke -----------------------------------------------------------

    def invoke(self, name: str, args: Optional[dict] = None, context: Optional[dict] = None) -> InvokeResult:
        skill = self.get(name)
        if skill is None:
            raise KeyError(f"unknown skill: {name}")
        render_ctx: dict = {"args": args or {}}
        if context:
            render_ctx.update(context)
        template = self._jinja.from_string(skill.body)
        rendered = template.render(**render_ctx)
        return InvokeResult(
            rendered_prompt=rendered,
            allowed_tools=list(skill.allowed_tools),
            model=skill.model or "",
        )

    # --- Watch pub/sub ----------------------------------------------------

    def subscribe(self) -> asyncio.Queue:
        """Return a new queue that will receive future :class:`SkillEventPayload`."""
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

    def _emit_event(self, kind: SkillEventKind, name: str) -> None:
        with self._lock:
            skill = self._skills.get(name)
            source = skill.source_path if skill else ""
        payload = SkillEventPayload(kind=kind, name=name, source_path=source)
        with self._subscribers_lock:
            subs = list(self._subscribers)
        for q in subs:
            try:
                q.put_nowait(payload)
            except asyncio.QueueFull:  # pragma: no cover
                _LOG.warning("skill subscriber queue full; dropping event")


# --- module singleton -----------------------------------------------------


_REGISTRY: Optional[SkillRegistry] = None
_REGISTRY_LOCK = threading.Lock()


def get_registry() -> SkillRegistry:
    global _REGISTRY
    with _REGISTRY_LOCK:
        if _REGISTRY is None:
            _REGISTRY = SkillRegistry()
        return _REGISTRY


def reset_registry_for_tests(skills_dir: Optional[pathlib.Path] = None) -> SkillRegistry:
    global _REGISTRY
    with _REGISTRY_LOCK:
        _REGISTRY = SkillRegistry(skills_dir=skills_dir)
        return _REGISTRY
