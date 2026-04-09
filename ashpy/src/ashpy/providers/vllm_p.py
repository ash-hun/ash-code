"""vLLM provider plugin.

vLLM exposes an OpenAI-compatible HTTP endpoint, so this provider is a
thin specialization of :class:`OpenAIProvider` that (a) reads
``VLLM_API_KEY`` / ``VLLM_BASE_URL`` instead of the OpenAI envs, and
(b) requires ``VLLM_BASE_URL`` to be set (the endpoint has no well-known
default).
"""

from __future__ import annotations

import os
from typing import AsyncIterator

from .base import (
    ChatDelta,
    ChatRequest,
    HealthState,
    HealthStatus,
    ProviderCaps,
    ProviderConfig,
    unconfigured_stream,
)
from .openai_p import OpenAIProvider


class VllmProvider(OpenAIProvider):
    name = "vllm"

    def __init__(self, config: ProviderConfig) -> None:
        # Reuse OpenAIProvider construction but force the auth envs to the
        # vLLM-specific variables before base.__init__ stores config.
        if "base_url_env" not in config.auth:
            config.auth["base_url_env"] = "VLLM_BASE_URL"
        if "api_key_env" not in config.auth:
            config.auth["api_key_env"] = "VLLM_API_KEY"
        super().__init__(config)

    def _missing_env(self) -> list[str]:
        # vLLM requires the endpoint; the API key is optional (many
        # deployments run unauthenticated inside a private network).
        base_env = self.config.auth.get("base_url_env", "VLLM_BASE_URL")
        if not os.environ.get(base_env):
            return [base_env]
        return []

    def _ensure_client(self):
        if self._client is not None:
            return self._client
        import openai

        base_env = self.config.auth.get("base_url_env", "VLLM_BASE_URL")
        key_env = self.config.auth.get("api_key_env", "VLLM_API_KEY")
        base_url = os.environ.get(base_env, "")
        # vLLM clients accept any non-empty api_key; use a placeholder when
        # the deployment is unauthenticated.
        api_key = os.environ.get(key_env, "") or "sk-vllm-unauthenticated"
        self._client = openai.AsyncOpenAI(api_key=api_key, base_url=base_url)
        return self._client

    def capabilities(self) -> ProviderCaps:
        return ProviderCaps(
            supports_tools=False,  # tool-use support is model-dependent; M3 will refine
            supports_vision=False,
            max_context_tokens=int(self.config.defaults.get("max_context_tokens", 32_000)),
            default_model=self.config.model() or "",
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
        # Inherit OpenAIProvider's streaming logic verbatim.
        async for delta in super().chat_stream(req):
            yield delta
