"""FastAPI router for `/v1/skills*` (M5)."""

from __future__ import annotations

from typing import Optional

from fastapi import APIRouter, HTTPException
from pydantic import BaseModel, Field

from ..skills import get_registry as get_skill_registry

router = APIRouter(prefix="/v1/skills", tags=["skills"])


class SkillInfo(BaseModel):
    name: str
    description: str
    triggers: list[str]
    allowed_tools: list[str]
    model: Optional[str] = None
    source_path: str


class ListSkillsResponse(BaseModel):
    skills: list[SkillInfo]


class SkillDetail(SkillInfo):
    body: str


class InvokeSkillRequest(BaseModel):
    args: dict[str, str] = Field(default_factory=dict)
    context: dict[str, str] = Field(default_factory=dict)


class InvokeSkillResponse(BaseModel):
    rendered_prompt: str
    allowed_tools: list[str]
    model: Optional[str] = None


class ReloadSkillsResponse(BaseModel):
    loaded: int
    errors: list[str]


def _to_info(skill) -> SkillInfo:
    return SkillInfo(
        name=skill.name,
        description=skill.description,
        triggers=list(skill.triggers),
        allowed_tools=list(skill.allowed_tools),
        model=skill.model or None,
        source_path=skill.source_path,
    )


@router.get("", response_model=ListSkillsResponse)
async def list_skills() -> ListSkillsResponse:
    reg = get_skill_registry()
    return ListSkillsResponse(skills=[_to_info(s) for s in reg.list_skills()])


@router.post("/reload", response_model=ReloadSkillsResponse)
async def reload_skills() -> ReloadSkillsResponse:
    reg = get_skill_registry()
    loaded, errors = reg.reload()
    return ReloadSkillsResponse(loaded=loaded, errors=errors)


@router.get("/{name}", response_model=SkillDetail)
async def get_skill(name: str) -> SkillDetail:
    reg = get_skill_registry()
    skill = reg.get(name)
    if skill is None:
        raise HTTPException(status_code=404, detail=f"skill not found: {name}")
    info = _to_info(skill)
    return SkillDetail(**info.model_dump(), body=skill.body)


@router.post("/{name}/invoke", response_model=InvokeSkillResponse)
async def invoke_skill(name: str, req: InvokeSkillRequest) -> InvokeSkillResponse:
    reg = get_skill_registry()
    try:
        result = reg.invoke(name, args=req.args, context=req.context)
    except KeyError:
        raise HTTPException(status_code=404, detail=f"skill not found: {name}")
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(status_code=400, detail=f"render failed: {exc}")
    return InvokeSkillResponse(
        rendered_prompt=result.rendered_prompt,
        allowed_tools=result.allowed_tools,
        model=result.model or None,
    )
