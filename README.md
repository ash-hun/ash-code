# AshCode

<img src="./assets/screenshot.png" />

**Containerized AI coding harness** — Rust turn-loop engine + Python sidecar, packaged in a single Docker container.

LLM과 대화하며 파일 읽기/쓰기, 코드 검색, 셸 실행을 자동 수행합니다. **Skills**(프롬프트 템플릿)과 **Commands**(슬래시 커맨드)를 파일 하나 추가로 확장할 수 있습니다.

## Highlights

- **Multi-provider LLM** — Anthropic · OpenAI · vLLM · Ollama, 런타임 전환
- **6 built-in tools** — `bash` · `file_read` · `file_write` · `file_edit` · `grep` · `glob`
- **Human-in-the-Loop** — `bash` 실행 시 TUI에서 사용자 승인 요구
- **Hot-reloadable Skills & Commands** — 파일 저장만으로 즉시 반영
- **Session persistence** — PostgreSQL 기반, 컨테이너 재시작 후에도 대화 유지
- **Dual interface** — TUI (ratatui) + HTTP API (Swagger UI)
- **Harness middleware** — `OnTurnStart` · `OnToolCall` · `OnStreamDelta` · `OnTurnEnd` 훅
- **Mid-turn cancel** — HTTP/gRPC/TUI 어디서든 진행 중인 응답 취소
- **Session watch** — SSE로 세션 이벤트 실시간 관찰

---

## Quickstart

```bash
# 1. 환경변수 설정
cp .env.example .env
# .env 파일에서 ANTHROPIC_API_KEY (또는 OPENAI_API_KEY 등) 입력

# 2. 실행 (PostgreSQL 포함)
docker compose --profile local-db up -d

# 3. 사용 시작
docker exec -it ash-code ash tui        # TUI
open http://localhost:8080/docs          # Swagger UI
```

> PostgreSQL 없이 빠르게 테스트하려면:
> ```bash
> ASH_SESSION_STORE=memory docker compose up -d ash-code
> ```

---

## Usage

### TUI

```bash
docker exec -it ash-code ash tui
```

| 키 | 동작 |
|---|---|
| `Enter` | 메시지 전송 |
| `/` | 슬래시 팔레트 (등록된 commands/skills 목록) |
| `↑` `↓` `Tab` | 팔레트 항목 이동 |
| `Esc` | 진행 중인 턴 취소 / 팔레트 닫기 |
| `PageUp` `PageDown` | 스크롤 |
| `Ctrl-C` | 종료 |

`bash` 도구 호출 시 승인 모달이 표시됩니다: `[1] Yes` · `[2] No` · `[3] 피드백 입력`.

### HTTP API

스택 실행 후 `http://localhost:8080/docs`에서 Swagger UI로 전체 API를 확인할 수 있습니다.

<details>
<summary><b>Chat — LLM과 대화하기</b></summary>

```bash
# 기본 채팅
curl -s -N -X POST http://localhost:8080/v1/chat \
  -H "Content-Type: application/json" \
  -d '{"prompt": "Hello!"}'

# 세션 지정 (대화 이어가기)
curl -s -N -X POST http://localhost:8080/v1/chat \
  -H "Content-Type: application/json" \
  -d '{"session_id": "my-session", "prompt": "이전 대화를 이어서"}'

# 모델 오버라이드
curl -s -N -X POST http://localhost:8080/v1/chat \
  -H "Content-Type: application/json" \
  -d '{"prompt": "hello", "provider": "anthropic", "model": "claude-sonnet-4-20250514"}'
```

응답은 SSE 스트림:

| event | 설명 |
|---|---|
| `text` | 어시스턴트 텍스트 (토큰 단위) |
| `tool_call` | 도구 호출 요청 |
| `tool_result` | 도구 실행 결과 |
| `finish` | 턴 종료 + 토큰 정보 |
| `outcome` | 최종 결과 |
| `done` | 스트림 종료 |

</details>

<details>
<summary><b>Sessions — 세션 관리</b></summary>

```bash
# 세션 목록
curl -s http://localhost:8080/v1/sessions

# 세션 상세 (메시지 포함)
curl -s http://localhost:8080/v1/sessions/my-session

# 세션 삭제
curl -s -X DELETE http://localhost:8080/v1/sessions/my-session

# 진행 중인 턴 취소
curl -s -X POST http://localhost:8080/v1/sessions/my-session/cancel

# 실시간 이벤트 관찰 (SSE)
curl -N http://localhost:8080/v1/sessions/my-session/watch
```

