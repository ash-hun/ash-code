"""OpenAI-compatible provider plugin.

Uses the official ``openai`` async SDK. Supports any OpenAI-compatible
endpoint via ``OPENAI_BASE_URL`` (e.g. Azure OpenAI, LiteLLM proxies).
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


class OpenAIProvider(LlmProvider):
    name = "openai"

    def __init__(self, config: ProviderConfig) -> None:
        super().__init__(config)
        self._client = None

    # --- helpers -----------------------------------------------------------

    def _missing_env(self) -> list[str]:
        key_env = self.config.auth.get("api_key_env", "OPENAI_API_KEY")
        if not os.environ.get(key_env):
            return [key_env]
        return []

    def _ensure_client(self):
        if self._client is not None:
            return self._client
        import openai  # lazy

        key_env = self.config.auth.get("api_key_env", "OPENAI_API_KEY")
        base_env = self.config.auth.get("base_url_env", "OPENAI_BASE_URL")
        kwargs: dict = {"api_key": os.environ.get(key_env, "")}
        base_url = os.environ.get(base_env, "")
        if base_url:
            kwargs["base_url"] = base_url
        self._client = openai.AsyncOpenAI(**kwargs)
        return self._client

    # --- LlmProvider API ---------------------------------------------------

    def capabilities(self) -> ProviderCaps:
        return ProviderCaps(
            supports_tools=True,
            supports_vision=True,
            max_context_tokens=128_000,
            default_model=self.config.model() or "gpt-4.1-mini",
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
        model = req.model or self.config.model() or "gpt-4.1-mini"
        messages = [{"role": m.role, "content": m.content} for m in req.messages]
        max_tokens = int(self.config.defaults.get("max_tokens", 4096))

        try:
            stream = await client.chat.completions.create(
                model=model,
                messages=messages,
                temperature=req.temperature,
                max_tokens=max_tokens,
                stream=True,
                stream_options={"include_usage": True},
            )
            input_tokens = 0
            output_tokens = 0
            finish_reason = "end_turn"
            async for chunk in stream:
                # Usage chunks (from stream_options.include_usage) have an
                # empty choices array; process them before the guard.
                usage = getattr(chunk, "usage", None)
                if usage:
                    input_tokens = getattr(usage, "prompt_tokens", 0) or input_tokens
                    output_tokens = getattr(usage, "completion_tokens", 0) or output_tokens
                if not chunk.choices:
                    continue
                choice = chunk.choices[0]
                delta = choice.delta
                if delta and delta.content:
                    yield ChatDelta(text=delta.content)
                if choice.finish_reason:
                    finish_reason = choice.finish_reason
            yield ChatDelta(
                finish_reason=str(finish_reason),
                input_tokens=input_tokens,
                output_tokens=output_tokens,
            )
        except Exception as exc:  # noqa: BLE001
            yield ChatDelta(error=f"openai error: {exc}")
            yield ChatDelta(finish_reason="error")
