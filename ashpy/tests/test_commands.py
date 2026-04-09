"""Unit tests for commands loader + registry (M6)."""

from __future__ import annotations

import asyncio
import pathlib

import pytest

from ashpy.commands import CommandRegistry
from ashpy.commands.loader import CommandLoadError, load_command_file
from ashpy.commands.schema import CommandEventKind


def _write_toml(root: pathlib.Path, name: str, prompt: str, **extra) -> pathlib.Path:
    lines = [f'name = "{name}"', f'description = "{name} command"']
    for k, v in extra.items():
        if isinstance(v, list):
            items = ", ".join(f'"{x}"' for x in v)
            lines.append(f"{k} = [{items}]")
        elif isinstance(v, str):
            lines.append(f'{k} = "{v}"')
        else:
            lines.append(f"{k} = {v}")
    lines.append('prompt = """')
    lines.append(prompt)
    lines.append('"""')
    path = root / f"{name}.toml"
    path.write_text("\n".join(lines) + "\n")
    return path


# --- loader ---------------------------------------------------------------


def test_load_valid_command(tmp_path: pathlib.Path):
    path = _write_toml(
        tmp_path,
        "review",
        "Focus: {{ args.focus | default('quality') }}",
        allowed_tools=["bash", "grep"],
        model="claude-opus-4-5",
    )
    cmd = load_command_file(path)
    assert cmd.name == "review"
    assert cmd.allowed_tools == ["bash", "grep"]
    assert cmd.model == "claude-opus-4-5"
    assert "Focus" in cmd.prompt


def test_load_without_name_falls_back_to_stem(tmp_path: pathlib.Path):
    path = tmp_path / "orphan.toml"
    path.write_text('description = "x"\nprompt = "body"\n')
    cmd = load_command_file(path)
    assert cmd.name == "orphan"


def test_load_invalid_toml_raises(tmp_path: pathlib.Path):
    path = tmp_path / "broken.toml"
    path.write_text("name = \nprompt = ")
    with pytest.raises(CommandLoadError):
        load_command_file(path)


def test_load_missing_prompt_raises(tmp_path: pathlib.Path):
    path = tmp_path / "empty.toml"
    path.write_text('name = "empty"\n')
    with pytest.raises(CommandLoadError):
        load_command_file(path)


# --- registry scan --------------------------------------------------------


def test_registry_scans_multiple(tmp_path: pathlib.Path):
    _write_toml(tmp_path, "a", "A body")
    _write_toml(tmp_path, "b", "B body")
    reg = CommandRegistry(commands_dir=tmp_path)
    names = [c.name for c in reg.list_commands()]
    assert names == ["a", "b"]


def test_registry_skips_broken_keeps_rest(tmp_path: pathlib.Path):
    _write_toml(tmp_path, "ok", "fine body")
    (tmp_path / "broken.toml").write_text("name = \n")
    reg = CommandRegistry(commands_dir=tmp_path)
    assert [c.name for c in reg.list_commands()] == ["ok"]
    assert len(reg.errors()) == 1


def test_registry_ignores_non_toml_files(tmp_path: pathlib.Path):
    (tmp_path / "README.md").write_text("hi")
    (tmp_path / "notes.txt").write_text("hi")
    _write_toml(tmp_path, "real", "R body")
    reg = CommandRegistry(commands_dir=tmp_path)
    assert [c.name for c in reg.list_commands()] == ["real"]


def test_registry_ignores_hidden_toml(tmp_path: pathlib.Path):
    _write_toml(tmp_path, ".secret", "hidden")
    _write_toml(tmp_path, "visible", "v")
    reg = CommandRegistry(commands_dir=tmp_path)
    assert [c.name for c in reg.list_commands()] == ["visible"]


# --- render ---------------------------------------------------------------


def test_render_uses_args_and_context(tmp_path: pathlib.Path):
    _write_toml(
        tmp_path,
        "greet",
        "Hi {{ args.who }} at {{ context.cwd }}",
    )
    reg = CommandRegistry(commands_dir=tmp_path)
    result = reg.render("greet", args={"who": "ash"}, context={"cwd": "/workspace"})
    assert "Hi ash at /workspace" in result.rendered_prompt


def test_render_unknown_raises_keyerror(tmp_path: pathlib.Path):
    reg = CommandRegistry(commands_dir=tmp_path)
    with pytest.raises(KeyError):
        reg.render("ghost")


def test_render_sandbox_blocks_attribute_escape(tmp_path: pathlib.Path):
    from jinja2.exceptions import SecurityError

    _write_toml(tmp_path, "sand", "{{ ''.__class__.__mro__ }}")
    reg = CommandRegistry(commands_dir=tmp_path)
    with pytest.raises(SecurityError):
        reg.render("sand")


# --- reload + pub/sub -----------------------------------------------------


def test_reload_picks_up_new_command(tmp_path: pathlib.Path):
    _write_toml(tmp_path, "a", "A")
    reg = CommandRegistry(commands_dir=tmp_path)
    assert len(reg.list_commands()) == 1
    _write_toml(tmp_path, "b", "B")
    loaded, errors = reg.reload()
    assert loaded == 2
    assert errors == []
    assert sorted(c.name for c in reg.list_commands()) == ["a", "b"]


@pytest.mark.asyncio
async def test_subscribe_receives_added_event(tmp_path: pathlib.Path):
    reg = CommandRegistry(commands_dir=tmp_path)
    q = reg.subscribe()
    try:
        _write_toml(tmp_path, "new", "N")
        reg.reload()
        event = await asyncio.wait_for(q.get(), timeout=1.0)
        assert event.kind == CommandEventKind.ADDED
        assert event.name == "new"
    finally:
        reg.unsubscribe(q)
