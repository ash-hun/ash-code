"""Observability middleware: dump every hook event to stderr as JSON."""

from __future__ import annotations

import json
import sys

from .base import (
    HookDecision,
    Middleware,
    ToolCallEvent,
    TurnContext,
    TurnResult,
    allow,
)


class LoggingMiddleware(Middleware):
    priority = 10
    name = "logging"

    def _emit(self, kind: str, payload: dict) -> None:
        try:
            print(
                json.dumps({"hook": kind, **payload}, ensure_ascii=False),
                file=sys.stderr,
                flush=True,
            )
        except Exception:
            pass

    async def on_turn_start(self, ctx: TurnContext) -> HookDecision:
        self._emit(
            "on_turn_start",
            {
                "session_id": ctx.session_id,
                "turn_id": ctx.turn_id,
                "provider": ctx.provider,
                "model": ctx.model,
                "message_count": len(ctx.messages),
            },
        )
        return allow()

    async def on_tool_call(self, event: ToolCallEvent) -> HookDecision:
        self._emit(
            "on_tool_call",
            {
                "session_id": event.session_id,
                "turn_id": event.turn_id,
                "tool": event.tool_name,
                "args_len": len(event.arguments or b""),
            },
        )
        return allow()

    async def on_stream_delta(self, delta: dict) -> None:
        return None

    async def on_turn_end(self, result: TurnResult) -> None:
        self._emit(
            "on_turn_end",
            {
                "session_id": result.session_id,
                "turn_id": result.turn_id,
                "stop_reason": result.stop_reason,
                "input_tokens": result.input_tokens,
                "output_tokens": result.output_tokens,
            },
        )
