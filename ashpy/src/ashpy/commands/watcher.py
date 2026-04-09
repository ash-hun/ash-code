"""Watchdog-driven hot-reload for the command registry.

Mirrors ``ashpy/skills/watcher.py`` rather than sharing a base class —
the duplication keeps each subsystem independently auditable and the
lifecycle code is trivially small.
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

from .registry import CommandRegistry

_LOG = logging.getLogger("ashpy.commands.watcher")

DEBOUNCE_SECONDS = 0.2


def _use_polling() -> bool:
    # Share the skills env var — the two subsystems live together and
    # users expect the same knob to control both.
    return os.environ.get("ASH_SKILLS_POLLING", "0").lower() in ("1", "true", "yes")


def _is_command_event(path: str) -> bool:
    return path.endswith(".toml")


class _Handler(FileSystemEventHandler):
    def __init__(self, loop: asyncio.AbstractEventLoop, trigger) -> None:
        self._loop = loop
        self._trigger = trigger

    def _maybe(self, event: FileSystemEvent) -> None:
        if event.is_directory:
            return
        src = getattr(event, "src_path", "")
        dst = getattr(event, "dest_path", "")
        if _is_command_event(src) or (dst and _is_command_event(dst)):
            asyncio.run_coroutine_threadsafe(self._trigger(), self._loop)

    def on_created(self, event):   # noqa: N802
        self._maybe(event)

    def on_modified(self, event):  # noqa: N802
        self._maybe(event)

    def on_moved(self, event):     # noqa: N802
        self._maybe(event)

    def on_deleted(self, event):   # noqa: N802
        self._maybe(event)


class CommandWatcher:
    def __init__(
        self,
        registry: CommandRegistry,
        loop: Optional[asyncio.AbstractEventLoop] = None,
    ) -> None:
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
                _LOG.warning("command reload failed: %s", exc)

    def start(self) -> None:
        root = self._registry.directory
        if not root.exists():
            _LOG.info("commands dir %s does not exist; creating", root)
            try:
                root.mkdir(parents=True, exist_ok=True)
            except Exception as exc:  # noqa: BLE001
                _LOG.warning("could not create %s: %s", root, exc)
                return
        handler = _Handler(self._loop, self._trigger)
        self._observer.schedule(handler, str(root), recursive=False)
        self._observer.start()
        _LOG.info(
            "command watcher started (path=%s, polling=%s)",
            root,
            _use_polling(),
        )

    def stop(self) -> None:
        try:
            self._observer.stop()
            self._observer.join(timeout=2.0)
        except Exception:  # noqa: BLE001
            pass
