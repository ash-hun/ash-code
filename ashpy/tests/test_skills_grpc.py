"""gRPC integration tests for SkillRegistryServicer (M5)."""

from __future__ import annotations

import asyncio
import pathlib

import grpc
import pytest
import pytest_asyncio
from grpc import aio as grpc_aio

from ashpy import _codegen
from ashpy.middleware import MiddlewareChain, allow
from ashpy.middleware.base import Middleware
from ashpy.server import (
    build_server,
    reset_middleware_chain_for_tests,
)
from ashpy.skills import reset_registry_for_tests

pytestmark = pytest.mark.asyncio


def _write_skill(root: pathlib.Path, name: str, body: str) -> None:
    d = root / name
    d.mkdir(parents=True, exist_ok=True)
    (d / "SKILL.md").write_text(
        f"---\nname: {name}\ndescription: {name} test\ntriggers: ['{name}']\n"
        f"allowed_tools: ['bash']\n---\n{body}\n"
    )


@pytest_asyncio.fixture
async def server_bind(tmp_path: pathlib.Path):
    _write_skill(tmp_path, "alpha", "Hello {{ args.who | default('anon') }}")
    _write_skill(tmp_path, "beta", "Beta body")
    reset_registry_for_tests(skills_dir=tmp_path)

    class AllowAll(Middleware):
        priority = 1
        name = "allow_all"

    reset_middleware_chain_for_tests(MiddlewareChain([AllowAll()]))

    server, bind = await build_server("127.0.0.1:0")
    await server.start()
    try:
        yield bind, tmp_path
    finally:
        await server.stop(grace=0.1)
        reset_middleware_chain_for_tests(None)


def _stubs():
    _codegen.ensure_generated()
    from ashpy._generated import ash_pb2, ash_pb2_grpc  # type: ignore

    return ash_pb2, ash_pb2_grpc


async def test_features_skills_is_v1(server_bind):
    bind, _ = server_bind
    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(bind) as ch:
        resp = await ash_pb2_grpc.HealthStub(ch).Ping(
            ash_pb2.PingRequest(client="pytest"), timeout=2.0
        )
    assert resp.features["skills"] == "v1"


async def test_list_returns_two_skills(server_bind):
    bind, _ = server_bind
    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(bind) as ch:
        stub = ash_pb2_grpc.SkillRegistryStub(ch)
        resp = await stub.List(ash_pb2.ListSkillsRequest(), timeout=2.0)
    names = sorted(s.name for s in resp.skills)
    assert names == ["alpha", "beta"]


async def test_invoke_renders_prompt(server_bind):
    bind, _ = server_bind
    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(bind) as ch:
        stub = ash_pb2_grpc.SkillRegistryStub(ch)
        resp = await stub.Invoke(
            ash_pb2.InvokeSkillRequest(name="alpha", args={"who": "ash"}),
            timeout=2.0,
        )
    assert "Hello ash" in resp.rendered_prompt
    assert resp.allowed_tools == ["bash"]


async def test_invoke_unknown_returns_not_found(server_bind):
    bind, _ = server_bind
    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(bind) as ch:
        stub = ash_pb2_grpc.SkillRegistryStub(ch)
        with pytest.raises(grpc.RpcError) as excinfo:
            await stub.Invoke(
                ash_pb2.InvokeSkillRequest(name="ghost"), timeout=2.0
            )
    assert excinfo.value.code() == grpc.StatusCode.NOT_FOUND


async def test_reload_returns_count(server_bind):
    bind, tmp_path = server_bind
    _write_skill(tmp_path, "gamma", "G")
    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(bind) as ch:
        stub = ash_pb2_grpc.SkillRegistryStub(ch)
        resp = await stub.Reload(ash_pb2.ReloadRequest(), timeout=2.0)
    assert resp.loaded == 3
    assert list(resp.errors) == []


async def test_watch_receives_added_event(server_bind):
    bind, tmp_path = server_bind
    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(bind) as ch:
        stub = ash_pb2_grpc.SkillRegistryStub(ch)
        call = stub.Watch(ash_pb2.WatchRequest())
        # Give the stream a tick to register its subscriber.
        await asyncio.sleep(0.05)

        _write_skill(tmp_path, "delta", "D")
        # Loader doesn't auto-watch in this test (no SkillWatcher); trigger reload.
        await stub.Reload(ash_pb2.ReloadRequest(), timeout=2.0)

        async def _first_event():
            async for event in call:
                return event

        event = await asyncio.wait_for(_first_event(), timeout=3.0)
        assert event.kind == ash_pb2.SkillEvent.ADDED
        assert event.name == "delta"
        call.cancel()
