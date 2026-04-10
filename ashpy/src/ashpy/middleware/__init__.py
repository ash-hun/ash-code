"""Harness middleware chain (M3).

Implements the query-loop hooks contract from :doc:`/docs/extensibility.md`:
every turn the Rust ``crates/query`` engine calls
``Harness.OnTurnStart``/``OnToolCall``/``OnStreamDelta``/``OnTurnEnd`` over
gRPC, and those RPCs walk an ordered middleware chain. The first non-ALLOW
decision short-circuits.
"""

from .base import (
    DecisionKind,
    HookDecision,
    Middleware,
    MiddlewareChain,
    ToolCallEvent,
    TurnContext,
    TurnResult,
    allow,
    deny,
    rewrite,
)
from .loader import MiddlewareLoader, build_default_chain

__all__ = [
    "DecisionKind",
    "HookDecision",
    "Middleware",
    "MiddlewareChain",
    "ToolCallEvent",
    "TurnContext",
    "TurnResult",
    "allow",
    "deny",
    "rewrite",
    "MiddlewareLoader",
    "build_default_chain",
]
