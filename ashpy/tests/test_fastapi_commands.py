"""FastAPI `/v1/commands*` tests (M6)."""

from __future__ import annotations

import pathlib

import pytest
from fastapi.testclient import TestClient

from ashpy.api import create_app
from ashpy.commands import reset_registry_for_tests as reset_command_registry


def _write_toml(root: pathlib.Path, name: str, prompt: str, tools=None, model=None) -> None:
    lines = [f'name = "{name}"', f'description = "{name}"']
    if tools:
        items = ", ".join(f'"{t}"' for t in tools)
        lines.append(f"allowed_tools = [{items}]")
    if model:
        lines.append(f'model = "{model}"')
    lines.append('prompt = """')
    lines.append(prompt)
    lines.append('"""')
    (root / f"{name}.toml").write_text("\n".join(lines) + "\n")


@pytest.fixture
def client(tmp_path: pathlib.Path):
    _write_toml(
        tmp_path,
        "review",
        "Focus: {{ args.focus | default('quality') }}",
        tools=["bash", "grep"],
        model="claude-opus-4-5",
    )
    _write_toml(tmp_path, "summarize", "Summarize", tools=["file_read"])
    reset_command_registry(commands_dir=tmp_path)
    app = create_app(query_host_endpoint="127.0.0.1:1")
    return TestClient(app)


def test_list_commands(client: TestClient):
    resp = client.get("/v1/commands")
    assert resp.status_code == 200
    names = sorted(c["name"] for c in resp.json()["commands"])
    assert names == ["review", "summarize"]


def test_command_detail(client: TestClient):
    resp = client.get("/v1/commands/review")
    assert resp.status_code == 200
    body = resp.json()
    assert body["name"] == "review"
    assert body["allowed_tools"] == ["bash", "grep"]
    assert body["model"] == "claude-opus-4-5"
    assert "Focus:" in body["prompt"]


def test_command_detail_404(client: TestClient):
    resp = client.get("/v1/commands/ghost")
    assert resp.status_code == 404


def test_render_command(client: TestClient):
    resp = client.post(
        "/v1/commands/review/render",
        json={"args": {"focus": "auth"}},
    )
    assert resp.status_code == 200
    body = resp.json()
    assert "Focus: auth" in body["rendered_prompt"]
    assert body["allowed_tools"] == ["bash", "grep"]
    assert body["model"] == "claude-opus-4-5"


def test_render_unknown_404(client: TestClient):
    resp = client.post("/v1/commands/ghost/render", json={"args": {}})
    assert resp.status_code == 404


def test_render_sandbox_escape_returns_400(client: TestClient, tmp_path: pathlib.Path):
    # Drop a malicious command and reload.
    _write_toml(tmp_path, "bad", "{{ ''.__class__.__mro__ }}")
    client.post("/v1/commands/reload")
    resp = client.post("/v1/commands/bad/render", json={"args": {}})
    assert resp.status_code == 400


def test_reload_returns_count(client: TestClient):
    resp = client.post("/v1/commands/reload")
    assert resp.status_code == 200
    assert resp.json()["loaded"] == 2


def test_openapi_includes_command_paths(client: TestClient):
    spec = client.get("/openapi.json").json()
    paths = spec["paths"]
    for required in (
        "/v1/commands",
        "/v1/commands/{name}",
        "/v1/commands/{name}/render",
        "/v1/commands/{name}/run",
        "/v1/commands/reload",
    ):
        assert required in paths


def test_features_commands_is_v1(client: TestClient):
    resp = client.get("/v1/health")
    assert resp.json()["features"]["commands"] == "v1"
