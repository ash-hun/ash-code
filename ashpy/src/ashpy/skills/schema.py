"""Pydantic schemas and value types for skills."""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import IntEnum
from typing import Optional

from pydantic import BaseModel, Field


class SkillFrontmatter(BaseModel):
    """Validated frontmatter of a ``SKILL.md`` file."""

    name: str = Field(..., min_length=1)
    description: str = ""
    triggers: list[str] = Field(default_factory=list)
    allowed_tools: list[str] = Field(default_factory=list)
    model: Optional[str] = None


@dataclass
class Skill:
    """Runtime representation of a loaded skill."""

    name: str
    description: str
    triggers: list[str]
    allowed_tools: list[str]
    model: str  # "" if unset
    body: str   # raw body (Jinja2 template source)
    source_path: str

    def frontmatter(self) -> SkillFrontmatter:
        return SkillFrontmatter(
            name=self.name,
            description=self.description,
            triggers=list(self.triggers),
            allowed_tools=list(self.allowed_tools),
            model=self.model or None,
        )


class SkillEventKind(IntEnum):
    ADDED = 0
    MODIFIED = 1
    REMOVED = 2


@dataclass
class SkillEventPayload:
    kind: SkillEventKind
    name: str
    source_path: str