</details>

<details>
<summary><b>전체 API 레퍼런스</b></summary>

| Method | Path | Description |
|---|---|---|
| `POST` | `/v1/chat` | 채팅 (SSE) |
| `GET` | `/v1/sessions` | 세션 목록 |
| `GET` | `/v1/sessions/{id}` | 세션 상세 |
| `DELETE` | `/v1/sessions/{id}` | 세션 삭제 |
| `POST` | `/v1/sessions/{id}/cancel` | 턴 취소 |
| `GET` | `/v1/sessions/{id}/watch` | 이벤트 관찰 (SSE) |
| `GET` | `/v1/skills` | 스킬 목록 |
| `GET` | `/v1/skills/{name}` | 스킬 상세 |
| `POST` | `/v1/skills/{name}/invoke` | 스킬 렌더링 |
| `POST` | `/v1/skills/reload` | 스킬 리로드 |
| `GET` | `/v1/commands` | 커맨드 목록 |
| `GET` | `/v1/commands/{name}` | 커맨드 상세 |
| `POST` | `/v1/commands/{name}/render` | 커맨드 렌더링 |
| `POST` | `/v1/commands/{name}/run` | 커맨드 실행 (SSE) |
| `POST` | `/v1/commands/reload` | 커맨드 리로드 |
| `GET` | `/v1/llm/providers` | 프로바이더 목록 |
| `POST` | `/v1/llm/switch` | 프로바이더 전환 |
| `GET` | `/v1/health` | 헬스 체크 |

</details>

---

## Customization

### Skills

**Skill**은 재사용 가능한 프롬프트 템플릿입니다. `skills/<name>/SKILL.md` 파일을 추가하면 자동으로 로딩됩니다.

<details>
<summary><b>기존 스킬 사용하기</b></summary>

```bash
# 등록된 스킬 목록
curl -s http://localhost:8080/v1/skills | python3 -m json.tool

# 스킬 렌더링 (실행 X, 프롬프트만 반환)
curl -s -X POST http://localhost:8080/v1/skills/review-diff/invoke \
  -H "Content-Type: application/json" \
  -d '{"args": {"focus": "security vulnerabilities"}}'
```

TUI에서는 `/`를 입력하면 팔레트에 스킬 목록이 표시됩니다. 선택하면 렌더링 → LLM 실행까지 자동 수행됩니다.

</details>

<details>
<summary><b>새 스킬 만들기</b></summary>

`skills/<name>/SKILL.md` 파일을 생성합니다:

```markdown
---
name: explain-error
description: 에러 메시지를 분석하고 수정 방법 제안
triggers: ["explain", "error"]
allowed_tools: ["bash", "file_read", "grep"]
model: ""
---
사용자가 에러를 만났습니다.

1. 에러 메시지를 분석하세요.
2. 파일 경로가 있으면 `file_read`로 확인하세요.
3. 원인을 설명하고 수정 방법을 제안하세요.

에러 내용: {{ args.error | default("(에러를 입력하세요)") }}
```

| 필드 | 필수 | 설명 |
|---|---|---|
| `name` | O | 스킬 이름 |
| `description` | - | 설명 |
| `triggers` | - | 트리거 키워드 |
| `allowed_tools` | - | 사용 가능 도구 |
| `model` | - | 모델 오버라이드 |

