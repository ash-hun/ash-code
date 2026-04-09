"""Commands subsystem (M6).

Discovers, loads, hot-reloads, and renders ``commands/<name>.toml``
files. Unlike skills, commands are intended to drive a real agent turn
— the FastAPI ``/v1/commands/{name}/run`` endpoint calls back into the
Rust ``QueryHost.RunTurn`` gRPC after rendering.
"""

from .loader import CommandLoadError, load_command_dir, load_command_file
from .registry import CommandRegistry, get_registry, reset_registry_for_tests
from .schema import (
    Command,
    CommandEventKind,
    CommandEventPayload,
    CommandFile,
)

__all__ = [
    "Command",
    "CommandEventKind",
    "CommandEventPayload",
    "CommandFile",
    "CommandLoadError",
    "CommandRegistry",
    "get_registry",
    "load_command_dir",
    "load_command_file",
    "reset_registry_for_tests",
]
