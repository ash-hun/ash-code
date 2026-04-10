"""Sample middleware: per-session token budget enforcement.

Tracks cumulative input + output tokens across turns and denies new
turns when the budget is exceeded. Useful for cost control in shared
environments.

Usage:
  1. Copy this file to ashpy/src/ashpy/middleware/
  2. Register it in ashpy/src/ashpy/middleware/loader.py
  3. Set ASH_TOKEN_BUDGET env var (default: 100_000)

Example:
  ASH_TOKEN_BUDGET=50000 docker compose up -d ash-code
"""

from __future__ import annotations

import os
from collections import defaultdict

from .base import (
    HookDecision,
    Middleware,
    TurnContext,
    TurnResult,
    allow,
    deny,
)

DEFAULT_BUDGET = 100_000


class TokenBudgetMiddleware(Middleware):
    """Deny turns when a session exceeds its token budget."""

    priority = 50
    name = "token_budget"

    def __init__(self) -> None:
        self._budget = int(os.environ.get("ASH_TOKEN_BUDGET", DEFAULT_BUDGET))
        # session_id -> cumulative tokens used
        self._usage: dict[str, int] = defaultdict(int)

    async def on_turn_start(self, ctx: TurnContext) -> HookDecision:
        used = self._usage[ctx.session_id]
        if used >= self._budget:
            return deny(
                f"Token budget exceeded: {used}/{self._budget} tokens used. "
                f"Start a new session or increase ASH_TOKEN_BUDGET."
            )
        return allow()

    async def on_turn_end(self, result: TurnResult) -> None:
        self._usage[result.session_id] += result.input_tokens + result.output_tokens
