"""gRPC integration tests (M1 Health + M2 LlmProvider)."""

from __future__ import annotations

import time
from typing import AsyncIterator

import grpc
import pytest
import pytest_asyncio
from grpc import aio as grpc_aio

from ashpy import __version__, _codegen
from ashpy.providers import (
    ChatDelta as PyChatDelta,
    ChatRequest as PyChatRequest,
    LlmProvider,
    ProviderCaps,
    ProviderConfig,
    loader,
)
from ashpy.providers.base import HealthState, HealthStatus
from ashpy.server import API_VERSION, build_server


pytestmark = pytest.mark.asyncio


# --- fakes ----------------------------------------------------------------


class FakeEchoProvider(LlmProvider):
    name = "echo"

    def capabilities(self) -> ProviderCaps:
        return ProviderCaps(
            supports_tools=False,
            supports_vision=False,
            max_context_tokens=1024,
            default_model="echo-1",
        )

    async def health(self) -> HealthStatus:
        return HealthStatus(HealthState.OK)

    async def chat_stream(self, req: PyChatRequest) -> AsyncIterator[PyChatDelta]:
        text = req.messages[-1].content if req.messages else ""
        for ch in text:
            yield PyChatDelta(text=ch)
        yield PyChatDelta(finish_reason="end_turn", input_tokens=1, output_tokens=len(text))


@pytest.fixture
def fake_registry(monkeypatch, tmp_path):
    providers_dir = tmp_path / "providers"
    providers_dir.mkdir()
    reg = loader.ProviderRegistry(providers_dir=providers_dir)
    # Inject a fully-configured fake provider as the active one.
    cfg = ProviderConfig(name="echo", source="builtin")
    reg._configs["echo"] = cfg
    reg._instances["echo"] = FakeEchoProvider(cfg)
    reg._active = "echo"
    monkeypatch.setattr(loader, "_REGISTRY", reg)
    yield reg


@pytest_asyncio.fixture
async def server_bind(fake_registry):
    server, bind = await build_server("127.0.0.1:0")
    await server.start()
    try:
        yield bind
    finally:
        await server.stop(grace=0.1)


def _stubs():
    _codegen.ensure_generated()
    from ashpy._generated import ash_pb2, ash_pb2_grpc  # type: ignore

    return ash_pb2, ash_pb2_grpc


# --- tests ----------------------------------------------------------------


async def test_codegen_produces_stubs():
    out = _codegen.generate(force=True)
    assert (out / "ash_pb2.py").is_file()
    assert (out / "ash_pb2_grpc.py").is_file()


async def test_health_ping(server_bind: str):
    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(server_bind) as channel:
        client = ash_pb2_grpc.HealthStub(channel)
        req = ash_pb2.PingRequest(client="pytest/1.0", sent_unix_ms=int(time.time() * 1000))
        resp = await client.Ping(req, timeout=2.0)
    assert resp.server == f"ashpy/{__version__}"
    assert resp.api_version == API_VERSION
    assert resp.features["health"] == "v1"
    assert resp.features["llm"] == "v1"  # promoted in M2
    assert resp.features["harness"] == "v1"  # promoted in M3


async def test_list_providers_contains_echo(server_bind: str):
    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(server_bind) as channel:
        client = ash_pb2_grpc.LlmProviderStub(channel)
        resp = await client.ListProviders(ash_pb2.ListProvidersRequest(), timeout=2.0)
    names = [p.name for p in resp.providers]
    assert "echo" in names


async def test_chat_stream_roundtrip(server_bind: str):
    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(server_bind) as channel:
        client = ash_pb2_grpc.LlmProviderStub(channel)
        req = ash_pb2.ChatRequest(
            provider="echo",
            model="echo-1",
            messages=[ash_pb2.ChatMessage(role="user", content="hello")],
            temperature=0.0,
        )
        texts: list[str] = []
        finish = None
        async for delta in client.ChatStream(req, timeout=3.0):
            kind = delta.WhichOneof("kind")
            if kind == "text":
                texts.append(delta.text)
            elif kind == "finish":
                finish = delta.finish
    assert "".join(texts) == "hello"
    assert finish is not None
    assert finish.stop_reason == "end_turn"
    assert finish.output_tokens == 5


async def test_switch_provider(server_bind: str, fake_registry):
    # Install a second provider to switch to.
    cfg2 = ProviderConfig(name="echo2", source="builtin")
    fake_registry._configs["echo2"] = cfg2
    fake_registry._instances["echo2"] = FakeEchoProvider(cfg2)

    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(server_bind) as channel:
        client = ash_pb2_grpc.LlmProviderStub(channel)
        resp = await client.Switch(
            ash_pb2.SwitchRequest(provider="echo2", model=""),
            timeout=2.0,
        )
    assert resp.ok is True
    assert fake_registry.active_name() == "echo2"


async def test_skills_still_unimplemented(server_bind: str):
    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(server_bind) as channel:
        client = ash_pb2_grpc.SkillRegistryStub(channel)
        with pytest.raises(grpc.RpcError) as excinfo:
            await client.List(ash_pb2.ListSkillsRequest(), timeout=2.0)
    assert excinfo.value.code() == grpc.StatusCode.UNIMPLEMENTED
