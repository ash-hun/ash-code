"""ashpy gRPC server (``grpc.aio``-based).

M3 adds:
  * Real ``Harness`` servicer backed by the middleware chain.
  * ``LlmProviderServicer.ChatStream`` detects the fake provider's
    ``_fake_tool_call`` sentinel and emits a protobuf ``ToolCall`` delta.
  * ``features.harness`` promoted to ``"v1"``.

M2 assets kept: real LlmProvider, four built-ins via plugin contract.
Skills / Commands / ToolRegistry remain ``UNIMPLEMENTED`` placeholders
(M5 / M6 / M3+).
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
from .middleware import (
    DecisionKind,
    ToolCallEvent as PyToolCallEvent,
    TurnContext as PyTurnContext,
    TurnResult as PyTurnResult,
    build_default_chain,
)
from .providers import ChatMessage as PyChatMessage
from .providers import ChatRequest as PyChatRequest
from .providers import get_registry

DEFAULT_BIND = "127.0.0.1:50051"
API_VERSION = "v1"

_LOG = logging.getLogger("ashpy.server")


# Env vars that must not be passed to provider SDKs as empty strings.
# docker-compose's ${VAR:-} pattern injects "" when the host is unset,
# and some SDKs (anthropic, openai) read these directly from os.environ
# rather than honoring an explicit `None`, producing
# ``httpx.UnsupportedProtocol`` when the value is "". Scrub once at startup.
_SCRUB_IF_EMPTY = (
    "ANTHROPIC_BASE_URL",
    "OPENAI_BASE_URL",
    "VLLM_BASE_URL",
    "VLLM_API_KEY",
    "OLLAMA_BASE_URL",
    "ASH_LLM_MODEL",
)


def _scrub_empty_env() -> None:
    import os as _os

    for key in _SCRUB_IF_EMPTY:
        if key in _os.environ and _os.environ[key] == "":
            del _os.environ[key]


_scrub_empty_env()


def _log(msg: str) -> None:
    print(f"[ashpy] {msg}", flush=True)


# --- global state ---------------------------------------------------------


_MIDDLEWARE_CHAIN = None


def get_middleware_chain():
    global _MIDDLEWARE_CHAIN
    if _MIDDLEWARE_CHAIN is None:
        _MIDDLEWARE_CHAIN = build_default_chain()
    return _MIDDLEWARE_CHAIN


def reset_middleware_chain_for_tests(chain):
    global _MIDDLEWARE_CHAIN
    _MIDDLEWARE_CHAIN = chain


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
                    "llm": "v1",
                    "skills": "planned",
                    "commands": "planned",
                    "harness": "v1",   # M3 — live
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
                infos.append(
                    ash_pb2.ProviderInfo(
                        name=name,
                        default_model=(caps.default_model if caps else cfg.model()),
                        supports_tools=bool(caps.supports_tools) if caps else False,
                        supports_vision=bool(caps.supports_vision) if caps else False,
                        source=cfg.source,
                    )
                )
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

        async def ChatStream(self, request, context):  # noqa: N802
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
            if False:  # pragma: no cover
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

    # --- Harness is LIVE as of M3 -----------------------------------------

    def _decision_to_pb(dec) -> "ash_pb2.HookDecision":
        return ash_pb2.HookDecision(
            kind=int(dec.kind),
            reason=dec.reason,
            rewritten_payload=dec.rewritten_payload,
        )

    class HarnessServicer(ash_pb2_grpc.HarnessServicer):
        async def OnTurnStart(self, request, context):  # noqa: N802
            chain = get_middleware_chain()
            ctx = PyTurnContext(
                session_id=request.session_id,
                turn_id=request.turn_id,
                provider=request.provider,
                model=request.model,
                messages=[
                    {"role": m.role, "content": m.content, "tool_call_id": m.tool_call_id}
                    for m in request.messages
                ],
                metadata=dict(request.metadata),
            )
            decision = await chain.on_turn_start(ctx)
            return _decision_to_pb(decision)

        async def OnToolCall(self, request, context):  # noqa: N802
            chain = get_middleware_chain()
            call = request.call or ash_pb2.ToolCall()
            event = PyToolCallEvent(
                session_id=request.session_id,
                turn_id=request.turn_id,
                tool_name=call.name,
                arguments=bytes(call.arguments or b""),
            )
            decision = await chain.on_tool_call(event)
            return _decision_to_pb(decision)

        async def OnStreamDelta(self, request, context):  # noqa: N802
            chain = get_middleware_chain()
            delta = request.delta
            payload = {"kind": delta.WhichOneof("kind") or ""}
            await chain.on_stream_delta(payload)
            return ash_pb2.Empty()

        async def OnTurnEnd(self, request, context):  # noqa: N802
            chain = get_middleware_chain()
            finish = request.finish or ash_pb2.TurnFinish()
            result = PyTurnResult(
                session_id=request.session_id,
                turn_id=request.turn_id,
                stop_reason=finish.stop_reason,
                input_tokens=finish.input_tokens,
                output_tokens=finish.output_tokens,
                assistant_text=request.assistant_text,
            )
            await chain.on_turn_end(result)
            return ash_pb2.Empty()

    class ToolRegistryServicer(ash_pb2_grpc.ToolRegistryServicer):
        List = _unimplemented_unary("ToolRegistry.List (M3+, host-owned)")
        Invoke = _unimplemented_unary("ToolRegistry.Invoke (M3+, host-owned)")

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
    _log(f"middleware chain: {get_middleware_chain().names()}")

    stop_event = asyncio.Event()
    loop = asyncio.get_running_loop()

    def _handle_signal():
        if not stop_event.is_set():
            _log("shutdown signal received")
            stop_event.set()

    for sig in (signal.SIGTERM, signal.SIGINT):
        try:
            loop.add_signal_handler(sig, _handle_signal)
        except NotImplementedError:  # pragma: no cover
            signal.signal(sig, lambda *_: _handle_signal())

    await stop_event.wait()
    await server.stop(grace=2.0)
    _log("sidecar stopped")
    return 0


def serve(bind: str = DEFAULT_BIND) -> int:
    return asyncio.run(_serve_async(bind))


if __name__ == "__main__":
    sys.exit(serve())
