# ash-code HTTP API 사용 가이드

ash-code는 Swagger UI(`http://localhost:8080/docs`)를 통해 모든 API를 브라우저에서 직접 테스트할 수 있습니다. 이 문서는 실제 사용 흐름을 중심으로 각 API의 사용법을 설명합니다.

---

## 시작하기

```bash
# 스택 실행
docker compose --profile local-db up -d

# Swagger UI 열기
open http://localhost:8080/docs

# 또는 curl로 직접 호출
curl -s http://localhost:8080/v1/health | python3 -m json.tool
```

---

## 1. Health — 서비스 상태 확인

서비스가 정상 동작하는지 확인합니다. 가장 먼저 호출해볼 엔드포인트.

### `GET /v1/health`

```bash
curl -s http://localhost:8080/v1/health
```

```json
{
  "status": "ok",
  "ashpy_version": "0.0.1",
  "api_version": "v1",
  "features": {
    "http": "v1",
    "llm": "v1",
    "harness": "v1",
    "skills": "v1",
    "commands": "v1"
  }
}
```

---

## 2. Chat — LLM과 대화하기

가장 핵심적인 엔드포인트. LLM에게 메시지를 보내면 응답을 **SSE(Server-Sent Events) 스트림**으로 받습니다.

### `POST /v1/chat`

**Request Body:**

| 필드 | 타입 | 필수 | 설명 |
|---|---|---|---|
| `prompt` | string | **O** | 사용자 메시지 |
| `session_id` | string | - | 기존 세션에 이어서 대화. 미지정 시 자동 생성 |
| `provider` | string | - | 프로바이더 오버라이드 (`anthropic`, `openai` 등) |
| `model` | string | - | 모델 오버라이드 |
| `reset_session` | bool | - | `true`이면 기존 대화 내역을 지우고 새로 시작 |

**기본 사용:**

```bash
curl -s -N -X POST http://localhost:8080/v1/chat \
  -H "Content-Type: application/json" \
  -d '{"prompt": "안녕하세요!"}'
```

**세션 지정:**

```bash
curl -s -N -X POST http://localhost:8080/v1/chat \
  -H "Content-Type: application/json" \
  -d '{
    "session_id": "my-session",
    "prompt": "이전 대화를 이어서 해주세요"
  }'
```

**모델 오버라이드:**

```bash
curl -s -N -X POST http://localhost:8080/v1/chat \
  -H "Content-Type: application/json" \
  -d '{
    "prompt": "간단한 질문",
    "provider": "anthropic",
    "model": "claude-sonnet-4-20250514"
  }'
```

**SSE 응답 이벤트 종류:**

| event | 설명 | 주요 필드 |
|---|---|---|
| `text` | 어시스턴트 텍스트 (토큰 단위 스트리밍) | `text` |
| `tool_call` | LLM이 도구 호출 요청 | `name`, `arguments` |
| `tool_result` | 도구 실행 결과 | `name`, `ok`, `stdout`, `stderr` |
| `finish` | 턴 종료 | `stop_reason`, `input_tokens`, `output_tokens` |
| `outcome` | 최종 결과 (스트림 마지막) | `stop_reason`, `turns_taken`, `denied` |
| `error` | 오류 발생 | `message` |
| `done` | 스트림 종료 마커 | `[DONE]` |

**응답 예시:**

```
event: text
data: {"type": "text", "text": "안녕"}

event: text
data: {"type": "text", "text": "하세요!"}

event: finish
data: {"type": "finish", "stop_reason": "end_turn", "input_tokens": 42, "output_tokens": 5}

event: outcome
data: {"type": "outcome", "stop_reason": "end_turn", "turns_taken": 1, "denied": false, "denial_reason": ""}

event: done
data: [DONE]
```

> **참고:** `text` 이벤트는 토큰 단위로 쪼개져 올 수 있습니다. 클라이언트에서 누적하여 전체 응답을 구성하세요.

---

## 3. Sessions — 세션 관리

