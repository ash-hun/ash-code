"""FastAPI router for `/v1/commands*` (M6).

``render`` returns the rendered prompt without executing anything.
``run`` renders then calls back into the Rust QueryHost to actually
drive a turn — the result is streamed to the caller as SSE.
"""

from __future__ import annotations

import json
import time
from typing import Optional

from fastapi import APIRouter, HTTPException, Request
from pydantic import BaseModel, Field
from sse_starlette.sse import EventSourceResponse

from ..commands import get_registry as get_command_registry
from .query_client import QueryHostClient

router = APIRouter(prefix="/v1/commands", tags=["commands"])


class CommandInfo(BaseModel):
    name: str
    description: str
    allowed_tools: list[str]
    model: Optional[str] = None
    source_path: str


class ListCommandsResponse(BaseModel):
    commands: list[CommandInfo]


class CommandDetail(CommandInfo):
    prompt: str


class RenderCommandRequest(BaseModel):
    args: dict[str, str] = Field(default_factory=dict)
    context: dict[str, str] = Field(default_factory=dict)


class RenderCommandResponse(BaseModel):
    rendered_prompt: str
    allowed_tools: list[str]
    model: Optional[str] = None


class RunCommandRequest(RenderCommandRequest):
    session_id: Optional[str] = Field(
        default=None, description="Existing session to append to. Empty → new session."
    )
    provider: Optional[str] = Field(default=None, description="Override provider.")
    model: Optional[str] = Field(default=None, description="Override model.")
    reset_session: bool = False


class ReloadCommandsResponse(BaseModel):
    loaded: int
    errors: list[str]


def _to_info(command) -> CommandInfo:
    return CommandInfo(
        name=command.name,
        description=command.description,
        allowed_tools=list(command.allowed_tools),
        model=command.model or None,
        source_path=command.source_path,
    )


@router.get("", response_model=ListCommandsResponse)
async def list_commands() -> ListCommandsResponse:
    reg = get_command_registry()
    return ListCommandsResponse(commands=[_to_info(c) for c in reg.list_commands()])


@router.post("/reload", response_model=ReloadCommandsResponse)
async def reload_commands() -> ReloadCommandsResponse:
    reg = get_command_registry()
    loaded, errors = reg.reload()
    return ReloadCommandsResponse(loaded=loaded, errors=errors)


@router.get("/{name}", response_model=CommandDetail)
async def get_command(name: str) -> CommandDetail:
    reg = get_command_registry()
    command = reg.get(name)
    if command is None:
        raise HTTPException(status_code=404, detail=f"command not found: {name}")
    info = _to_info(command)
    return CommandDetail(**info.model_dump(), prompt=command.prompt)


@router.post("/{name}/render", response_model=RenderCommandResponse)
async def render_command(name: str, req: RenderCommandRequest) -> RenderCommandResponse:
    reg = get_command_registry()
    try:
        result = reg.render(name, args=req.args, context=req.context)
    except KeyError:
        raise HTTPException(status_code=404, detail=f"command not found: {name}")
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(status_code=400, detail=f"render failed: {exc}")
    return RenderCommandResponse(
        rendered_prompt=result.rendered_prompt,
        allowed_tools=result.allowed_tools,
        model=result.model or None,
    )


@router.post("/{name}/run")
async def run_command(name: str, req: RunCommandRequest, request: Request):
    """Render a command and drive a real agent turn via QueryHost.RunTurn.

    Returns Server-Sent Events: ``text`` / ``tool_call`` / ``tool_result``
    / ``finish`` / ``error`` / ``outcome`` / ``done``.
    """
    reg = get_command_registry()
    try:
        result = reg.render(name, args=req.args, context=req.context)
    except KeyError:
        raise HTTPException(status_code=404, detail=f"command not found: {name}")
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(status_code=400, detail=f"render failed: {exc}")

    # Command's own model beats the request override beats the session default.
    effective_model = req.model or result.model or ""
    session_id = req.session_id or f"cmd-{name}-{int(time.time() * 1000)}"
    client: QueryHostClient = request.app.state.query_client

    prompt = result.rendered_prompt

    async def event_stream():
        try:
            async for event in client.run_turn(
                session_id=session_id,
                prompt=prompt,
                provider=req.provider or "",
                model=effective_model,
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
