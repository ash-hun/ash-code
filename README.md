# ash-code

Docker 컨테이너 기반 코딩 하네스. LLM과 대화하면서 파일 읽기/쓰기, bash 실행 등을 자동으로 수행합니다.

**Skills**(재사용 가능한 프롬프트 템플릿)과 **Commands**(슬래시 커맨드)를 파일 하나만 추가하면 바로 사용할 수 있습니다.

## Quickstart

```bash
# 1. 환경변수 설정
cp .env.example .env
# .env 파일에서 ANTHROPIC_API_KEY (또는 OPENAI_API_KEY) 입력

# 2. 실행
docker compose --profile local-db up -d

# 3. 채팅 시작 (택 1)
docker exec -it ash-code ash tui                    # TUI
open http://localhost:8080/docs                      # Swagger UI
curl -s -N -X POST http://localhost:8080/v1/chat \   # HTTP
  -H "Content-Type: application/json" \
  -d '{"prompt": "Hello!"}'
```

> PostgreSQL 없이 빠르게 테스트하려면:
> ```bash
> ASH_SESSION_STORE=memory docker compose up -d ash-code
> ```

---

## 기본 기능

### 채팅

LLM에게 메시지를 보내고 응답을 스트리밍으로 받습니다. 대화는 세션 단위로 관리되며 PostgreSQL에 자동 저장됩니다.

```bash
# 새 세션에서 채팅
curl -s -N -X POST http://localhost:8080/v1/chat \
  -H "Content-Type: application/json" \
  -d '{"prompt": "Cargo.toml 파일을 분석해줘"}'

# 기존 세션 이어서 채팅
curl -s -N -X POST http://localhost:8080/v1/chat \
  -H "Content-Type: application/json" \
  -d '{"session_id": "my-session", "prompt": "아까 그 파일에서 버전을 올려줘"}'
```

### 내장 도구 (6개)

LLM이 대화 중 필요하면 자동으로 호출합니다.

| 도구 | 설명 |
|---|---|
| `bash` | 쉘 명령 실행 (TUI에서는 사용자 승인 필요) |
| `file_read` | 파일 읽기 |
| `file_write` | 파일 쓰기 |
| `file_edit` | 파일 부분 수정 |
| `grep` | 파일 내용 검색 |
| `glob` | 파일 패턴 검색 |

### 세션 관리

```bash
# 세션 목록
curl -s http://localhost:8080/v1/sessions

# 세션 상세 (메시지 포함)
curl -s http://localhost:8080/v1/sessions/my-session

# 세션 삭제
curl -s -X DELETE http://localhost:8080/v1/sessions/my-session

# 진행 중인 응답 취소
curl -s -X POST http://localhost:8080/v1/sessions/my-session/cancel

# 세션 이벤트 실시간 관찰 (SSE)
curl -N http://localhost:8080/v1/sessions/my-session/watch
```

### LLM 프로바이더

`.env`에서 `ASH_LLM_PROVIDER`를 설정합니다.

| 프로바이더 | 필요한 환경변수 |
|---|---|
| `anthropic` (기본) | `ANTHROPIC_API_KEY` |
| `openai` | `OPENAI_API_KEY`, `OPENAI_BASE_URL` (선택) |
| `vllm` | `VLLM_BASE_URL`, `VLLM_API_KEY` (선택) |
| `ollama` | `OLLAMA_BASE_URL` |

---

## Skills

**Skill**은 재사용 가능한 프롬프트 템플릿입니다. `SKILL.md` 파일 하나로 정의하고, API로 호출하면 LLM에게 전달할 프롬프트가 렌더링됩니다.

### 기존 스킬 사용하기

```bash
# 로딩된 스킬 목록 확인
curl -s http://localhost:8080/v1/skills | python3 -m json.tool
```

```json
{
  "skills": [
    {
      "name": "review-diff",
      "description": "Review the staged git diff with a configurable focus area",
      "triggers": ["review", "리뷰", "diff"],
      "allowed_tools": ["bash", "file_read", "grep"],
      "source_path": "/root/.ash/skills/review-diff/SKILL.md"
    },
    {
      "name": "summarize-file",
      "description": "Read a file and emit a structured summary",
      "triggers": ["summarize", "요약"],
      "allowed_tools": ["file_read"],
      "source_path": "/root/.ash/skills/summarize-file/SKILL.md"
    }
  ]
}
```

```bash
# 스킬 상세 보기 (템플릿 body 포함)
curl -s http://localhost:8080/v1/skills/review-diff

# 스킬 호출 (프롬프트 렌더링만, 실행 X)
curl -s -X POST http://localhost:8080/v1/skills/review-diff/invoke \
  -H "Content-Type: application/json" \
  -d '{"args": {"focus": "security vulnerabilities"}}'
```

