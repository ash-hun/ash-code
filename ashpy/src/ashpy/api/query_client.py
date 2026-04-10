"""gRPC client that the FastAPI layer uses to talk to the Rust QueryHost."""

from __future__ import annotations

import json
import os
from typing import AsyncIterator

import grpc
from grpc import aio as grpc_aio

from .. import _codegen

DEFAULT_QUERY_HOST_ENDPOINT = os.environ.get(
    "ASH_QUERY_HOST_ENDPOINT", "127.0.0.1:50052"
)


class QueryHostClient:
    """Thin wrapper around the generated QueryHost gRPC stub."""

    def __init__(self, endpoint: str = DEFAULT_QUERY_HOST_ENDPOINT) -> None:
        self._endpoint = endpoint
        self._channel: grpc_aio.Channel | None = None
        self._stub = None

    async def _ensure(self):
        if self._channel is None:
            _codegen.ensure_generated()
            from ashpy._generated import ash_pb2_grpc  # type: ignore

            self._channel = grpc_aio.insecure_channel(self._endpoint)
            self._stub = ash_pb2_grpc.QueryHostStub(self._channel)
        return self._stub

    async def run_turn(
        self,
        session_id: str,
        prompt: str,
        provider: str = "",
        model: str = "",
        reset_session: bool = False,
    ) -> AsyncIterator[dict]:
        """Open a ``RunTurn`` stream and yield events as plain dicts."""
        stub = await self._ensure()
        from ashpy._generated import ash_pb2  # type: ignore

        req = ash_pb2.RunTurnRequest(
            session_id=session_id,
            prompt=prompt,
            provider=provider,
            model=model,
            reset_session=reset_session,
        )
        try:
            async for delta in stub.RunTurn(req):
                kind = delta.WhichOneof("kind")
                if kind == "text":
                    yield {"type": "text", "text": delta.text}
                elif kind == "tool_call":
                    yield {
                        "type": "tool_call",
                        "name": delta.tool_call.name,
                        "arguments": delta.tool_call.arguments.decode("utf-8", errors="replace"),
                    }
                elif kind == "tool_result":
                    tr = delta.tool_result
                    yield {
                        "type": "tool_result",
                        "name": tr.name,
                        "ok": tr.ok,
                        "stdout": tr.stdout,
                        "stderr": tr.stderr,
                        "exit_code": tr.exit_code,
                    }
                elif kind == "finish":
                    f = delta.finish
                    yield {
                        "type": "finish",
                        "stop_reason": f.stop_reason,
                        "input_tokens": f.input_tokens,
                        "output_tokens": f.output_tokens,
                    }
                elif kind == "error":
                    yield {"type": "error", "message": delta.error}
                elif kind == "outcome":
                    o = delta.outcome
                    yield {
                        "type": "outcome",
                        "stop_reason": o.stop_reason,
                        "turns_taken": o.turns_taken,
                        "denied": o.denied,
                        "denial_reason": o.denial_reason,
                    }
        except grpc.RpcError as exc:
            yield {"type": "error", "message": f"grpc error: {exc.code().name}: {exc.details()}"}

    async def watch_session(self, session_id: str) -> AsyncIterator[dict]:
        """Open a ``WatchSession`` stream and yield events as plain dicts."""
        stub = await self._ensure()
        from ashpy._generated import ash_pb2  # type: ignore

        req = ash_pb2.WatchSessionRequest(session_id=session_id)
        try:
            async for event in stub.WatchSession(req):
                yield {
                    "event_type": event.event_type,
                    "session_id": event.session_id,
                    "payload": json.loads(event.payload.decode("utf-8"))
                    if event.payload
                    else {},
                }
        except grpc.RpcError as exc:
            yield {
                "event_type": "error",
                "session_id": session_id,
                "payload": {"message": f"grpc error: {exc.code().name}: {exc.details()}"},
            }

    async def list_sessions(self) -> list[dict]:
        stub = await self._ensure()
        from ashpy._generated import ash_pb2  # type: ignore

        resp = await stub.ListSessions(ash_pb2.ListSessionsRequest())
        return [
            {
                "id": s.id,
                "provider": s.provider,
                "model": s.model,
                "message_count": s.message_count,
            }
            for s in resp.sessions
        ]

    async def get_session(self, session_id: str) -> dict | None:
        stub = await self._ensure()
        from ashpy._generated import ash_pb2  # type: ignore

        try:
            resp = await stub.GetSession(ash_pb2.GetSessionRequest(id=session_id))
        except grpc.RpcError as exc:
            if exc.code() == grpc.StatusCode.NOT_FOUND:
                return None
            raise
        summary = resp.summary
        return {
            "summary": {
                "id": summary.id,
                "provider": summary.provider,
                "model": summary.model,
                "message_count": summary.message_count,
            },
            "messages": [
                {"role": m.role, "content": m.content, "tool_call_id": m.tool_call_id}
                for m in resp.messages
            ],
        }

    async def cancel_turn(self, session_id: str) -> dict:
        """Cancel an in-flight turn for the given session."""
        stub = await self._ensure()
        from ashpy._generated import ash_pb2  # type: ignore

        resp = await stub.CancelTurn(ash_pb2.CancelTurnRequest(session_id=session_id))
        return {"ok": resp.ok, "message": resp.message}

    async def delete_session(self, session_id: str) -> bool:
        stub = await self._ensure()
        from ashpy._generated import ash_pb2  # type: ignore

        resp = await stub.DeleteSession(ash_pb2.DeleteSessionRequest(id=session_id))
        return resp.ok

    async def close(self) -> None:
        if self._channel is not None:
            await self._channel.close()
            self._channel = None
            self._stub = None