세션은 대화의 단위입니다. 한 세션 안에서 여러 번 `/v1/chat`을 호출하면 대화가 이어집니다.

### `GET /v1/sessions` — 세션 목록

```bash
curl -s http://localhost:8080/v1/sessions | python3 -m json.tool
```

```json
{
  "sessions": [
    {
      "id": "my-session",
      "provider": "anthropic",
      "model": "claude-opus-4-5",
      "message_count": 4
    }
  ]
}
```

### `GET /v1/sessions/{session_id}` — 세션 상세 (메시지 포함)

```bash
curl -s http://localhost:8080/v1/sessions/my-session | python3 -m json.tool
```

```json
{
  "summary": {
    "id": "my-session",
    "provider": "anthropic",
    "model": "claude-opus-4-5",
    "message_count": 4
  },
  "messages": [
    {"role": "user", "content": "안녕하세요!", "tool_call_id": ""},
    {"role": "assistant", "content": "안녕하세요! 무엇을 도와드릴까요?", "tool_call_id": ""}
  ]
}
```

### `DELETE /v1/sessions/{session_id}` — 세션 삭제

```bash
curl -s -X DELETE http://localhost:8080/v1/sessions/my-session
```

```json
{"ok": true}
```

> 존재하지 않는 세션을 삭제하면 404가 반환됩니다.

### `POST /v1/sessions/{session_id}/cancel` — 진행 중인 턴 취소

긴 응답을 중간에 멈추고 싶을 때 사용합니다.

```bash
# 터미널 1: 긴 작업 시작
curl -s -N -X POST http://localhost:8080/v1/chat \
  -d '{"session_id": "long-task", "prompt": "1000줄짜리 코드를 작성해줘"}'

# 터미널 2: 중간에 취소
curl -s -X POST http://localhost:8080/v1/sessions/long-task/cancel
```

```json
{"ok": true, "message": "cancelled"}
```

| 응답 | 의미 |
|---|---|
| `ok: true` | 진행 중인 턴을 취소했음 |
| `ok: false` | 진행 중인 턴이 없음 (이미 끝났거나 시작 전) |

> 두 경우 모두 HTTP 200입니다. `ok` 필드로 구분하세요.

### `GET /v1/sessions/{session_id}/watch` — 실시간 이벤트 관찰 (SSE)

다른 클라이언트가 진행 중인 세션의 이벤트를 실시간으로 관찰합니다.

```bash
# 터미널 1: 대화 시작
curl -s -N -X POST http://localhost:8080/v1/chat \
  -d '{"session_id": "demo", "prompt": "hello"}'

# 터미널 2: 같은 세션 관찰
curl -N http://localhost:8080/v1/sessions/demo/watch
```

```
event: assistant_text
data: {"event_type": "assistant_text", "session_id": "demo", "payload": {"text": "Hello!"}}

event: outcome
data: {"event_type": "outcome", "session_id": "demo", "payload": {"stop_reason": "end_turn", "turns_taken": 1, "denied": false}}
```

**이벤트 종류:**

| event_type | 설명 |
|---|---|
| `user_message` | 사용자 메시지 |
| `assistant_text` | 어시스턴트 응답 텍스트 |
| `tool_call` | 도구 호출 |
| `tool_result` | 도구 실행 결과 |
| `turn_finish` | 턴 종료 (토큰 정보 포함) |
| `turn_error` | 턴 중 에러 |
| `cancelled` | 취소됨 |
| `outcome` | 최종 결과 |

> **주의:** watch는 구독 이후 발생하는 이벤트만 받습니다. 과거 이벤트는 재생되지 않습니다. 과거 메시지가 필요하면 `GET /v1/sessions/{id}`를 사용하세요.

---

## 4. Skills — 스킬 관리

### `GET /v1/skills` — 스킬 목록

```bash
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
      "model": "claude-opus-4-5",
      "source_path": "/root/.ash/skills/review-diff/SKILL.md"
    }
  ]
}
```

