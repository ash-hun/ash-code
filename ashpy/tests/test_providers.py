"""Unit tests for provider plugins + registry (M2)."""

from __future__ import annotations

import os
import pathlib
from typing import AsyncIterator
from unittest import mock

import pytest

from ashpy.providers import (
    ChatDelta,
    ChatMessage,
    ChatRequest,
    HealthState,
    LlmProvider,
    ProviderConfig,
    ProviderRegistry,
)
from ashpy.providers.anthropic_p import AnthropicProvider
from ashpy.providers.ollama_p import OllamaProvider
from ashpy.providers.openai_p import OpenAIProvider
from ashpy.providers.vllm_p import VllmProvider


pytestmark = pytest.mark.asyncio


# --- registry --------------------------------------------------------------


@pytest.fixture
def clean_env(monkeypatch):
    for key in (
        "ASH_LLM_PROVIDER",
        "ASH_LLM_MODEL",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_BASE_URL",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "VLLM_BASE_URL",
        "VLLM_API_KEY",
        "OLLAMA_BASE_URL",
    ):
        monkeypatch.delenv(key, raising=False)
    yield monkeypatch


def _empty_providers_dir(tmp_path: pathlib.Path) -> pathlib.Path:
    d = tmp_path / "providers"
    d.mkdir()
    return d


async def test_registry_lists_all_builtins_without_env(clean_env, tmp_path):
    reg = ProviderRegistry(providers_dir=_empty_providers_dir(tmp_path))
    assert reg.list_names() == ["anthropic", "ollama", "openai", "vllm"]
    # Default active provider falls back to anthropic even with no env set.
    assert reg.active_name() == "anthropic"


async def test_registry_honors_ash_llm_provider(clean_env, tmp_path):
    clean_env.setenv("ASH_LLM_PROVIDER", "ollama")
    reg = ProviderRegistry(providers_dir=_empty_providers_dir(tmp_path))
    assert reg.active_name() == "ollama"


async def test_registry_honors_ash_llm_model(clean_env, tmp_path):
    clean_env.setenv("ASH_LLM_PROVIDER", "anthropic")
    clean_env.setenv("ASH_LLM_MODEL", "claude-sonnet-4-6")
    reg = ProviderRegistry(providers_dir=_empty_providers_dir(tmp_path))
    caps = reg.capabilities("anthropic")
    assert caps.default_model == "claude-sonnet-4-6"


async def test_toml_override_wins_over_builtin_defaults(clean_env, tmp_path):
    providers_dir = _empty_providers_dir(tmp_path)
    (providers_dir / "anthropic.toml").write_text(
        """
[provider]
name = "anthropic"

[defaults]
model = "claude-haiku-4-5"
temperature = 0.0
""".strip()
    )
    reg = ProviderRegistry(providers_dir=providers_dir)
    caps = reg.capabilities("anthropic")
    assert caps.default_model == "claude-haiku-4-5"
    assert reg.configs()["anthropic"].source.startswith("plugin:")


async def test_switch_updates_active_and_model(clean_env, tmp_path):
    reg = ProviderRegistry(providers_dir=_empty_providers_dir(tmp_path))
    reg.switch("openai", "gpt-4.1")
    assert reg.active_name() == "openai"
    assert reg.capabilities("openai").default_model == "gpt-4.1"


# --- graceful degradation --------------------------------------------------


@pytest.mark.parametrize("provider_cls", [AnthropicProvider, OpenAIProvider, VllmProvider, OllamaProvider])
async def test_provider_health_unconfigured_when_env_missing(clean_env, provider_cls):
    cfg = ProviderConfig(name=provider_cls.name)
    if provider_cls is AnthropicProvider:
        cfg.auth = {"api_key_env": "ANTHROPIC_API_KEY", "base_url_env": "ANTHROPIC_BASE_URL"}
    elif provider_cls is OpenAIProvider:
        cfg.auth = {"api_key_env": "OPENAI_API_KEY", "base_url_env": "OPENAI_BASE_URL"}
    elif provider_cls is VllmProvider:
        cfg.auth = {"api_key_env": "VLLM_API_KEY", "base_url_env": "VLLM_BASE_URL"}
    elif provider_cls is OllamaProvider:
        cfg.auth = {"base_url_env": "OLLAMA_BASE_URL"}
    provider = provider_cls(cfg)
    status = await provider.health()
    assert status.state == HealthState.UNCONFIGURED


