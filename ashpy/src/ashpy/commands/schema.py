"""Pydantic schemas and value types for commands."""

from __future__ import annotations

from dataclasses import dataclass
from enum import IntEnum
from typing import Optional

from pydantic import BaseModel, Field


class CommandFile(BaseModel):
    """Validated contents of a ``commands/<name>.toml`` file."""

    name: str = Field(..., min_length=1)
    description: str = ""
    allowed_tools: list[str] = Field(default_factory=list)
    model: Optional[str] = None
    prompt: str = Field(..., min_length=1)


@dataclass
class Command:
    """Runtime representation of a loaded command."""

    name: str
    description: str
    allowed_tools: list[str]
    model: str  # "" if unset
    prompt: str  # Jinja2 template source
    source_path: str


class CommandEventKind(IntEnum):
    ADDED = 0
    MODIFIED = 1
    REMOVED = 2


@dataclass
class CommandEventPayload:
    kind: CommandEventKind
    name: str
    source_path: str
