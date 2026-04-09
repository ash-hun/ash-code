"""M1 gRPC integration tests.

Stand up a real in-process ashpy gRPC server on an ephemeral port and
verify ``Health.Ping`` + placeholder behavior for services that land in
later milestones.
"""

from __future__ import annotations

import time

import grpc
import pytest

from ashpy import __version__, _codegen
from ashpy.server import API_VERSION, build_server


@pytest.fixture(scope="module")
def server_bind():
    server, bind = build_server("127.0.0.1:0")
    server.start()
    try:
        yield bind
    finally:
        server.stop(grace=0.1)


def _stubs():
    _codegen.ensure_generated()
    from ashpy._generated import ash_pb2, ash_pb2_grpc  # type: ignore

    return ash_pb2, ash_pb2_grpc


def test_codegen_produces_stubs():
    out = _codegen.generate(force=True)
    assert (out / "ash_pb2.py").is_file()
    assert (out / "ash_pb2_grpc.py").is_file()


def test_health_ping(server_bind: str):
    ash_pb2, ash_pb2_grpc = _stubs()
    with grpc.insecure_channel(server_bind) as channel:
        client = ash_pb2_grpc.HealthStub(channel)
        req = ash_pb2.PingRequest(client="pytest/1.0", sent_unix_ms=int(time.time() * 1000))
        resp = client.Ping(req, timeout=2.0)
    assert resp.server == f"ashpy/{__version__}"
    assert resp.api_version == API_VERSION
    assert resp.features["health"] == "v1"
    assert resp.features["llm"] == "planned"
    assert resp.features["harness"] == "planned"


def test_llm_chat_stream_is_unimplemented(server_bind: str):
    ash_pb2, ash_pb2_grpc = _stubs()
    with grpc.insecure_channel(server_bind) as channel:
        client = ash_pb2_grpc.LlmProviderStub(channel)
        req = ash_pb2.ChatRequest(provider="anthropic", model="claude-opus-4-6")
        with pytest.raises(grpc.RpcError) as excinfo:
            for _ in client.ChatStream(req, timeout=2.0):
                pass
    assert excinfo.value.code() == grpc.StatusCode.UNIMPLEMENTED
    assert "M2" in (excinfo.value.details() or "")


def test_harness_hook_is_unimplemented(server_bind: str):
    ash_pb2, ash_pb2_grpc = _stubs()
    with grpc.insecure_channel(server_bind) as channel:
        client = ash_pb2_grpc.HarnessStub(channel)
        with pytest.raises(grpc.RpcError) as excinfo:
            client.OnTurnStart(ash_pb2.TurnContext(), timeout=2.0)
    assert excinfo.value.code() == grpc.StatusCode.UNIMPLEMENTED