### `GET /v1/skills/{name}` — 스킬 상세 (body 포함)

```bash
curl -s http://localhost:8080/v1/skills/review-diff | python3 -m json.tool
```

응답에 `body` 필드가 추가됩니다 (Jinja2 템플릿 원문).

### `POST /v1/skills/{name}/invoke` — 스킬 프롬프트 렌더링

스킬을 실행하지 않고, 인자를 대입하여 렌더링된 프롬프트만 받습니다.

```bash
curl -s -X POST http://localhost:8080/v1/skills/review-diff/invoke \
  -H "Content-Type: application/json" \
  -d '{"args": {"focus": "security vulnerabilities"}}'
```

```json
{
  "rendered_prompt": "You are reviewing a staged git diff.\n\nSteps:\n1. Run `git diff --staged`...\n3. Focus on: security vulnerabilities\n...",
  "allowed_tools": ["bash", "file_read", "grep"],
  "model": "claude-opus-4-5"
}
```

**렌더링 → 실행 패턴:**

invoke로 받은 `rendered_prompt`를 `/v1/chat`의 `prompt`로 전달하면 LLM이 스킬대로 작업합니다:

```bash
# 1단계: 스킬 렌더링
PROMPT=$(curl -s -X POST http://localhost:8080/v1/skills/review-diff/invoke \
  -d '{"args": {"focus": "security"}}' | python3 -c "import sys,json; print(json.load(sys.stdin)['rendered_prompt'])")

# 2단계: 렌더링된 프롬프트로 채팅
curl -s -N -X POST http://localhost:8080/v1/chat \
  -H "Content-Type: application/json" \
  -d "{\"prompt\": $(python3 -c "import json; print(json.dumps('$PROMPT'))")}"
```

### `POST /v1/skills/reload` — 스킬 리로드

`skills/` 디렉토리에 파일을 추가/수정한 후 수동으로 리로드합니다.

```bash
curl -s -X POST http://localhost:8080/v1/skills/reload
```

```json
{"loaded": 3, "errors": []}
```

> Linux에서는 inotify로 자동 반영되므로 수동 리로드가 불필요합니다. macOS/Windows에서 `ASH_SKILLS_POLLING=1` 설정 시에도 자동 반영됩니다.

---

## 5. Commands — 커맨드 관리

### `GET /v1/commands` — 커맨드 목록

```bash
curl -s http://localhost:8080/v1/commands | python3 -m json.tool
```

### `GET /v1/commands/{name}` — 커맨드 상세

```bash
curl -s http://localhost:8080/v1/commands/test | python3 -m json.tool
```

### `POST /v1/commands/{name}/render` — 프롬프트 렌더링만

커맨드를 실행하지 않고 렌더링된 프롬프트만 확인합니다.

```bash
curl -s -X POST http://localhost:8080/v1/commands/test/render \
  -H "Content-Type: application/json" \
  -d '{"args": {"target": "unit tests only"}}'
```

```json
{
  "rendered_prompt": "Run the project's test suite and analyze the result.\n\n1. Detect the stack...\n2. Run the appropriate test command. Target: unit tests only.\n...",
  "allowed_tools": ["bash", "file_read"],
  "model": null
}
```

### `POST /v1/commands/{name}/run` — 커맨드 실행 (SSE)

렌더링 + LLM 실행까지 한 번에 수행합니다. 응답은 SSE 스트림.

```bash
curl -s -N -X POST http://localhost:8080/v1/commands/test/run \
  -H "Content-Type: application/json" \
  -d '{"args": {"target": "all"}}'
```

**Request Body:**

| 필드 | 타입 | 필수 | 설명 |
|---|---|---|---|
| `args` | object | - | 템플릿 인자 (Jinja2 변수) |
| `context` | object | - | 컨텍스트 변수 |
| `session_id` | string | - | 세션 ID (미지정 시 `cmd-{name}-{timestamp}`) |
| `provider` | string | - | 프로바이더 오버라이드 |
| `model` | string | - | 모델 오버라이드 |
| `reset_session` | bool | - | 세션 초기화 |

