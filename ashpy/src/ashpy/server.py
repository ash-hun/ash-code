"""ashpy gRPC server.

M1 scope:
  * Implements ``Health.Ping`` for real — used by ``ash doctor --check-sidecar``.
  * Registers every other service (``LlmProvider`` / ``SkillRegistry`` /
    ``CommandRegistry`` / ``Harness`` / ``ToolRegistry``) as a placeholder
    that returns ``UNIMPLEMENTED`` until its owning milestone fills it in.
  * Handles SIGTERM/SIGINT gracefully so ``supervisord`` can restart cleanly.

The generated gRPC stubs are produced on-the-fly by :mod:`ashpy._codegen`
on first import, so there are no checked-in generated files to drift.
"""

from __future__ import annotations

import asyncio
import signal
import sys
import time
from concurrent.futures import ThreadPoolExecutor

import grpc

from . import __version__, _codegen

DEFAULT_BIND = "127.0.0.1:50051"
API_VERSION = "v1"


def _log(msg: str) -> None:
    print(f"[ashpy] {msg}", flush=True)


# --- Service implementations ----------------------------------------------


def _build_servicers():
    """Import generated stubs lazily so that ``import ashpy`` stays cheap."""
    _codegen.ensure_generated()
    from ashpy._generated import ash_pb2, ash_pb2_grpc  # type: ignore

    class HealthServicer(ash_pb2_grpc.HealthServicer):
        def Ping(self, request, context):  # noqa: N802 — grpc naming
            return ash_pb2.PingResponse(
                server=f"ashpy/{__version__}",
                api_version=API_VERSION,
                received_unix_ms=int(time.time() * 1000),
                features={
                    "health": "v1",
                    # Advertise the next milestones as 'planned' so clients can
                    # distinguish "not yet" from "unknown".
                    "llm": "planned",
                    "skills": "planned",
                    "commands": "planned",
                    "harness": "planned",
                    "tools": "planned",
                },
            )

    def _unimplemented(service: str):
        def _stub(self, request, context):  # noqa: ARG001
            context.set_code(grpc.StatusCode.UNIMPLEMENTED)
            context.set_details(f"{service} lands in a later milestone")
            return ash_pb2.Empty() if hasattr(ash_pb2, "Empty") else ash_pb2.ReloadResponse()

        return _stub

    class LlmProviderServicer(ash_pb2_grpc.LlmProviderServicer):
        ListProviders = _unimplemented("LlmProvider.ListProviders")
        Capabilities = _unimplemented("LlmProvider.Capabilities")
        Switch = _unimplemented("LlmProvider.Switch")

        def ChatStream(self, request, context):  # noqa: N802
            context.set_code(grpc.StatusCode.UNIMPLEMENTED)
            context.set_details("LlmProvider.ChatStream lands in M2")
            return
            yield  # pragma: no cover — generator marker

    class SkillRegistryServicer(ash_pb2_grpc.SkillRegistryServicer):
        List = _unimplemented("SkillRegistry.List")
        Invoke = _unimplemented("SkillRegistry.Invoke")
        Reload = _unimplemented("SkillRegistry.Reload")

        def Watch(self, request, context):  # noqa: N802
            context.set_code(grpc.StatusCode.UNIMPLEMENTED)
            context.set_details("SkillRegistry.Watch lands in M5")
            return
            yield  # pragma: no cover

    class CommandRegistryServicer(ash_pb2_grpc.CommandRegistryServicer):
        List = _unimplemented("CommandRegistry.List")
        Run = _unimplemented("CommandRegistry.Run")
        Reload = _unimplemented("CommandRegistry.Reload")

    class HarnessServicer(ash_pb2_grpc.HarnessServicer):
        OnTurnStart = _unimplemented("Harness.OnTurnStart")
        OnToolCall = _unimplemented("Harness.OnToolCall")
        OnStreamDelta = _unimplemented("Harness.OnStreamDelta")
        OnTurnEnd = _unimplemented("Harness.OnTurnEnd")

    class ToolRegistryServicer(ash_pb2_grpc.ToolRegistryServicer):
        List = _unimplemented("ToolRegistry.List")
        Invoke = _unimplemented("ToolRegistry.Invoke")

    return {
        "pb2": ash_pb2,
        "pb2_grpc": ash_pb2_grpc,
        "health": HealthServicer(),
        "llm": LlmProviderServicer(),
        "skills": SkillRegistryServicer(),
        "commands": CommandRegistryServicer(),
        "harness": HarnessServicer(),
        "tools": ToolRegistryServicer(),
    }


def build_server(bind: str = DEFAULT_BIND) -> tuple[grpc.Server, str]:
    """Create a configured, unstarted gRPC server. Returns (server, actual_bind)."""
    servicers = _build_servicers()
    pb2_grpc = servicers["pb2_grpc"]

    server = grpc.server(ThreadPoolExecutor(max_workers=8))
    pb2_grpc.add_HealthServicer_to_server(servicers["health"], server)
    pb2_grpc.add_LlmProviderServicer_to_server(servicers["llm"], server)
    pb2_grpc.add_SkillRegistryServicer_to_server(servicers["skills"], server)
    pb2_grpc.add_CommandRegistryServicer_to_server(servicers["commands"], server)
    pb2_grpc.add_HarnessServicer_to_server(servicers["harness"], server)
    pb2_grpc.add_ToolRegistryServicer_to_server(servicers["tools"], server)

    port = server.add_insecure_port(bind)
    # If the caller passed port 0, the effective port is whatever the OS picked.
    if bind.endswith(":0"):
        bind = bind.rsplit(":", 1)[0] + f":{port}"
    return server, bind


def serve(bind: str = DEFAULT_BIND) -> int:
    """Blocking entry point used by ``ashpy serve`` and supervisord."""
    server, effective_bind = build_server(bind)
    server.start()
    _log(f"ashpy gRPC server listening on {effective_bind}")

    stop_event = asyncio.Event() if False else None  # kept for future async path
    shutting_down = {"flag": False}

    def _handle(signum, _frame):
        if shutting_down["flag"]:
            return
        shutting_down["flag"] = True
        _log(f"received signal {signum}, shutting down")
        server.stop(grace=2.0)

    signal.signal(signal.SIGTERM, _handle)
    signal.signal(signal.SIGINT, _handle)

    try:
        server.wait_for_termination()
    except KeyboardInterrupt:
        _handle(signal.SIGINT, None)
        server.wait_for_termination()

    _log("sidecar stopped")
    return 0


if __name__ == "__main__":
    sys.exit(serve())