@pytest.mark.parametrize("provider_cls", [AnthropicProvider, OpenAIProvider, VllmProvider, OllamaProvider])
async def test_provider_chat_stream_yields_error_delta_when_unconfigured(clean_env, provider_cls):
    cfg = ProviderConfig(name=provider_cls.name)
    if provider_cls is AnthropicProvider:
        cfg.auth = {"api_key_env": "ANTHROPIC_API_KEY"}
    elif provider_cls is OpenAIProvider:
        cfg.auth = {"api_key_env": "OPENAI_API_KEY"}
    elif provider_cls is VllmProvider:
        cfg.auth = {"base_url_env": "VLLM_BASE_URL"}
    elif provider_cls is OllamaProvider:
        cfg.auth = {"base_url_env": "OLLAMA_BASE_URL"}
    provider = provider_cls(cfg)
    req = ChatRequest(
        provider=provider_cls.name,
        model="test",
        messages=[ChatMessage(role="user", content="hi")],
    )
    deltas: list[ChatDelta] = []
    async for d in provider.chat_stream(req):
        deltas.append(d)
    assert any(d.error for d in deltas)
    assert deltas[-1].is_finish
    assert deltas[-1].finish_reason == "error"


# --- capabilities ----------------------------------------------------------


@pytest.mark.parametrize(
    "provider_cls,expected_default",
    [
        (AnthropicProvider, "claude-opus-4-6"),
        (OpenAIProvider, "gpt-4.1-mini"),
        (OllamaProvider, "llama3.1"),
    ],
)
async def test_provider_capabilities_defaults(provider_cls, expected_default):
    cfg = ProviderConfig(name=provider_cls.name, defaults={"model": expected_default})
    caps = provider_cls(cfg).capabilities()
    assert caps.default_model == expected_default
    assert caps.max_context_tokens > 0


# --- streaming happy path (mocked) -----------------------------------------


class _StubEvent:
    def __init__(self, **kwargs):
        for k, v in kwargs.items():
            setattr(self, k, v)


def _text_event_seq(chunks):
    """Build a fake anthropic event stream that emits text deltas."""
    events = [
        _StubEvent(
            type="content_block_start",
            index=0,
            content_block=_StubEvent(type="text", text=""),
        )
    ]
    for c in chunks:
        events.append(
            _StubEvent(
                type="content_block_delta",
                index=0,
                delta=_StubEvent(type="text_delta", text=c),
            )
        )
    events.append(_StubEvent(type="content_block_stop", index=0))
    return events


class _FakeAnthropicStream:
    """Mimics the async context manager shape of anthropic.messages.stream."""

    def __init__(self, chunks):
        self._events = _text_event_seq(chunks)

    async def __aenter__(self):
        return self

    async def __aexit__(self, exc_type, exc, tb):
        return False

    def __aiter__(self):
        async def _gen():
            for ev in self._events:
                yield ev

        return _gen()

    async def get_final_message(self):
        class _Final:
            stop_reason = "end_turn"

            class usage:
                input_tokens = 3
                output_tokens = 5

        return _Final()


async def test_anthropic_chat_stream_happy_path(clean_env):
    clean_env.setenv("ANTHROPIC_API_KEY", "sk-test")
    cfg = ProviderConfig(
        name="anthropic",
        defaults={"model": "claude-opus-4-6", "max_tokens": 16},
        auth={"api_key_env": "ANTHROPIC_API_KEY"},
    )
    provider = AnthropicProvider(cfg)

    fake_client = mock.Mock()
    fake_client.messages.stream = mock.Mock(
        return_value=_FakeAnthropicStream(["Hel", "lo", "!"])
    )
    provider._client = fake_client  # inject

    req = ChatRequest(
        provider="anthropic",
        model="claude-opus-4-6",
        messages=[ChatMessage(role="user", content="hi")],
    )
    deltas: list[ChatDelta] = []
    async for d in provider.chat_stream(req):
        deltas.append(d)

    text = "".join(d.text for d in deltas if d.text)
    assert text == "Hello!"
    assert deltas[-1].finish_reason == "end_turn"
    assert deltas[-1].output_tokens == 5