응답으로 렌더링된 프롬프트가 돌아옵니다:

```json
{
  "rendered_prompt": "You are reviewing a staged git diff.\n\nSteps:\n1. Run `git diff --staged`...\n3. Focus on: security vulnerabilities\n...",
  "allowed_tools": ["bash", "file_read", "grep"],
  "model": "claude-opus-4-5"
}
```

이 프롬프트를 `/v1/chat`에 전달하면 LLM이 스킬 지시대로 작업합니다.

### 새 스킬 만들기

`skills/` 디렉토리 아래에 `<이름>/SKILL.md` 파일을 생성합니다.

```
skills/
  review-diff/SKILL.md      # 기존
  summarize-file/SKILL.md   # 기존
  my-skill/SKILL.md          # 새로 추가
```

**SKILL.md 포맷:**

```markdown
---
name: explain-error
description: 에러 메시지를 분석하고 수정 방법을 제안
triggers: ["explain", "error", "에러"]
allowed_tools: ["bash", "file_read", "grep"]
model: ""
---
사용자가 에러를 만났습니다.

1. 에러 메시지를 분석하세요.
2. 파일 경로가 있으면 `file_read`로 해당 코드를 확인하세요.
3. 원인을 설명하고 수정 방법을 제안하세요.

에러 내용:
{{ args.error | default("(에러를 입력하세요)") }}
```

| 필드 | 필수 | 설명 |
|---|---|---|
| `name` | O | 스킬 이름 (미지정 시 디렉토리명 사용) |
| `description` | - | 스킬 설명 |
| `triggers` | - | 트리거 키워드 목록 |
| `allowed_tools` | - | 사용 가능한 도구 목록 |
| `model` | - | 모델 오버라이드 (빈 문자열이면 세션 기본값) |

**`---` 아래의 body** 부분이 Jinja2 템플릿입니다. `{{ args.키 }}` 로 호출 시 전달한 인자를 참조합니다.

**Hot-reload:** 파일을 저장하면 자동으로 반영됩니다 (Linux: inotify, macOS/Windows: `ASH_SKILLS_POLLING=1` 필요). 수동으로 하려면:

```bash
curl -s -X POST http://localhost:8080/v1/skills/reload
# → {"loaded": 3, "errors": []}
```

---

## Commands

**Command**는 슬래시 커맨드 스타일의 실행 가능한 작업입니다. TOML 파일로 정의하고, API를 통해 **렌더링만** 하거나 **LLM 실행까지** 할 수 있습니다.

### 기존 커맨드 사용하기

```bash
# 로딩된 커맨드 목록
curl -s http://localhost:8080/v1/commands | python3 -m json.tool
```

```json
{
  "commands": [
    {"name": "review", "description": "Review the staged git diff with a configurable focus", ...},
    {"name": "summarize", "description": "Read a file and emit a structured summary", ...},
    {"name": "test", "description": "Run project tests and analyze the output", ...}
  ]
}
```

**렌더링만 (프롬프트 확인):**

```bash
curl -s -X POST http://localhost:8080/v1/commands/review/render \
  -H "Content-Type: application/json" \
  -d '{"args": {"focus": "performance"}}'
# → {"rendered_prompt": "You are reviewing...", "allowed_tools": [...]}
```

**실행 (LLM이 실제로 작업 수행, SSE 스트리밍):**

```bash
curl -s -N -X POST http://localhost:8080/v1/commands/test/run \
  -H "Content-Type: application/json" \
  -d '{"args": {"target": "unit tests only"}}'
```

```
event: text
data: {"type":"text","text":"Looking at the repo root..."}

event: tool_call
data: {"type":"tool_call","name":"bash","arguments":"{\"command\":\"cargo test\"}"}

event: tool_result
data: {"type":"tool_result","name":"bash","ok":true,"stdout":"test result: ok. 8 passed..."}

event: outcome
data: {"type":"outcome","stop_reason":"end_turn","turns_taken":1}

event: done
data: [DONE]
```

`run`은 세션 관리도 지원합니다:

```bash
curl -s -N -X POST http://localhost:8080/v1/commands/review/run \
  -H "Content-Type: application/json" \
  -d '{
    "args": {"focus": "security"},
    "session_id": "my-review-session",
    "provider": "anthropic",
    "model": "claude-sonnet-4-20250514"
  }'
```

### 새 커맨드 만들기

`commands/` 디렉토리에 `<이름>.toml` 파일을 추가합니다.

```
commands/
  review.toml       # 기존
  summarize.toml    # 기존
  test.toml         # 기존
  deploy-check.toml  # 새로 추가
```

**TOML 포맷:**

