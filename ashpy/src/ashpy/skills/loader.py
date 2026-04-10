"""`SKILL.md` parser (frontmatter + body).

Layout convention (M5, Q1=a):
    <root>/<skill_name>/SKILL.md
Each skill lives in its own subdirectory so future releases can ship
resource files (images, reference .md, sample data) alongside the main
markdown. Bare ``<root>/<name>.md`` files are deliberately NOT supported.
"""

from __future__ import annotations

import logging
import pathlib
from typing import Optional

import frontmatter
from pydantic import ValidationError

from .schema import Skill, SkillFrontmatter

_LOG = logging.getLogger("ashpy.skills.loader")

SKILL_FILE_NAME = "SKILL.md"


class SkillLoadError(ValueError):
    """Raised when a SKILL.md file is malformed."""


def load_skill_file(path: pathlib.Path) -> Skill:
    """Parse a single ``SKILL.md`` file. Raises :class:`SkillLoadError`."""
    try:
        post = frontmatter.load(path)
    except Exception as exc:  # noqa: BLE001
        raise SkillLoadError(f"failed to parse frontmatter in {path}: {exc}") from exc

    metadata = dict(post.metadata or {})
    if "name" not in metadata:
        # Fall back to the parent directory name.
        metadata["name"] = path.parent.name

    try:
        fm = SkillFrontmatter(**metadata)
    except ValidationError as exc:
        raise SkillLoadError(f"invalid frontmatter in {path}: {exc}") from exc

    return Skill(
        name=fm.name,
        description=fm.description,
        triggers=list(fm.triggers),
        allowed_tools=list(fm.allowed_tools),
        model=fm.model or "",
        body=str(post.content or "").strip(),
        source_path=str(path),
    )


def load_skill_dir(root: pathlib.Path) -> tuple[list[Skill], list[tuple[pathlib.Path, str]]]:
    """Scan ``root`` for ``<name>/SKILL.md`` files.

    Returns ``(skills, errors)`` where ``errors`` is a list of
    ``(path, message)`` pairs for files that failed to parse. Parsing
    errors are never fatal — the surviving skills still load.
    """
    skills: list[Skill] = []
    errors: list[tuple[pathlib.Path, str]] = []

    if not root.is_dir():
        return skills, errors

    for entry in sorted(root.iterdir()):
        if not entry.is_dir():
            continue
        skill_file = entry / SKILL_FILE_NAME
        if not skill_file.is_file():
            continue
        try:
            skill = load_skill_file(skill_file)
        except SkillLoadError as exc:
            _LOG.warning("%s", exc)
            errors.append((skill_file, str(exc)))
            continue
        skills.append(skill)

    # Detect duplicate names — last one wins, first is dropped with a warning.
    seen: dict[str, Skill] = {}
    for s in skills:
        if s.name in seen:
            _LOG.warning(
                "duplicate skill name %r: %s shadows %s",
                s.name,
                s.source_path,
                seen[s.name].source_path,
            )
        seen[s.name] = s
    return list(seen.values()), errors
