"""gRPC integration: Harness + middleware end-to-end (M3).

Uses an inline in-test ``EchoProvider`` — no network, no dependency on a
shipped ``fake`` built-in. This mirrors the ``FakeEchoProvider`` pattern
already used in ``test_grpc.py``.
"""

from __future__ import annotations

from typing import AsyncIterator

import grpc
import pytest
import pytest_asyncio
from grpc import aio as grpc_aio

from ashpy import _codegen
from ashpy.middleware import MiddlewareChain, allow, deny
from ashpy.middleware.base import (
    DecisionKind,
    Middleware,
    ToolCallEvent,
)
from ashpy.providers import loader
from ashpy.providers.base import (
    ChatDelta as PyChatDelta,
    ChatRequest as PyChatRequest,
    HealthState,
    HealthStatus,
    LlmProvider,
    ProviderCaps,
    ProviderConfig,
)
from ashpy.server import build_server, reset_middleware_chain_for_tests

pytestmark = pytest.mark.asyncio


# --- inline test provider (no network) ----------------------------------


class EchoProvider(LlmProvider):
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
        last = req.messages[-1].content if req.messages else ""
        for ch in last:
            yield PyChatDelta(text=ch)
        yield PyChatDelta(
            finish_reason="end_turn", input_tokens=1, output_tokens=len(last)
        )


# --- fixtures --------------------------------------------------------------


@pytest.fixture
def echo_registry(monkeypatch, tmp_path):
    providers_dir = tmp_path / "providers"
    providers_dir.mkdir()
    reg = loader.ProviderRegistry(providers_dir=providers_dir)
    cfg = ProviderConfig(name="echo", source="builtin", defaults={"model": "echo-1"})
    reg._configs["echo"] = cfg
    reg._instances["echo"] = EchoProvider(cfg)
    reg._active = "echo"
    monkeypatch.setattr(loader, "_REGISTRY", reg)
    yield reg


@pytest.fixture
def permissive_middleware():
    class AllowAll(Middleware):
        priority = 1
        name = "allow_all"

    chain = MiddlewareChain([AllowAll()])
    reset_middleware_chain_for_tests(chain)
    yield chain
    reset_middleware_chain_for_tests(None)


@pytest.fixture
def denying_bash_middleware():
    class DenyBash(Middleware):
        priority = 1
        name = "deny_bash"

        async def on_tool_call(self, event: ToolCallEvent):
            if event.tool_name == "bash":
                return deny("test: no bash for you")
            return allow()

    chain = MiddlewareChain([DenyBash()])
    reset_middleware_chain_for_tests(chain)
    yield chain
    reset_middleware_chain_for_tests(None)


@pytest_asyncio.fixture
async def server_bind(echo_registry, permissive_middleware):
    server, bind = await build_server("127.0.0.1:0")
    await server.start()
    try:
        yield bind
    finally:
        await server.stop(grace=0.1)


@pytest_asyncio.fixture
async def deny_bash_server_bind(echo_registry, denying_bash_middleware):
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


# --- tests -----------------------------------------------------------------


async def test_features_harness_is_v1(server_bind):
    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(server_bind) as channel:
        resp = await ash_pb2_grpc.HealthStub(channel).Ping(
            ash_pb2.PingRequest(client="pytest"), timeout=2.0
        )
    assert resp.features["harness"] == "v1"


async def test_on_turn_start_allow_via_allow_all(server_bind):
    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(server_bind) as channel:
        client = ash_pb2_grpc.HarnessStub(channel)
        resp = await client.OnTurnStart(
            ash_pb2.TurnContext(session_id="s", turn_id="t", provider="echo", model="echo-1"),
            timeout=2.0,
        )
    assert resp.kind == int(DecisionKind.ALLOW)


async def test_on_tool_call_deny_via_deny_bash(deny_bash_server_bind):
    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(deny_bash_server_bind) as channel:
        client = ash_pb2_grpc.HarnessStub(channel)
        req = ash_pb2.ToolCallEvent(
            session_id="s",
            turn_id="t",
            call=ash_pb2.ToolCall(id="c1", name="bash", arguments=b'{"command":"ls"}'),
        )
        resp = await client.OnToolCall(req, timeout=2.0)
    assert resp.kind == int(DecisionKind.DENY)
    assert "no bash" in resp.reason


async def test_chat_stream_text_echo_roundtrip(server_bind):
    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(server_bind) as channel:
        llm = ash_pb2_grpc.LlmProviderStub(channel)
        req = ash_pb2.ChatRequest(
            provider="echo",
            model="echo-1",
            messages=[ash_pb2.ChatMessage(role="user", content="hello")],
        )
        collected = ""
        finish = None
        async for delta in llm.ChatStream(req, timeout=3.0):
            kind = delta.WhichOneof("kind")
            if kind == "text":
                collected += delta.text
            elif kind == "finish":
                finish = delta.finish
    assert collected == "hello"
    assert finish is not None
    assert finish.stop_reason == "end_turn"
    assert finish.output_tokens == 5
