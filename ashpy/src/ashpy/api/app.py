"""FastAPI application factory.

Wires together the HTTP endpoints. Swagger UI is served at ``/docs`` and
the raw OpenAPI spec at ``/openapi.json`` (FastAPI defaults).
"""

from __future__ import annotations

import json
import os
import time

from fastapi import FastAPI, HTTPException
from fastapi.middleware.cors import CORSMiddleware
from sse_starlette.sse import EventSourceResponse

from .. import __version__
from ..providers import get_registry
from .query_client import DEFAULT_QUERY_HOST_ENDPOINT, QueryHostClient
from .schemas import (
    ChatRequest,
    DeleteSessionResponse,
    HealthResponse,
    ListProvidersResponse,
    ListSessionsResponse,
    ProviderInfo,
    SessionDetail,
    SessionSummary,
    SwitchProviderRequest,
    SwitchProviderResponse,
)

API_TITLE = "ash-code API"
API_DESCRIPTION = (
    "HTTP surface of the ash-code coding harness. "
    "Delegates turn execution to the Rust QueryHost gRPC service."
)


def create_app(
    query_host_endpoint: str = DEFAULT_QUERY_HOST_ENDPOINT,
) -> FastAPI:
    app = FastAPI(
        title=API_TITLE,
        version=__version__,
        description=API_DESCRIPTION,
    )
    app.add_middleware(
        CORSMiddleware,
        allow_origins=["*"],  # localhost dev harness; tighten in M9
        allow_methods=["*"],
        allow_headers=["*"],
    )
    app.state.query_client = QueryHostClient(query_host_endpoint)

    # --- Health -----------------------------------------------------------

    @app.get("/v1/health", response_model=HealthResponse, tags=["health"])
    async def health() -> HealthResponse:
        return HealthResponse(
            status="ok",
            ashpy_version=__version__,
            api_version="v1",
            features={
                "http": "v1",
                "llm": "v1",
                "harness": "v1",
                "skills": "planned",
                "commands": "planned",
            },
        )

    # --- LLM providers ----------------------------------------------------

    @app.get(
        "/v1/llm/providers",
        response_model=ListProvidersResponse,
        tags=["llm"],
    )
    async def list_providers() -> ListProvidersResponse:
        reg = get_registry()
        out: list[ProviderInfo] = []
        for name in reg.list_names():
            cfg = reg.configs()[name]
            try:
                caps = reg.capabilities(name)
            except Exception:
                caps = None
            out.append(
                ProviderInfo(
                    name=name,
                    default_model=(caps.default_model if caps else cfg.model()),
                    supports_tools=bool(caps.supports_tools) if caps else False,
                    supports_vision=bool(caps.supports_vision) if caps else False,
                    source=cfg.source,
                )
            )
        return ListProvidersResponse(providers=out)

    @app.post(
        "/v1/llm/switch",
        response_model=SwitchProviderResponse,
        tags=["llm"],
    )
    async def switch_provider(req: SwitchProviderRequest) -> SwitchProviderResponse:
        reg = get_registry()
        try:
            reg.switch(req.provider, req.model or "")
        except KeyError as exc:
            raise HTTPException(status_code=404, detail=str(exc))
        return SwitchProviderResponse(
            ok=True,
            message=f"switched to {req.provider}"
            + (f" ({req.model})" if req.model else ""),
        )

    # --- Sessions ---------------------------------------------------------

    @app.get(
        "/v1/sessions",
        response_model=ListSessionsResponse,
        tags=["sessions"],
    )
    async def list_sessions() -> ListSessionsResponse:
        client: QueryHostClient = app.state.query_client
        raw = await client.list_sessions()
        return ListSessionsResponse(
            sessions=[SessionSummary(**s) for s in raw],
        )

    @app.get(
        "/v1/sessions/{session_id}",
        response_model=SessionDetail,
        tags=["sessions"],
    )
    async def get_session(session_id: str) -> SessionDetail:
        client: QueryHostClient = app.state.query_client
        raw = await client.get_session(session_id)
        if raw is None:
            raise HTTPException(status_code=404, detail=f"session not found: {session_id}")
        return SessionDetail(
            summary=SessionSummary(**raw["summary"]),
            messages=raw["messages"],
        )

    @app.delete(
        "/v1/sessions/{session_id}",
        response_model=DeleteSessionResponse,
        tags=["sessions"],
    )
    async def delete_session(session_id: str) -> DeleteSessionResponse:
        client: QueryHostClient = app.state.query_client
        ok = await client.delete_session(session_id)
        if not ok:
            raise HTTPException(status_code=404, detail="session not found")
        return DeleteSessionResponse(ok=True)

    # --- Chat (SSE) -------------------------------------------------------

    @app.post("/v1/chat", tags=["chat"])
    async def chat(req: ChatRequest):
        client: QueryHostClient = app.state.query_client
        session_id = req.session_id or f"sess-{int(time.time() * 1000)}"

        async def event_stream():
            try:
                async for event in client.run_turn(
                    session_id=session_id,
                    prompt=req.prompt,
                    provider=req.provider or "",
                    model=req.model or "",
                    reset_session=req.reset_session,
                ):
                    yield {"event": event["type"], "data": json.dumps(event)}
            except Exception as exc:  # noqa: BLE001
                yield {
                    "event": "error",
                    "data": json.dumps({"type": "error", "message": str(exc)}),
                }
            finally:
                yield {"event": "done", "data": "[DONE]"}

        return EventSourceResponse(event_stream())

    return app
