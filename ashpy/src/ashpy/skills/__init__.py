"""Skills subsystem (M5).

Discovers, loads, hot-reloads, and invokes ``SKILL.md`` files dropped by
the user into ``skills/<name>/SKILL.md``.
"""

from .loader import SkillLoadError, load_skill_dir
from .registry import SkillRegistry, get_registry, reset_registry_for_tests
from .schema import Skill, SkillEventKind, SkillEventPayload, SkillFrontmatter

__all__ = [
    "Skill",
    "SkillEventKind",
    "SkillEventPayload",
    "SkillFrontmatter",
    "SkillLoadError",
    "SkillRegistry",
    "get_registry",
    "load_skill_dir",
    "reset_registry_for_tests",
]
