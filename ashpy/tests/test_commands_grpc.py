"""gRPC integration tests for CommandRegistryServicer (M6)."""

from __future__ import annotations

import pathlib

import grpc
import pytest
import pytest_asyncio
from grpc import aio as grpc_aio

from ashpy import _codegen
from ashpy.commands import reset_registry_for_tests as reset_command_registry
from ashpy.middleware import MiddlewareChain
from ashpy.middleware.base import Middleware
from ashpy.server import build_server, reset_middleware_chain_for_tests

pytestmark = pytest.mark.asyncio


def _write_toml(root: pathlib.Path, name: str, prompt: str, tools=None) -> None:
    tool_str = ""
    if tools:
        items = ", ".join(f'"{t}"' for t in tools)
        tool_str = f"allowed_tools = [{items}]\n"
    (root / f"{name}.toml").write_text(
        f'name = "{name}"\ndescription = "{name} test"\n'
        f'{tool_str}'
        f'prompt = """\n{prompt}\n"""\n'
    )


@pytest_asyncio.fixture
async def server_bind(tmp_path: pathlib.Path):
    _write_toml(tmp_path, "alpha", "Hi {{ args.who | default('anon') }}", tools=["bash"])
    _write_toml(tmp_path, "beta", "Beta")
    reset_command_registry(commands_dir=tmp_path)

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


async def test_features_commands_is_v1(server_bind):
    bind, _ = server_bind
    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(bind) as ch:
        resp = await ash_pb2_grpc.HealthStub(ch).Ping(
            ash_pb2.PingRequest(client="pytest"), timeout=2.0
        )
    assert resp.features["commands"] == "v1"


async def test_list_returns_two(server_bind):
    bind, _ = server_bind
    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(bind) as ch:
        stub = ash_pb2_grpc.CommandRegistryStub(ch)
        resp = await stub.List(ash_pb2.ListCommandsRequest(), timeout=2.0)
    names = sorted(c.name for c in resp.commands)
    assert names == ["alpha", "beta"]


async def test_run_renders_prompt(server_bind):
    bind, _ = server_bind
    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(bind) as ch:
        stub = ash_pb2_grpc.CommandRegistryStub(ch)
        resp = await stub.Run(
            ash_pb2.RunCommandRequest(name="alpha", args={"who": "ash"}),
            timeout=2.0,
        )
    assert "Hi ash" in resp.rendered_prompt
    assert resp.allowed_tools == ["bash"]


async def test_run_unknown_returns_not_found(server_bind):
    bind, _ = server_bind
    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(bind) as ch:
        stub = ash_pb2_grpc.CommandRegistryStub(ch)
        with pytest.raises(grpc.RpcError) as excinfo:
            await stub.Run(
                ash_pb2.RunCommandRequest(name="ghost"), timeout=2.0
            )
    assert excinfo.value.code() == grpc.StatusCode.NOT_FOUND


async def test_reload_returns_count(server_bind):
    bind, tmp_path = server_bind
    _write_toml(tmp_path, "gamma", "G")
    ash_pb2, ash_pb2_grpc = _stubs()
    async with grpc_aio.insecure_channel(bind) as ch:
        stub = ash_pb2_grpc.CommandRegistryStub(ch)
        resp = await stub.Reload(ash_pb2.ReloadRequest(), timeout=2.0)
    assert resp.loaded == 3
    assert list(resp.errors) == []
