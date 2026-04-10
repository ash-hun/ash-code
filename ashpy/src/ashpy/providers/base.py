"""Provider plugin contract.

All LLM providers — the four built-ins (``anthropic``, ``openai``,
``vllm``, ``ollama``) and any third-party drop-in — implement this
interface. The built-ins are not privileged: they load through the same
``ProviderRegistry`` path a user plugin would use.

Graceful degradation
--------------------
A provider is allowed to instantiate even when its credentials are
missing. In that case ``health()`` reports ``UNCONFIGURED`` and
``chat_stream()`` yields a single ``ChatDelta(error=...)`` explaining
what environment variable the user needs to set. This keeps the sidecar
healthy when the user has only set up one provider and lets
``ListProviders`` enumerate the full catalog.
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, AsyncIterator, ClassVar, Optional


# --- Errors -----------------------------------------------------------------


class ProviderError(RuntimeError):
    """Base class for provider runtime errors."""


class ProviderNotConfigured(ProviderError):
    """Raised (or reported via HealthStatus) when required credentials are missing."""

    def __init__(self, provider: str, missing: list[str]):
        self.provider = provider
        self.missing = missing
        super().__init__(
            f"provider '{provider}' is not configured; missing env vars: {', '.join(missing)}"
        )


# --- Value types ------------------------------------------------------------


@dataclass(frozen=True)
class ProviderCaps:
    supports_tools: bool = False
    supports_vision: bool = False
    max_context_tokens: int = 0
    default_model: str = ""


class HealthState(str, Enum):
    OK = "ok"
    UNCONFIGURED = "unconfigured"
    UNREACHABLE = "unreachable"
    ERROR = "error"


@dataclass(frozen=True)
class HealthStatus:
    state: HealthState
    message: str = ""


@dataclass
class ProviderConfig:
    """Everything a provider needs at construction time.

    Sourced from (in order): ``providers/<name>.toml`` on the mounted
    volume, ``ASH_*`` environment variables, and hard-coded defaults.
    """

    name: str
    module: str = ""
    class_name: str = ""
    defaults: dict[str, Any] = field(default_factory=dict)
    auth: dict[str, Any] = field(default_factory=dict)
    source: str = "builtin"  # "builtin" | "plugin:<path>"

    def model(self) -> str:
        return str(self.defaults.get("model", ""))

    def temperature(self) -> float:
        raw = self.defaults.get("temperature", 0.2)
        try:
            return float(raw)
        except (TypeError, ValueError):
            return 0.2


@dataclass
class ChatMessage:
    role: str  # "system" | "user" | "assistant" | "tool"
    content: str
    tool_call_id: str = ""


@dataclass
class ToolSpec:
    """Tool advertised to the LLM. Mirrors proto ``ToolSpec``."""

    name: str
    description: str
    input_schema: dict  # parsed JSON Schema


@dataclass
class ChatRequest:
    provider: str
    model: str
    messages: list[ChatMessage]
    temperature: float = 0.2
    tools: list[ToolSpec] = field(default_factory=list)


@dataclass
class ToolCallDelta:
    """Tool-use request emitted by the model mid-stream."""

    id: str
    name: str
    arguments: str  # UTF-8 JSON


@dataclass
class ChatDelta:
    """Unified streaming delta.

    Exactly one of ``text``, ``tool_call``, ``finish``, or ``error`` is
    set on a given delta. M7 enables ``tool_call`` so HITL works.
    """

    text: str = ""
    tool_call: Optional["ToolCallDelta"] = None
    finish_reason: str = ""
    input_tokens: int = 0
    output_tokens: int = 0
    error: str = ""

    @property
    def is_finish(self) -> bool:
        return bool(self.finish_reason)

    @property
    def is_error(self) -> bool:
        return bool(self.error)

    @property
    def is_tool_call(self) -> bool:
        return self.tool_call is not None


# --- Contract ---------------------------------------------------------------


class LlmProvider(ABC):
    """Abstract base every provider plugin implements."""

    #: Stable identifier exposed via ``ListProviders``. Set on subclasses.
    name: ClassVar[str] = ""

    def __init__(self, config: ProviderConfig) -> None:
        self.config = config

    @abstractmethod
    def capabilities(self) -> ProviderCaps:  # pragma: no cover — abstract
        ...

    @abstractmethod
    async def chat_stream(self, req: ChatRequest) -> AsyncIterator[ChatDelta]:  # pragma: no cover
        ...

    @abstractmethod
    async def health(self) -> HealthStatus:  # pragma: no cover
        ...

    def source(self) -> str:
        return self.config.source or "builtin"


async def unconfigured_stream(provider: str, missing: list[str]) -> AsyncIterator[ChatDelta]:
    """Convenience helper for providers that can't run without credentials.

    Yields a single error delta followed by a finish so the client sees a
    clean termination.
    """
    yield ChatDelta(
        error=(
            f"provider '{provider}' is not configured; set {', '.join(missing)} "
            "in the environment or providers/<name>.toml"
        )
    )
    yield ChatDelta(finish_reason="error")
