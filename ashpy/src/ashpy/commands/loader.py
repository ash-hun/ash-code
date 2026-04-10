"""`commands/<name>.toml` parser.

Layout convention (M6, Q1=a): a command is a single TOML file at
``<root>/<name>.toml``. Subdirectories are not walked — commands are
intentionally lightweight compared to skills.
"""

from __future__ import annotations

import logging
import pathlib

try:  # py >= 3.11
    import tomllib
except ModuleNotFoundError:  # pragma: no cover
    import tomli as tomllib  # type: ignore

from pydantic import ValidationError

from .schema import Command, CommandFile

_LOG = logging.getLogger("ashpy.commands.loader")


class CommandLoadError(ValueError):
    """Raised when a command TOML file is malformed."""


def load_command_file(path: pathlib.Path) -> Command:
    """Parse a single ``<name>.toml`` file. Raises :class:`CommandLoadError`."""
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except Exception as exc:  # noqa: BLE001
        raise CommandLoadError(f"failed to parse {path}: {exc}") from exc

    if "name" not in data:
        # Fall back to the file stem if the TOML omits a name.
        data["name"] = path.stem

    try:
        cf = CommandFile(**data)
    except ValidationError as exc:
        raise CommandLoadError(f"invalid command in {path}: {exc}") from exc

    return Command(
        name=cf.name,
        description=cf.description,
        allowed_tools=list(cf.allowed_tools),
        model=cf.model or "",
        prompt=str(cf.prompt).strip(),
        source_path=str(path),
    )


def load_command_dir(root: pathlib.Path) -> tuple[list[Command], list[tuple[pathlib.Path, str]]]:
    """Scan ``root`` for ``*.toml`` files.

    Returns ``(commands, errors)``. Errors are collected per-file so one
    malformed file cannot take down the rest.
    """
    commands: list[Command] = []
    errors: list[tuple[pathlib.Path, str]] = []

    if not root.is_dir():
        return commands, errors

    for entry in sorted(root.iterdir()):
        if not entry.is_file() or entry.suffix != ".toml" or entry.name.startswith("."):
            continue
        try:
            command = load_command_file(entry)
        except CommandLoadError as exc:
            _LOG.warning("%s", exc)
            errors.append((entry, str(exc)))
            continue
        commands.append(command)

    seen: dict[str, Command] = {}
    for c in commands:
        if c.name in seen:
            _LOG.warning(
                "duplicate command name %r: %s shadows %s",
                c.name,
                c.source_path,
                seen[c.name].source_path,
            )
        seen[c.name] = c
    return list(seen.values()), errors
