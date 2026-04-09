"""Unit tests for skills loader + registry (M5)."""

from __future__ import annotations

import pathlib
import textwrap

import pytest

from ashpy.skills import SkillRegistry
from ashpy.skills.loader import SkillLoadError, load_skill_file
from ashpy.skills.schema import SkillEventKind


def _write_skill(root: pathlib.Path, name: str, body: str, **fm) -> pathlib.Path:
    skill_dir = root / name
    skill_dir.mkdir(parents=True, exist_ok=True)
    fm_defaults = {"name": name, "description": f"{name} skill"}
    fm_defaults.update(fm)
    frontmatter_lines = "\n".join(
        f"{k}: {v if not isinstance(v, list) else str(v)}" for k, v in fm_defaults.items()
    )
    content = f"---\n{frontmatter_lines}\n---\n{body}\n"
    path = skill_dir / "SKILL.md"
    path.write_text(content)
    return path


# --- loader ---------------------------------------------------------------


def test_load_valid_skill(tmp_path: pathlib.Path):
    path = _write_skill(
        tmp_path,
        "hello",
        "Say hi to {{ args.target | default('world') }}",
        triggers="['hi', 'hello']",
        allowed_tools="['bash']",
    )
    skill = load_skill_file(path)
    assert skill.name == "hello"
    assert skill.triggers == ["hi", "hello"]
    assert skill.allowed_tools == ["bash"]
    assert "Say hi to" in skill.body


def test_load_without_name_falls_back_to_directory(tmp_path: pathlib.Path):
    d = tmp_path / "from-dirname"
    d.mkdir()
    (d / "SKILL.md").write_text("---\ndescription: x\n---\nbody")
    skill = load_skill_file(d / "SKILL.md")
    assert skill.name == "from-dirname"


def test_load_invalid_frontmatter_raises(tmp_path: pathlib.Path):
    d = tmp_path / "broken"
    d.mkdir()
    (d / "SKILL.md").write_text("---\nname:\n---\nbody")
    with pytest.raises(SkillLoadError):
        load_skill_file(d / "SKILL.md")


# --- registry scan --------------------------------------------------------


def test_registry_scans_multiple_skills(tmp_path: pathlib.Path):
    _write_skill(tmp_path, "a", "A body")
    _write_skill(tmp_path, "b", "B body")
    reg = SkillRegistry(skills_dir=tmp_path)
    names = [s.name for s in reg.list_skills()]
    assert names == ["a", "b"]


def test_registry_skips_broken_but_loads_rest(tmp_path: pathlib.Path):
    _write_skill(tmp_path, "ok", "fine body")
    broken_dir = tmp_path / "broken"
    broken_dir.mkdir()
    (broken_dir / "SKILL.md").write_text("---\nname:\n---\n")
    reg = SkillRegistry(skills_dir=tmp_path)
    names = [s.name for s in reg.list_skills()]
    assert names == ["ok"]
    assert len(reg.errors()) == 1


def test_registry_directory_without_skill_md_is_ignored(tmp_path: pathlib.Path):
    (tmp_path / "nonskill").mkdir()
    (tmp_path / "nonskill" / "README.md").write_text("hi")
    reg = SkillRegistry(skills_dir=tmp_path)
    assert reg.list_skills() == []


# --- invoke / render ------------------------------------------------------


def test_invoke_renders_jinja2_context(tmp_path: pathlib.Path):
    _write_skill(
        tmp_path,
        "greet",
        "Hello {{ args.target | default('world') }} from {{ cwd | default('?') }}",
    )
    reg = SkillRegistry(skills_dir=tmp_path)
    result = reg.invoke("greet", args={"target": "ash"}, context={"cwd": "/workspace"})
    assert "Hello ash from /workspace" in result.rendered_prompt


def test_invoke_unknown_skill_raises_keyerror(tmp_path: pathlib.Path):
    reg = SkillRegistry(skills_dir=tmp_path)
    with pytest.raises(KeyError):
        reg.invoke("nope")


def test_invoke_sandboxed_jinja_blocks_attribute_access(tmp_path: pathlib.Path):
    from jinja2.exceptions import SecurityError

    _write_skill(tmp_path, "sand", "{{ ''.__class__.__mro__ }}")
    reg = SkillRegistry(skills_dir=tmp_path)
    with pytest.raises(SecurityError):
        reg.invoke("sand")


# --- reload + pub/sub -----------------------------------------------------


def test_reload_picks_up_new_skill(tmp_path: pathlib.Path):
    _write_skill(tmp_path, "a", "A")
    reg = SkillRegistry(skills_dir=tmp_path)
    assert len(reg.list_skills()) == 1
    _write_skill(tmp_path, "b", "B")
    loaded, errors = reg.reload()
    assert loaded == 2
    assert errors == []
    assert sorted(s.name for s in reg.list_skills()) == ["a", "b"]


@pytest.mark.asyncio
async def test_subscribe_receives_added_event_on_reload(tmp_path: pathlib.Path):
    reg = SkillRegistry(skills_dir=tmp_path)
    q = reg.subscribe()
    try:
        _write_skill(tmp_path, "new", "N")
        reg.reload()
        import asyncio

        event = await asyncio.wait_for(q.get(), timeout=1.0)
        assert event.kind == SkillEventKind.ADDED
        assert event.name == "new"
    finally:
        reg.unsubscribe(q)


# --- directory layout enforcement ----------------------------------------


def test_bare_markdown_file_is_not_a_skill(tmp_path: pathlib.Path):
    # Q1=a: only <name>/SKILL.md works; a bare <name>.md must be ignored.
    (tmp_path / "bare.md").write_text("---\nname: bare\n---\nbody")
    reg = SkillRegistry(skills_dir=tmp_path)
    assert reg.list_skills() == []
