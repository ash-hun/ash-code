"""Tests for `POST /v1/commands/{name}/run` (SSE) with a fake QueryHostClient.

The real endpoint calls back into the Rust QueryHost gRPC. We monkey-
patch ``app.state.query_client`` with a fake async iterator so the test
can run entirely in-process without a running Rust server.
"""

from __future__ import annotations

import pathlib
from typing import AsyncIterator

import pytest
from fastapi.testclient import TestClient

from ashpy.api import create_app
from ashpy.commands import reset_registry_for_tests as reset_command_registry


def _write_toml(root: pathlib.Path, name: str, prompt: str) -> None:
    (root / f"{name}.toml").write_text(
        f'name = "{name}"\ndescription = "{name}"\n'
        f'prompt = """\n{prompt}\n"""\n'
    )


class FakeQueryHostClient:
    def __init__(self) -> None:
        self.last_prompt: str | None = None
        self.last_model: str | None = None

    async def run_turn(
        self,
        session_id: str,
        prompt: str,
        provider: str = "",
        model: str = "",
        reset_session: bool = False,
    ) -> AsyncIterator[dict]:
        self.last_prompt = prompt
        self.last_model = model
        yield {"type": "text", "text": "ok"}
        yield {
            "type": "finish",
            "stop_reason": "end_turn",
            "input_tokens": 1,
            "output_tokens": 1,
        }
        yield {
            "type": "outcome",
            "stop_reason": "end_turn",
            "turns_taken": 1,
            "denied": False,
            "denial_reason": "",
        }

    async def close(self) -> None:
        pass


@pytest.fixture
def client_with_fake(tmp_path: pathlib.Path):
    _write_toml(
        tmp_path, "echo", 'Say hi to {{ args.who | default("anon") }}'
    )
    reset_command_registry(commands_dir=tmp_path)
    app = create_app(query_host_endpoint="127.0.0.1:1")
    fake = FakeQueryHostClient()
    app.state.query_client = fake
    return TestClient(app), fake


def _parse_sse(body: str) -> list[tuple[str, str]]:
    events: list[tuple[str, str]] = []
    current_event = ""
    for line in body.splitlines():
        if line.startswith("event:"):
            current_event = line.split(":", 1)[1].strip()
        elif line.startswith("data:"):
            data = line.split(":", 1)[1].strip()
            events.append((current_event, data))
            current_event = ""
    return events


def test_run_streams_events_from_queryhost(client_with_fake):
    client, fake = client_with_fake
    resp = client.post(
        "/v1/commands/echo/run",
        json={"args": {"who": "ash"}, "session_id": "s1"},
    )
    assert resp.status_code == 200
    events = _parse_sse(resp.text)
    event_types = [e for e, _ in events]
    assert "text" in event_types
    assert "finish" in event_types
    assert "outcome" in event_types
    assert "done" in event_types
    # The fake recorded the rendered prompt it received.
    assert fake.last_prompt is not None
    assert "Say hi to ash" in fake.last_prompt


def test_run_unknown_404(client_with_fake):
    client, _ = client_with_fake
    resp = client.post("/v1/commands/ghost/run", json={"args": {}})
    assert resp.status_code == 404


def test_run_uses_command_model_when_request_omits(client_with_fake, tmp_path: pathlib.Path):
    # Rewrite the existing command to have an explicit model field.
    _write_toml(
        tmp_path, "echo", 'Say hi to {{ args.who | default("anon") }}'
    )
    (tmp_path / "echo.toml").write_text(
        'name = "echo"\ndescription = "echo"\nmodel = "claude-opus-4-5"\n'
        'prompt = """\nSay hi to {{ args.who | default("anon") }}\n"""\n'
    )
    client, fake = client_with_fake
    client.post("/v1/commands/reload")
    resp = client.post(
        "/v1/commands/echo/run",
        json={"args": {"who": "ash"}},
    )
    assert resp.status_code == 200
    assert fake.last_model == "claude-opus-4-5"
