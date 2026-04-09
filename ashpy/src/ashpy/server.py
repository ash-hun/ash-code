"""ashpy gRPC server (``grpc.aio``-based).

M2 scope:
  * ``Health.Ping`` — live (M1).
  * ``LlmProvider`` — live: ListProviders / Capabilities / ChatStream / Switch
    backed by :mod:`ashpy.providers`.
  * ``SkillRegistry`` / ``CommandRegistry`` / ``Harness`` / ``ToolRegistry`` —
    still ``UNIMPLEMENTED`` placeholders with owning-milestone messages.

See :doc:`/docs/comparison_grpcio_grpcaio.md` for why we picked the async
model.
"""

from __future__ import annotations

import asyncio
import logging
import signal
import sys
import time
from typing import AsyncIterator

import grpc
from grpc import aio as grpc_aio

from . import __version__, _codegen
from .providers import ChatMessage as PyChatMessage
from .providers import ChatRequest as PyChatRequest
from .providers import get_registry

DEFAULT_BIND = "127.0.0.1:50051"
API_VERSION = "v1"

_LOG = logging.getLogger("ashpy.server")


def _log(msg: str) -> None:
    print(f"[ashpy] {msg}", flush=True)


# --- Servicers -------------------------------------------------------------


def _build_servicers():
    _codegen.ensure_generated()
    from ashpy._generated import ash_pb2, ash_pb2_grpc  # type: ignore

    class HealthServicer(ash_pb2_grpc.HealthServicer):
        async def Ping(self, request, context):  # noqa: N802
            return ash_pb2.PingResponse(
                server=f"ashpy/{__version__}",
                api_version=API_VERSION,
                received_unix_ms=int(time.time() * 1000),
                features={
                    "health": "v1",
                    "llm": "v1",        # M2 — live
                    "skills": "planned",
                    "commands": "planned",
                    "harness": "planned",
                    "tools": "planned",
                },
            )

    class LlmProviderServicer(ash_pb2_grpc.LlmProviderServicer):
        async def ListProviders(self, request, context):  # noqa: N802
            registry = get_registry()
            infos = []
            for name in registry.list_names():
                cfg = registry.configs()[name]
                try:
                    caps = registry.capabilities(name)
                except Exception as exc:  # noqa: BLE001
                    _LOG.warning("capabilities(%s) failed: %s", name, exc)
                    caps = None
                info = ash_pb2.ProviderInfo(
                    name=name,
                    default_model=(caps.default_model if caps else cfg.model()),
                    supports_tools=bool(caps.supports_tools) if caps else False,
                    supports_vision=bool(caps.supports_vision) if caps else False,
                    source=cfg.source,
                )
                infos.append(info)
            return ash_pb2.ListProvidersResponse(providers=infos)

        async def Capabilities(self, request, context):  # noqa: N802
            registry = get_registry()
            name = request.provider or registry.active_name()
            try:
                caps = registry.capabilities(name)
            except KeyError:
                await context.abort(grpc.StatusCode.NOT_FOUND, f"unknown provider: {name}")
            return ash_pb2.CapabilitiesResponse(
                supports_tools=caps.supports_tools,
                supports_vision=caps.supports_vision,
                max_context_tokens=caps.max_context_tokens,
                model=caps.default_model,
            )

        async def ChatStream(  # noqa: N802
            self, request, context
        ) -> AsyncIterator:
            registry = get_registry()
            name = request.provider or registry.active_name()
            try:
                provider = registry.get(name)
            except KeyError:
                await context.abort(grpc.StatusCode.NOT_FOUND, f"unknown provider: {name}")
                return

            py_req = PyChatRequest(
                provider=name,
                model=request.model,
                messages=[
                    PyChatMessage(role=m.role, content=m.content, tool_call_id=m.tool_call_id)
                    for m in request.messages
                ],
                temperature=request.temperature or 0.0,
            )

            async for delta in provider.chat_stream(py_req):
                if delta.error:
                    # Represent provider errors as finish(stop_reason=error)
                    # carried in a TurnFinish delta with the text describing
                    # the error. This keeps the stream schema uniform.
                    yield ash_pb2.ChatDelta(text=f"[error] {delta.error}")
                    continue
                if delta.is_finish:
                    yield ash_pb2.ChatDelta(
                        finish=ash_pb2.TurnFinish(
                            stop_reason=delta.finish_reason,
                            input_tokens=delta.input_tokens,
                            output_tokens=delta.output_tokens,
                        )
                    )
                    continue
                if delta.text:
                    yield ash_pb2.ChatDelta(text=delta.text)

        async def Switch(self, request, context):  # noqa: N802
            registry = get_registry()
            try:
                registry.switch(request.provider, request.model)
            except KeyError as exc:
                return ash_pb2.SwitchResponse(ok=False, message=str(exc))
            return ash_pb2.SwitchResponse(
                ok=True,
                message=f"switched to {request.provider}"
                + (f" ({request.model})" if request.model else ""),
            )

    def _unimplemented_unary(service: str):
        async def _stub(self, request, context):  # noqa: ARG001
            await context.abort(
                grpc.StatusCode.UNIMPLEMENTED,
                f"{service} lands in a later milestone",
            )

        return _stub

    def _unimplemented_stream(service: str):
        async def _stub(self, request, context):  # noqa: ARG001
            await context.abort(
                grpc.StatusCode.UNIMPLEMENTED,
                f"{service} lands in a later milestone",
            )
            if False:  # pragma: no cover — generator marker
                yield

        return _stub

    class SkillRegistryServicer(ash_pb2_grpc.SkillRegistryServicer):
        List = _unimplemented_unary("SkillRegistry.List (M5)")
        Invoke = _unimplemented_unary("SkillRegistry.Invoke (M5)")
        Reload = _unimplemented_unary("SkillRegistry.Reload (M5)")
        Watch = _unimplemented_stream("SkillRegistry.Watch (M5)")

    class CommandRegistryServicer(ash_pb2_grpc.CommandRegistryServicer):
        List = _unimplemented_unary("CommandRegistry.List (M6)")
        Run = _unimplemented_unary("CommandRegistry.Run (M6)")
        Reload = _unimplemented_unary("CommandRegistry.Reload (M6)")

    class HarnessServicer(ash_pb2_grpc.HarnessServicer):
        OnTurnStart = _unimplemented_unary("Harness.OnTurnStart (M3)")
        OnToolCall = _unimplemented_unary("Harness.OnToolCall (M3)")
        OnStreamDelta = _unimplemented_unary("Harness.OnStreamDelta (M3)")
        OnTurnEnd = _unimplemented_unary("Harness.OnTurnEnd (M3)")

    class ToolRegistryServicer(ash_pb2_grpc.ToolRegistryServicer):
        List = _unimplemented_unary("ToolRegistry.List (M3+)")
        Invoke = _unimplemented_unary("ToolRegistry.Invoke (M3+)")

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