```toml
name = "deploy-check"
description = "Run pre-deployment checks"
allowed_tools = ["bash", "file_read"]
model = ""
prompt = """
배포 전 점검을 수행합니다.

1. `bash`로 `git status`를 확인하여 커밋되지 않은 변경사항이 있는지 확인
2. `bash`로 테스트를 실행 ({{ args.test_cmd | default("cargo test") }})
3. `file_read`로 설정 파일({{ args.config | default("docker-compose.yml") }})을 검토
4. 점검 결과를 요약하고 배포 가능 여부를 판단
"""
```

| 필드 | 필수 | 설명 |
|---|---|---|
| `name` | O | 커맨드 이름 (미지정 시 파일명 사용) |
| `prompt` | O | Jinja2 프롬프트 템플릿 |
| `description` | - | 커맨드 설명 |
| `allowed_tools` | - | 사용 가능한 도구 목록 |
| `model` | - | 모델 오버라이드 |

**Reload:** 커맨드는 시작 시 로딩됩니다. 파일 추가 후 수동으로 리로드:

```bash
curl -s -X POST http://localhost:8080/v1/commands/reload
# → {"loaded": 4, "errors": []}
```

### Skill vs Command 차이

| | Skill | Command |
|---|---|---|
| **파일 포맷** | Markdown (`SKILL.md`) | TOML (`.toml`) |
| **위치** | `skills/<name>/SKILL.md` | `commands/<name>.toml` |
| **API** | `/v1/skills/{name}/invoke` | `/v1/commands/{name}/render` 또는 `/run` |
| **Hot-reload** | 자동 (파일 변경 감지) | 수동 (`/reload`) |
| **LLM 실행** | invoke는 렌더링만, 별도로 `/v1/chat` 호출 필요 | `/run`이 렌더링 + LLM 실행까지 한 번에 |
| **용도** | 재사용 가능한 프롬프트 (리뷰, 요약 등) | 즉시 실행 가능한 작업 (테스트, 배포 점검 등) |

---

## 프로젝트 구조

```
crates/          Rust 워크스페이스
  cli/             CLI (ash serve, ash tui)
  core/            세션 저장소 (PostgreSQL / Memory)
  api/             QueryHost gRPC 서버
  query/           턴 루프 엔진
  tools/           내장 도구 레지스트리
  tui/             TUI (ratatui)
  ipc/             Rust ↔ Python gRPC 통신
  bus/             세션 이벤트 버스
ashpy/           Python 사이드카
  providers/       LLM 프로바이더 플러그인
  api/             FastAPI + gRPC 클라이언트
  skills/          스킬 레지스트리
  commands/        커맨드 레지스트리
  middleware/      하네스 미들웨어
proto/           gRPC 프로토콜 정의
docker/          Dockerfile, supervisord
skills/          사용자 스킬 (SKILL.md)
commands/        사용자 커맨드 (TOML)
samples/         예시 스킬, 커맨드, 미들웨어
```

## 전체 API 레퍼런스

스택 실행 후 `http://localhost:8080/docs`에서 Swagger UI로 확인할 수 있습니다.

| Method | Path | Description |
|---|---|---|
| `POST` | `/v1/chat` | 채팅 (SSE 스트림) |
| `GET` | `/v1/sessions` | 세션 목록 |
| `GET` | `/v1/sessions/{id}` | 세션 상세 |
| `DELETE` | `/v1/sessions/{id}` | 세션 삭제 |
| `POST` | `/v1/sessions/{id}/cancel` | 진행 중인 턴 취소 |
| `GET` | `/v1/sessions/{id}/watch` | 세션 이벤트 관찰 (SSE) |
| `GET` | `/v1/skills` | 스킬 목록 |
| `GET` | `/v1/skills/{name}` | 스킬 상세 |
| `POST` | `/v1/skills/{name}/invoke` | 스킬 프롬프트 렌더링 |
| `POST` | `/v1/skills/reload` | 스킬 리로드 |
| `GET` | `/v1/commands` | 커맨드 목록 |
| `GET` | `/v1/commands/{name}` | 커맨드 상세 |
| `POST` | `/v1/commands/{name}/render` | 커맨드 프롬프트 렌더링 |
| `POST` | `/v1/commands/{name}/run` | 커맨드 실행 (SSE 스트림) |
| `POST` | `/v1/commands/reload` | 커맨드 리로드 |
| `GET` | `/v1/health` | 헬스 체크 |

## 테스트

```bash
# Rust 단위 테스트
docker build --target rust-builder -f docker/Dockerfile -t ash-test .
docker run --rm ash-test cargo test --release --workspace

# Python 테스트
cd ashpy && uv run pytest -q

# E2E 스모크 테스트 (실행 중인 스택 필요)
./scripts/e2e-smoke.sh

# 성능 테스트 (실행 중인 스택 필요)
python3 scripts/perf-smoke.py --turns 50
```
