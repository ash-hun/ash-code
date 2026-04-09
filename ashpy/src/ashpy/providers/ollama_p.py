"""Ollama provider plugin.

Uses the official ``ollama`` async client. Requires ``OLLAMA_BASE_URL`` to
be set; otherwise degrades gracefully.
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


class OllamaProvider(LlmProvider):
    name = "ollama"

    def __init__(self, config: ProviderConfig) -> None:
        super().__init__(config)
        self._client = None

    def _missing_env(self) -> list[str]:
        base_env = self.config.auth.get("base_url_env", "OLLAMA_BASE_URL")
        if not os.environ.get(base_env):
            return [base_env]
        return []

    def _ensure_client(self):
        if self._client is not None:
            return self._client
        import ollama

        base_env = self.config.auth.get("base_url_env", "OLLAMA_BASE_URL")
        host = os.environ.get(base_env, "")
        self._client = ollama.AsyncClient(host=host) if host else ollama.AsyncClient()
        return self._client

    def capabilities(self) -> ProviderCaps:
        return ProviderCaps(
            supports_tools=False,  # model-dependent; refined in M3
            supports_vision=False,
            max_context_tokens=int(self.config.defaults.get("max_context_tokens", 32_000)),
            default_model=self.config.model() or "llama3.1",
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
        model = req.model or self.config.model() or "llama3.1"
        messages = [{"role": m.role, "content": m.content} for m in req.messages]

        try:
            stream = await client.chat(
                model=model,
                messages=messages,
                stream=True,
                options={"temperature": req.temperature},
            )
            finish_reason = "end_turn"
            async for chunk in stream:
                msg = chunk.get("message") if isinstance(chunk, dict) else getattr(chunk, "message", None)
                if msg:
                    content = msg.get("content", "") if isinstance(msg, dict) else getattr(msg, "content", "")
                    if content:
                        yield ChatDelta(text=content)
                done = chunk.get("done") if isinstance(chunk, dict) else getattr(chunk, "done", False)
                if done:
                    done_reason = (
                        chunk.get("done_reason") if isinstance(chunk, dict)
                        else getattr(chunk, "done_reason", "")
                    ) or "end_turn"
                    finish_reason = str(done_reason)
                    break
            yield ChatDelta(finish_reason=finish_reason)
        except Exception as exc:  # noqa: BLE001
            yield ChatDelta(error=f"ollama error: {exc}")
            yield ChatDelta(finish_reason="error")