async def build_server(bind: str = DEFAULT_BIND) -> tuple[grpc_aio.Server, str]:
    servicers = _build_servicers()
    pb2_grpc = servicers["pb2_grpc"]

    server = grpc_aio.server()
    pb2_grpc.add_HealthServicer_to_server(servicers["health"], server)
    pb2_grpc.add_LlmProviderServicer_to_server(servicers["llm"], server)
    pb2_grpc.add_SkillRegistryServicer_to_server(servicers["skills"], server)
    pb2_grpc.add_CommandRegistryServicer_to_server(servicers["commands"], server)
    pb2_grpc.add_HarnessServicer_to_server(servicers["harness"], server)
    pb2_grpc.add_ToolRegistryServicer_to_server(servicers["tools"], server)

    port = server.add_insecure_port(bind)
    if bind.endswith(":0"):
        bind = bind.rsplit(":", 1)[0] + f":{port}"
    return server, bind


async def _serve_async(bind: str) -> int:
    server, effective_bind = await build_server(bind)
    await server.start()
    _log(f"ashpy gRPC server listening on {effective_bind}")

    stop_event = asyncio.Event()
    loop = asyncio.get_running_loop()

    def _handle_signal():
        if not stop_event.is_set():
            _log("shutdown signal received")
            stop_event.set()

    for sig in (signal.SIGTERM, signal.SIGINT):
        try:
            loop.add_signal_handler(sig, _handle_signal)
        except NotImplementedError:  # pragma: no cover — Windows
            signal.signal(sig, lambda *_: _handle_signal())

    await stop_event.wait()
    await server.stop(grace=2.0)
    _log("sidecar stopped")
    return 0


def serve(bind: str = DEFAULT_BIND) -> int:
    return asyncio.run(_serve_async(bind))


if __name__ == "__main__":
    sys.exit(serve())