> **모델 우선순위:** 커맨드에 정의된 `model` > 요청의 `model` > 세션 기본값

```
event: text
data: {"type": "text", "text": "Looking at the repo root..."}

event: tool_call
data: {"type": "tool_call", "name": "bash", "arguments": "{\"command\": \"ls\"}"}

event: tool_result
data: {"type": "tool_result", "name": "bash", "ok": true, "stdout": "Cargo.toml\nashpy\n..."}

event: outcome
data: {"type": "outcome", "stop_reason": "end_turn", "turns_taken": 1}

event: done
data: [DONE]
```

### `POST /v1/commands/reload` — 커맨드 리로드

```bash
curl -s -X POST http://localhost:8080/v1/commands/reload
```

```json
{"loaded": 3, "errors": []}
```

---

## 6. LLM Providers — 프로바이더 관리

### `GET /v1/llm/providers` — 프로바이더 목록

```bash
curl -s http://localhost:8080/v1/llm/providers | python3 -m json.tool
```

```json
{
  "providers": [
    {
      "name": "anthropic",
      "default_model": "claude-opus-4-5",
      "supports_tools": true,
      "supports_vision": true,
      "source": "builtin"
    }
  ]
}
```

### `POST /v1/llm/switch` — 프로바이더 전환

```bash
curl -s -X POST http://localhost:8080/v1/llm/switch \
  -H "Content-Type: application/json" \
  -d '{"provider": "openai", "model": "gpt-4o"}'
```

```json
{"ok": true, "message": "switched to openai (gpt-4o)"}
```

---

## 실전 워크플로우 예시

### 예시 1: 코드 리뷰

```bash
# 1. review 커맨드로 보안 관점 리뷰
curl -s -N -X POST http://localhost:8080/v1/commands/review/run \
  -H "Content-Type: application/json" \
  -d '{"args": {"focus": "security vulnerabilities"}}'
```

### 예시 2: 파일 분석 → 후속 질문

```bash
# 1. 파일 요약
curl -s -N -X POST http://localhost:8080/v1/chat \
  -H "Content-Type: application/json" \
  -d '{"session_id": "analyze", "prompt": "Cargo.toml 파일을 분석해줘"}'

# 2. 같은 세션에서 후속 질문
curl -s -N -X POST http://localhost:8080/v1/chat \
  -H "Content-Type: application/json" \
  -d '{"session_id": "analyze", "prompt": "거기서 사용하지 않는 의존성을 찾아줘"}'

# 3. 대화 내역 확인
curl -s http://localhost:8080/v1/sessions/analyze | python3 -m json.tool
```

### 예시 3: 긴 작업 + 취소 + 관찰

```bash
# 터미널 1: 긴 작업 시작
curl -s -N -X POST http://localhost:8080/v1/chat \
  -d '{"session_id": "long", "prompt": "전체 프로젝트를 리팩토링해줘"}'

# 터미널 2: 실시간 관찰
curl -N http://localhost:8080/v1/sessions/long/watch

# 터미널 3: 필요하면 취소
curl -s -X POST http://localhost:8080/v1/sessions/long/cancel
```

---

## Swagger UI 사용 팁

1. **Try it out** 버튼을 누르면 각 엔드포인트를 직접 테스트할 수 있습니다.
2. SSE 응답(`/v1/chat`, `/v1/commands/{name}/run`, `/v1/sessions/{id}/watch`)은 Swagger UI에서 스트리밍이 제대로 표시되지 않을 수 있습니다. 이런 경우 `curl -N`으로 테스트하세요.
3. Schemas 섹션에서 모든 request/response 모델의 전체 필드 명세를 확인할 수 있습니다.
4. OpenAPI 스펙 JSON은 `http://localhost:8080/openapi.json`에서 직접 다운로드 가능합니다.
