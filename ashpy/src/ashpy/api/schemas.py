"""Pydantic schemas for the FastAPI layer (auto-generate OpenAPI)."""

from __future__ import annotations

from typing import Optional

from pydantic import BaseModel, Field


class HealthResponse(BaseModel):
    status: str
    ashpy_version: str
    api_version: str
    features: dict[str, str]


class ProviderInfo(BaseModel):
    name: str
    default_model: str
    supports_tools: bool
    supports_vision: bool
    source: str


class ListProvidersResponse(BaseModel):
    providers: list[ProviderInfo]


class SwitchProviderRequest(BaseModel):
    provider: str = Field(..., description="Provider plugin name")
    model: Optional[str] = Field(
        default=None, description="Optional model override. Empty keeps the current default."
    )


class SwitchProviderResponse(BaseModel):
    ok: bool
    message: str


class SessionSummary(BaseModel):
    id: str
    provider: str
    model: str
    message_count: int


class ListSessionsResponse(BaseModel):
    sessions: list[SessionSummary]


class SessionDetail(BaseModel):
    summary: SessionSummary
    messages: list[dict]


class ChatRequest(BaseModel):
    session_id: Optional[str] = Field(
        default=None, description="Existing session to append to. Empty → new session."
    )
    prompt: str = Field(..., description="User message")
    provider: Optional[str] = Field(
        default=None, description="Override provider; empty uses the server default."
    )
    model: Optional[str] = Field(default=None, description="Override model")
    reset_session: bool = Field(
        default=False, description="Drop existing session state before running this turn."
    )


class CancelTurnResponse(BaseModel):
    ok: bool
    message: str


class DeleteSessionResponse(BaseModel):
    ok: bool
