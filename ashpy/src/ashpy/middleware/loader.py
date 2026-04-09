"""Middleware discovery.

Order (later wins priority among equal-priority):
  1. Built-in middleware: :class:`LoggingMiddleware`, :class:`BashGuardMiddleware`.
  2. Any Python file under ``/root/.ash/middleware/`` (or
     ``ASH_MIDDLEWARE_DIR``) that defines one or more ``Middleware``
     subclasses. The file stem becomes the default name.

M3 ships "load on startup" semantics. Hot-reload is deferred to align with
M5/M6 skill/command watchdog plumbing.
"""

from __future__ import annotations

import importlib.util
import inspect
import logging
import os
import pathlib
from typing import Optional

from .base import Middleware, MiddlewareChain

_LOG = logging.getLogger("ashpy.middleware.loader")


class MiddlewareLoader:
    def __init__(self, middleware_dir: Optional[pathlib.Path] = None) -> None:
        self._dir = middleware_dir or self._default_dir()

    @staticmethod
    def _default_dir() -> pathlib.Path:
        env = os.environ.get("ASH_MIDDLEWARE_DIR")
        if env:
            return pathlib.Path(env)
        return pathlib.Path("/root/.ash/middleware")

    def discover(self) -> list[Middleware]:
        out: list[Middleware] = []
        if not self._dir.is_dir():
            return out
        for py_file in sorted(self._dir.glob("*.py")):
            if py_file.name.startswith("_"):
                continue
            try:
                out.extend(self._load_from_file(py_file))
            except Exception as exc:  # noqa: BLE001
                _LOG.warning("failed to load middleware %s: %s", py_file, exc)
        return out

    def _load_from_file(self, path: pathlib.Path) -> list[Middleware]:
        module_name = f"ash_user_middleware_{path.stem}"
        spec = importlib.util.spec_from_file_location(module_name, path)
        if spec is None or spec.loader is None:
            raise ImportError(f"cannot load spec for {path}")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return _instantiate_middlewares(module)


def _instantiate_middlewares(module) -> list[Middleware]:
    out: list[Middleware] = []
    for _, obj in inspect.getmembers(module, inspect.isclass):
        if obj is Middleware:
            continue
        if issubclass(obj, Middleware) and obj.__module__ == module.__name__:
            try:
                out.append(obj())
            except Exception as exc:  # noqa: BLE001
                _LOG.warning("failed to instantiate %s: %s", obj, exc)
    return out


def build_default_chain(
    middleware_dir: Optional[pathlib.Path] = None,
    include_builtins: bool = True,
) -> MiddlewareChain:
    chain = MiddlewareChain()
    if include_builtins:
        from .bash_guard_middleware import BashGuardMiddleware
        from .logging_middleware import LoggingMiddleware

        chain.add(LoggingMiddleware())
        chain.add(BashGuardMiddleware())
    for mw in MiddlewareLoader(middleware_dir).discover():
        chain.add(mw)
    return chain
