"""Anthropic provider plugin.

Uses the official ``anthropic`` SDK. Missing credentials degrade
gracefully — ``health()`` returns ``UNCONFIGURED`` and ``chat_stream``
emits an error delta instead of raising.
"""

from __future__ import annotations

import os
from typing import AsyncIterator

import json
from typing import Optional

from .base import (
    ChatDelta,
    ChatRequest,
    HealthState,
    HealthStatus,
    LlmProvider,
    ProviderCaps,
    ProviderConfig,
    ToolCallDelta,
    unconfigured_stream,
)


def _try_parse_tool_use_sentinel(content: str) -> Optional[dict]:
    """Return the parsed payload if `content` is an ash-code tool_use sentinel."""
    if not content or "__ash_tool_use__" not in content:
        return None
    try:
        data = json.loads(content)
    except json.JSONDecodeError:
        return None
    if isinstance(data, dict) and data.get("__ash_tool_use__") is True:
        return data
    return None


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
                continue
            if m.role == "tool":
                # Translate ash-code's `tool` role into Anthropic's
                # tool_result content block carried inside a user turn.
                messages.append(
                    {
                        "role": "user",
                        "content": [
                            {
                                "type": "tool_result",
                                "tool_use_id": m.tool_call_id or "unknown",
                                "content": m.content,
                            }
                        ],
                    }
                )
                continue
            if m.role == "assistant":
                # Detect the ash-code tool_use sentinel that the Rust
                # query loop writes after a model-requested tool call.
                # Convert it back into Anthropic's tool_use content block
                # so the next-turn pairing with tool_result is valid.
                tool_use = _try_parse_tool_use_sentinel(m.content)
                if tool_use is not None:
                    messages.append(
                        {
                            "role": "assistant",
                            "content": [
                                {
                                    "type": "tool_use",
                                    "id": tool_use["id"],
                                    "name": tool_use["name"],
                                    "input": tool_use.get("input", {}),
                                }
                            ],
                        }
                    )
                    continue
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

        # Forward tool definitions so the model knows what it can call.
        if req.tools:
            kwargs["tools"] = [
                {
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                }
                for t in req.tools
            ]

        try:
            # Track partial tool_use blocks across deltas. Anthropic streams
            # tool input as input_json_delta partials that we accumulate.
            partial_tools: dict[int, dict] = {}
            async with client.messages.stream(**kwargs) as stream:
                async for event in stream:
                    etype = getattr(event, "type", "")
                    if etype == "content_block_start":
                        block = getattr(event, "content_block", None)
                        if block is not None and getattr(block, "type", "") == "tool_use":
                            partial_tools[event.index] = {
                                "id": getattr(block, "id", ""),
                                "name": getattr(block, "name", ""),
                                "input_json": "",
                            }
                    elif etype == "content_block_delta":
                        delta = getattr(event, "delta", None)
                        delta_type = getattr(delta, "type", "")
                        if delta_type == "text_delta":
                            text = getattr(delta, "text", "")
                            if text:
                                yield ChatDelta(text=text)
                        elif delta_type == "input_json_delta":
                            entry = partial_tools.get(event.index)
                            if entry is not None:
                                entry["input_json"] += getattr(delta, "partial_json", "")
                    elif etype == "content_block_stop":
                        entry = partial_tools.pop(event.index, None)
                        if entry is not None:
                            args_json = entry["input_json"] or "{}"
                            try:
                                # Validate it's parseable JSON; re-serialise
                                # to a canonical form for downstream consumers.
                                args_json = json.dumps(json.loads(args_json))
                            except json.JSONDecodeError:
                                pass
                            yield ChatDelta(
                                tool_call=ToolCallDelta(
                                    id=entry["id"],
                                    name=entry["name"],
                                    arguments=args_json,
                                )
                            )
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
