"""FastAPI `/v1/skills*` tests (M5)."""

from __future__ import annotations

import pathlib

import pytest
from fastapi.testclient import TestClient

from ashpy.api import create_app
from ashpy.skills import reset_registry_for_tests


def _write_skill(root: pathlib.Path, name: str, body: str) -> None:
    d = root / name
    d.mkdir(parents=True, exist_ok=True)
    (d / "SKILL.md").write_text(
        f"---\nname: {name}\ndescription: {name}\nallowed_tools: ['bash']\n"
        f"---\n{body}\n"
    )


@pytest.fixture
def client(tmp_path: pathlib.Path):
    _write_skill(tmp_path, "hello", "hi {{ args.who | default('anon') }}")
    _write_skill(tmp_path, "bye", "bye")
    reset_registry_for_tests(skills_dir=tmp_path)
    app = create_app(query_host_endpoint="127.0.0.1:1")
    return TestClient(app)


def test_list_skills(client: TestClient):
    resp = client.get("/v1/skills")
    assert resp.status_code == 200
    names = sorted(s["name"] for s in resp.json()["skills"])
    assert names == ["bye", "hello"]


def test_get_skill_detail(client: TestClient):
    resp = client.get("/v1/skills/hello")
    assert resp.status_code == 200
    body = resp.json()
    assert body["name"] == "hello"
    assert "hi " in body["body"]


def test_get_unknown_skill_404(client: TestClient):
    resp = client.get("/v1/skills/ghost")
    assert resp.status_code == 404


def test_invoke_renders(client: TestClient):
    resp = client.post("/v1/skills/hello/invoke", json={"args": {"who": "ash"}})
    assert resp.status_code == 200
    body = resp.json()
    assert "hi ash" in body["rendered_prompt"]
    assert body["allowed_tools"] == ["bash"]


def test_invoke_unknown_404(client: TestClient):
    resp = client.post("/v1/skills/ghost/invoke", json={"args": {}})
    assert resp.status_code == 404


def test_reload_returns_count(client: TestClient):
    resp = client.post("/v1/skills/reload")
    assert resp.status_code == 200
    body = resp.json()
    assert body["loaded"] == 2


def test_openapi_includes_skill_paths(client: TestClient):
    resp = client.get("/openapi.json")
    spec = resp.json()
    paths = spec["paths"]
    for required in ("/v1/skills", "/v1/skills/{name}", "/v1/skills/{name}/invoke", "/v1/skills/reload"):
        assert required in paths


def test_features_skills_is_v1(client: TestClient):
    resp = client.get("/v1/health")
    assert resp.json()["features"]["skills"] == "v1"