Body는 [Jinja2](https://jinja.palletsprojects.com/) 템플릿입니다. `{{ args.key }}`로 호출 시 인자를 참조합니다.

**Hot-reload**: Linux에서는 inotify로 자동 반영. macOS/Windows는 `ASH_SKILLS_POLLING=1` 설정.

</details>

### Commands

**Command**는 슬래시 커맨드 형태의 실행 가능한 작업입니다. `commands/<name>.toml` 파일로 정의합니다.

<details>
<summary><b>기존 커맨드 사용하기</b></summary>

```bash
# 등록된 커맨드 목록
curl -s http://localhost:8080/v1/commands | python3 -m json.tool

# 렌더링만 (프롬프트 확인)
curl -s -X POST http://localhost:8080/v1/commands/test/render \
  -H "Content-Type: application/json" \
  -d '{"args": {"target": "unit tests"}}'

# 실행 (LLM이 실제 작업 수행, SSE 스트림)
curl -s -N -X POST http://localhost:8080/v1/commands/test/run \
  -H "Content-Type: application/json" \
  -d '{"args": {"target": "unit tests"}}'
```

TUI에서 `/test` 입력 시 팔레트에서 선택 → 렌더링 → LLM 실행까지 자동 수행.

</details>

<details>
<summary><b>새 커맨드 만들기</b></summary>

`commands/<name>.toml` 파일을 생성합니다:

```toml
name = "deploy-check"
description = "Run pre-deployment checks"
allowed_tools = ["bash", "file_read"]
prompt = """
배포 전 점검을 수행합니다.

1. `bash`로 `git status` 확인
2. 테스트 실행 ({{ args.test_cmd | default("cargo test") }})
3. 설정 파일 검토 ({{ args.config | default("docker-compose.yml") }})
4. 점검 결과 요약 + 배포 가능 여부 판단
"""
```

| 필드 | 필수 | 설명 |
|---|---|---|
| `name` | O | 커맨드 이름 |
| `prompt` | O | Jinja2 프롬프트 템플릿 |
| `description` | - | 설명 |
| `allowed_tools` | - | 사용 가능 도구 |
| `model` | - | 모델 오버라이드 |

파일 추가 후: `curl -s -X POST http://localhost:8080/v1/commands/reload`

</details>

<details>
<summary><b>Skill vs Command 차이</b></summary>

| | Skill | Command |
|---|---|---|
| 파일 포맷 | Markdown (`SKILL.md`) | TOML (`.toml`) |
| 위치 | `skills/<name>/SKILL.md` | `commands/<name>.toml` |
| API | `invoke` → 렌더링만 | `render` (렌더링) 또는 `run` (실행까지) |
| Hot-reload | 자동 (파일 변경 감지) | 수동 (`/reload`) |
| 용도 | 재사용 프롬프트 (리뷰, 요약) | 즉시 실행 작업 (테스트, 배포 점검) |

</details>

### Middleware

Harness 미들웨어로 턴 루프의 동작을 커스터마이즈할 수 있습니다.

<details>
<summary><b>미들웨어 구조 및 확장</b></summary>

4가지 훅을 선택적으로 오버라이드합니다:

```python
from ashpy.middleware.base import Middleware, HookDecision, allow, deny

class MyMiddleware(Middleware):
    priority = 50  # 낮을수록 먼저 실행
    name = "my_middleware"

    async def on_turn_start(self, ctx) -> HookDecision:
        return allow()

    async def on_tool_call(self, event) -> HookDecision:
        if event.tool_name == "bash" and "rm -rf" in event.arguments:
            return deny("destructive command blocked")
        return allow()

    async def on_turn_end(self, result) -> None:
        print(f"Turn finished: {result.stop_reason}")
```

**내장 미들웨어**: `LoggingMiddleware` (모든 훅 JSON 로깅), `BashGuardMiddleware` (위험 명령 차단)

**확장**: `samples/middleware/token_budget_middleware.py` 참고

</details>

### LLM Providers

<details>
<summary><b>프로바이더 설정</b></summary>

`.env`에서 설정:

| 프로바이더 | 환경변수 |
|---|---|
| `anthropic` (기본) | `ANTHROPIC_API_KEY` |
| `openai` | `OPENAI_API_KEY`, `OPENAI_BASE_URL` (선택) |
| `vllm` | `VLLM_BASE_URL`, `VLLM_API_KEY` (선택) |
| `ollama` | `OLLAMA_BASE_URL` |

```bash
# 런타임 전환
curl -s -X POST http://localhost:8080/v1/llm/switch \
  -H "Content-Type: application/json" \
  -d '{"provider": "openai", "model": "gpt-4o"}'
```

</details>

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Docker Container                         │
│                                                             │
│  ┌──────────────────────┐    ┌───────────────────────────┐  │
│  │  Rust Host            │    │  Python Sidecar (ashpy)   │  │
│  │                      │    │                           │  │
│  │  ┌────────────────┐  │    │  ┌─────────────────────┐  │  │
│  │  │ TUI (ratatui)  │  │    │  │ FastAPI (HTTP/SSE)  │  │  │
│  │  └────────┬───────┘  │    │  └─────────┬───────────┘  │  │
│  │           │          │    │            │              │  │
│  │  ┌────────▼───────┐  │    │  ┌─────────▼───────────┐  │  │
│  │  │ QueryEngine    │◄─┼gRPC┼──┤ LLM Providers       │  │  │
│  │  │ (turn loop)    │──┼────┼─►│ Skills / Commands    │  │  │
│  │  └────────┬───────┘  │    │  │ Middleware           │  │  │
│  │           │          │    │  └─────────────────────┘  │  │
│  │  ┌────────▼───────┐  │    │                           │  │
│  │  │ ToolRegistry   │  │    └───────────────────────────┘  │
│  │  │ (6 built-in)   │  │                                   │
│  │  └────────────────┘  │    ┌───────────────────────────┐  │
│  │                      │    │  PostgreSQL (optional)     │  │
│  │  ┌────────────────┐  │    │  Session persistence      │  │
│  │  │ SessionBus     │  │    └───────────────────────────┘  │
│  │  │ (event pub/sub)│  │                                   │
│  │  └────────────────┘  │                                   │
│  └──────────────────────┘                                   │
└─────────────────────────────────────────────────────────────┘
```

<details>
<summary><b>프로젝트 구조</b></summary>

```
crates/              Rust workspace
  cli/                 CLI entrypoint (ash serve, ash tui)
  core/                SessionStore trait + Postgres/Memory backends
  api/                 QueryHost gRPC server
  query/               Turn loop engine + CancellationToken
  tools/               Built-in tool registry
  tui/                 TUI (ratatui) + HITL approval + slash palette
  ipc/                 gRPC codegen + SidecarClient
  bus/                 SessionBus (broadcast pub/sub)
ashpy/               Python sidecar
  providers/           LLM provider plugins
  api/                 FastAPI + QueryHost gRPC client
  skills/              Skill registry + file watcher
  commands/            Command registry
  middleware/          Harness middleware chain
proto/               gRPC contract (ash.proto)
docker/              Dockerfile, supervisord, entrypoint
skills/              User-provided SKILL.md files (hot-reloaded)
commands/            User-provided command definitions (TOML)
samples/             Example skills, commands, middleware
scripts/             E2E smoke test, performance smoke
docs/                Architecture docs, task reports
```

</details>

---

## Session Persistence

세션은 PostgreSQL에 저장됩니다 (기본값). 컨테이너를 재시작해도 대화가 유지됩니다.

<details>
<summary><b>설정 옵션</b></summary>

**로컬 개발 (번들 PostgreSQL):**
```bash
docker compose --profile local-db up -d
```

**외부 관리형 DB (RDS, Supabase 등):**
```bash
ASH_POSTGRES_URL=postgres://user:pass@host:5432/dbname docker compose up -d ash-code
```

**인메모리 (테스트/데모용):**
```bash
ASH_SESSION_STORE=memory docker compose up -d ash-code
```

자세한 내용은 [docs/persistence.md](docs/persistence.md) 참조.

</details>

---

## Testing

```bash
# Rust unit tests
docker build --target rust-builder -f docker/Dockerfile -t ash-test .
docker run --rm ash-test cargo test --release --workspace

# Python tests
cd ashpy && uv run pytest -q

# E2E smoke test (requires running stack)
./scripts/e2e-smoke.sh

# Performance smoke (requires running stack)
python3 scripts/perf-smoke.py --turns 50
```

---

## Built-in Tools

| Tool | Description |
|---|---|
| `bash` | Shell command execution (HITL approval in TUI) |
| `file_read` | Read file contents |
| `file_write` | Write/create files |
| `file_edit` | Partial file editing |
| `grep` | Search file contents (ripgrep) |
| `glob` | Find files by pattern |

---

## Samples

`samples/` 디렉토리에 복사-붙여넣기 가능한 예시가 포함되어 있습니다:

| Sample | Type | Description |
|---|---|---|
| `skills/explain-error/` | Skill | 에러 분석 + 수정 제안 |
| `commands/healthcheck.toml` | Command | 서비스 상태 점검 |
| `commands/git-summary.toml` | Command | Git 활동 요약 |
| `middleware/token_budget_middleware.py` | Middleware | 세션별 토큰 예산 제한 |

---

## Documentation

| Document | Description |
|---|---|
| [docs/api_guide.md](docs/api_guide.md) | HTTP API 사용 가이드 (curl 예시 포함) |
| [docs/extensibility.md](docs/extensibility.md) | 확장성 설계 (Skills, Commands, Middleware) |
| [docs/persistence.md](docs/persistence.md) | 세션 저장소 설정 + 백업/복구 |
| [docs/skills.md](docs/skills.md) | Skill 시스템 상세 |
| [docs/commands.md](docs/commands.md) | Command 시스템 상세 |
| [docs/tui.md](docs/tui.md) | TUI 설계 |

---

## License

MIT
