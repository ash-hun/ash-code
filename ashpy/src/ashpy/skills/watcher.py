"""File-system watcher that debounces `SKILL.md` changes into registry reloads.

Uses ``watchdog`` on a background thread. When a ``SKILL.md`` file is
created, modified, moved, or deleted, we schedule a coroutine onto the
asyncio event loop that (after a short debounce window) calls
``SkillRegistry.reload()``. Reload itself publishes events to all
connected Watch subscribers.

Platforms:
- Linux containers use inotify via ``watchdog.observers.Observer``.
- Environments where inotify is unreliable (Docker Desktop on macOS
  with bind-mounted host paths, older kernels) can opt into polling
  with ``ASH_SKILLS_POLLING=1``.
"""

from __future__ import annotations

import asyncio
import logging
import os
import pathlib
from typing import Optional

from watchdog.events import FileSystemEvent, FileSystemEventHandler
from watchdog.observers import Observer
from watchdog.observers.polling import PollingObserver

from .registry import SkillRegistry

_LOG = logging.getLogger("ashpy.skills.watcher")

DEBOUNCE_SECONDS = 0.2


def _use_polling() -> bool:
    return os.environ.get("ASH_SKILLS_POLLING", "0").lower() in ("1", "true", "yes")


def _is_skill_event(path: str) -> bool:
    return path.endswith("/SKILL.md") or path.endswith("\\SKILL.md")


class _Handler(FileSystemEventHandler):
    def __init__(self, loop: asyncio.AbstractEventLoop, trigger) -> None:
        self._loop = loop
        self._trigger = trigger

    def _maybe(self, event: FileSystemEvent) -> None:
        if event.is_directory:
            return
        src = getattr(event, "src_path", "")
        dst = getattr(event, "dest_path", "")
        if _is_skill_event(src) or (dst and _is_skill_event(dst)):
            asyncio.run_coroutine_threadsafe(self._trigger(), self._loop)

    def on_created(self, event):   # noqa: N802
        self._maybe(event)

    def on_modified(self, event):  # noqa: N802
        self._maybe(event)

    def on_moved(self, event):     # noqa: N802
        self._maybe(event)

    def on_deleted(self, event):   # noqa: N802
        self._maybe(event)


class SkillWatcher:
    """Owns a watchdog observer + an asyncio debounce task."""

    def __init__(self, registry: SkillRegistry, loop: Optional[asyncio.AbstractEventLoop] = None) -> None:
        self._registry = registry
        self._loop = loop or asyncio.get_event_loop()
        self._observer = PollingObserver() if _use_polling() else Observer()
        self._debounce_task: Optional[asyncio.Task] = None
        self._pending = False

    async def _trigger(self) -> None:
        self._pending = True
        if self._debounce_task is None or self._debounce_task.done():
            self._debounce_task = asyncio.create_task(self._debounced_reload())

    async def _debounced_reload(self) -> None:
        await asyncio.sleep(DEBOUNCE_SECONDS)
        if self._pending:
            self._pending = False
            try:
                self._registry.reload()
            except Exception as exc:  # noqa: BLE001
                _LOG.warning("skill reload failed: %s", exc)

    def start(self) -> None:
        root = self._registry.directory
        if not root.exists():
            _LOG.info("skills dir %s does not exist; creating", root)
            try:
                root.mkdir(parents=True, exist_ok=True)
            except Exception as exc:  # noqa: BLE001
                _LOG.warning("could not create %s: %s", root, exc)
                return
        handler = _Handler(self._loop, self._trigger)
        self._observer.schedule(handler, str(root), recursive=True)
        self._observer.start()
        _LOG.info(
            "skill watcher started (path=%s, polling=%s)",
            root,
            _use_polling(),
        )

    def stop(self) -> None:
        try:
            self._observer.stop()
            self._observer.join(timeout=2.0)
        except Exception:  # noqa: BLE001
            pass
