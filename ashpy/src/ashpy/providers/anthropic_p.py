"""Anthropic provider plugin.

Uses the official ``anthropic`` SDK. Missing credentials degrade
gracefully — ``health()`` returns ``UNCONFIGURED`` and ``chat_stream``
emits an error delta instead of raising.
"""

from __future__ import annotations

import os
from typing import AsyncIterator

from .base import (
    ChatDelta,
    ChatRequest,
    HealthState,
    HealthStatus,
    LlmProvider,
    ProviderCaps,
    ProviderConfig,
    unconfigured_stream,
)


class AnthropicProvider(LlmProvider):
    name = "anthropic"

    def __init__(self, config: ProviderConfig) -> None:
        super().__init__(config)
        self._client = None  # lazily built

    # --- credential plumbing -----------------------------------------------

    def _missing_env(self) -> list[str]:
        key_env = self.config.auth.get("api_key_env", "ANTHROPIC_API_KEY")
        if not os.environ.get(key_env):
            return [key_env]
        return []

    def _ensure_client(self):
        if self._client is not None:
            return self._client
        import anthropic  # lazy import

        key_env = self.config.auth.get("api_key_env", "ANTHROPIC_API_KEY")
        base_env = self.config.auth.get("base_url_env", "ANTHROPIC_BASE_URL")
        kwargs: dict = {"api_key": os.environ.get(key_env, "")}
        base_url = os.environ.get(base_env, "")
        if base_url:
            kwargs["base_url"] = base_url
        self._client = anthropic.AsyncAnthropic(**kwargs)
        return self._client

    # --- LlmProvider API ---------------------------------------------------

    def capabilities(self) -> ProviderCaps:
        return ProviderCaps(
            supports_tools=True,
            supports_vision=True,
            max_context_tokens=200_000,
            default_model=self.config.model() or "claude-opus-4-6",
        )

    async def health(self) -> HealthStatus:
        missing = self._missing_env()
        if missing:
            return HealthStatus(
                HealthState.UNCONFIGURED,
                f"missing env: {', '.join(missing)}",
            )
        return HealthStatus(HealthState.OK)

    async def chat_stream(self, req: ChatRequest) -> AsyncIterator[ChatDelta]:
        missing = self._missing_env()
        if missing:
            async for delta in unconfigured_stream(self.name, missing):
                yield delta
            return

        client = self._ensure_client()

        system_parts: list[str] = []
        messages: list[dict] = []
        for m in req.messages:
            if m.role == "system":
                system_parts.append(m.content)
            else:
                messages.append({"role": m.role, "content": m.content})

        model = req.model or self.config.model() or "claude-opus-4-6"
        max_tokens = int(self.config.defaults.get("max_tokens", 4096))

        kwargs: dict = {
            "model": model,
            "max_tokens": max_tokens,
            "messages": messages,
            "temperature": req.temperature,
        }
        if system_parts:
            kwargs["system"] = "\n\n".join(system_parts)

        try:
            async with client.messages.stream(**kwargs) as stream:
                async for chunk in stream.text_stream:
                    if chunk:
                        yield ChatDelta(text=chunk)
                final = await stream.get_final_message()
            stop_reason = getattr(final, "stop_reason", "end_turn") or "end_turn"
            usage = getattr(final, "usage", None)
            yield ChatDelta(
                finish_reason=str(stop_reason),
                input_tokens=getattr(usage, "input_tokens", 0) or 0,
                output_tokens=getattr(usage, "output_tokens", 0) or 0,
            )
        except Exception as exc:  # noqa: BLE001
            yield ChatDelta(error=f"anthropic error: {exc}")
            yield ChatDelta(finish_reason="error")
