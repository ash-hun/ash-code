"""FastAPI layer tests (M4).

Uses ``fastapi.testclient.TestClient`` — sync, no server boot required.
``/v1/chat`` is NOT covered here because it requires a running Rust
QueryHost gRPC server; that path is verified via the Docker E2E
scenario in the M4 task report.
"""

from __future__ import annotations

import pytest
from fastapi.testclient import TestClient

from ashpy import __version__
from ashpy.api import create_app


@pytest.fixture
def client():
    app = create_app(query_host_endpoint="127.0.0.1:1")  # unreachable is fine for these tests
    return TestClient(app)


def test_health_endpoint(client: TestClient):
    resp = client.get("/v1/health")
    assert resp.status_code == 200
    body = resp.json()
    assert body["status"] == "ok"
    assert body["ashpy_version"] == __version__
    assert body["api_version"] == "v1"
    assert body["features"]["http"] == "v1"
    assert body["features"]["llm"] == "v1"
    assert body["features"]["harness"] == "v1"


def test_list_providers_returns_four_builtins(client: TestClient):
    resp = client.get("/v1/llm/providers")
    assert resp.status_code == 200
    body = resp.json()
    names = sorted(p["name"] for p in body["providers"])
    assert names == ["anthropic", "ollama", "openai", "vllm"]
    for p in body["providers"]:
        assert p["source"] == "builtin"


def test_switch_provider_success(client: TestClient):
    resp = client.post(
        "/v1/llm/switch", json={"provider": "openai", "model": "gpt-4.1"}
    )
    assert resp.status_code == 200
    body = resp.json()
    assert body["ok"] is True
    assert "openai" in body["message"]


def test_switch_provider_unknown(client: TestClient):
    resp = client.post("/v1/llm/switch", json={"provider": "not-a-real-provider"})
    assert resp.status_code == 404


def test_openapi_json_is_valid_and_covers_core_paths(client: TestClient):
    resp = client.get("/openapi.json")
    assert resp.status_code == 200
    spec = resp.json()
    assert spec["openapi"].startswith("3.")
    assert spec["info"]["title"] == "ash-code API"
    paths = spec["paths"]
    for required in ("/v1/health", "/v1/llm/providers", "/v1/llm/switch", "/v1/chat", "/v1/sessions"):
        assert required in paths, f"missing path {required}"


def test_swagger_ui_served(client: TestClient):
    resp = client.get("/docs")
    assert resp.status_code == 200
    assert "swagger" in resp.text.lower()
