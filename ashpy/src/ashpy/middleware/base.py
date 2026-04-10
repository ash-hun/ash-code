"""Middleware ABC + decision primitives."""

from __future__ import annotations

from abc import ABC
from dataclasses import dataclass, field
from enum import IntEnum
from typing import ClassVar, Optional


class DecisionKind(IntEnum):
    ALLOW = 0
    DENY = 1
    REWRITE = 2


@dataclass
class HookDecision:
    kind: DecisionKind = DecisionKind.ALLOW
    reason: str = ""
    rewritten_payload: bytes = b""


def allow() -> HookDecision:
    return HookDecision(DecisionKind.ALLOW)


def deny(reason: str) -> HookDecision:
    return HookDecision(DecisionKind.DENY, reason=reason)


def rewrite(payload: bytes, reason: str = "") -> HookDecision:
    return HookDecision(DecisionKind.REWRITE, reason=reason, rewritten_payload=payload)


# --- context objects mirrored from the proto schema ----------------------


@dataclass
class TurnContext:
    session_id: str
    turn_id: str
    provider: str
    model: str
    messages: list[dict] = field(default_factory=list)
    metadata: dict[str, str] = field(default_factory=dict)


@dataclass
class ToolCallEvent:
    session_id: str
    turn_id: str
    tool_name: str
    arguments: bytes


@dataclass
class TurnResult:
    session_id: str
    turn_id: str
    stop_reason: str
    input_tokens: int
    output_tokens: int
    assistant_text: str


# --- Middleware contract --------------------------------------------------


class Middleware(ABC):
    """Override any subset of the four hooks. Defaults are no-op / ALLOW."""

    #: Lower priority runs first (matches the M0.5 spec).
    priority: ClassVar[int] = 100
    #: Stable identifier — defaults to class name.
    name: ClassVar[str] = ""

    async def on_turn_start(self, ctx: TurnContext) -> HookDecision:
        return allow()

    async def on_tool_call(self, event: ToolCallEvent) -> HookDecision:
        return allow()

    async def on_stream_delta(self, delta: dict) -> None:
        return None

    async def on_turn_end(self, result: TurnResult) -> None:
        return None


class MiddlewareChain:
    """Ordered collection of middleware; short-circuits on first non-ALLOW."""

    def __init__(self, middlewares: Optional[list[Middleware]] = None) -> None:
        self._chain: list[Middleware] = []
        for mw in middlewares or []:
            self.add(mw)

    def add(self, mw: Middleware) -> None:
        self._chain.append(mw)
        self._chain.sort(key=lambda m: (m.priority, m.name or type(m).__name__))

    def names(self) -> list[str]:
        return [m.name or type(m).__name__ for m in self._chain]

    def __len__(self) -> int:
        return len(self._chain)

    async def on_turn_start(self, ctx: TurnContext) -> HookDecision:
        for mw in self._chain:
            decision = await mw.on_turn_start(ctx)
            if decision.kind != DecisionKind.ALLOW:
                return decision
        return allow()

    async def on_tool_call(self, event: ToolCallEvent) -> HookDecision:
        for mw in self._chain:
            decision = await mw.on_tool_call(event)
            if decision.kind != DecisionKind.ALLOW:
                return decision
        return allow()

    async def on_stream_delta(self, delta: dict) -> None:
        for mw in self._chain:
            await mw.on_stream_delta(delta)

    async def on_turn_end(self, result: TurnResult) -> None:
        for mw in self._chain:
            await mw.on_turn_end(result)
