# Remaining Tasks

ash-code 현재 상태와 남은 작업을 정리. 작성 시점: M9.1까지 완료 직후.

## 전체 마일스톤 진행 상황

| M | 범위 | 상태 |
|---|---|---|
| M0 | scaffold + 확장성 설계 | ✅ |
| M0.5 | extensibility design 문서 | ✅ |
| M1 | gRPC IPC (Rust ↔ Python) | ✅ |
| M2 | LLM providers (anthropic / openai / vllm / ollama) | ✅ |
| M3 | turn loop + 6 빌트인 도구 + Harness 미들웨어 | ✅ |
| M4 | FastAPI + Swagger + QueryHost gRPC | ✅ |
| M5 | Skills (`SKILL.md`, hot-reload) | ✅ |
| M6 | Commands (`*.toml`, render/run SSE) | ✅ |
| M7 | TUI (ratatui) + HITL bash 승인 + Anthropic tool_use | ✅ |
| M8 | Event bus + mid-turn cancel + OnStreamDelta opt-in | ✅ |
| **M9.1** | **PostgreSQL 세션 영속화** | **✅ (방금 완료)** |
| M9.2 | Cancel HTTP/gRPC | ⏳ 다음 |
| M9.3 | `WatchSession` SSE | ⏳ |
| M9.4 | CORS 외부화 + bearer token + `docs/security.md` | ⏳ |
| M9.5 | GitHub Actions CI | ⏳ |
| M10 | E2E 통합 + 최종 정리 | ⏳ |

`features.{health, llm, skills, commands, harness}` = `v1`,
`features.tools` = `planned` (M3+ 백로그).

---

## M9 — 운영화 (남은 4개 sub-step)

### M9.2 — Cancel HTTP / gRPC

**현재 상태**: M8에서 `CancellationToken`을 도입했지만 TUI에서만 사용. HTTP/gRPC 클라이언트는 turn을 중간에 멈출 방법이 없음.

**구현**:
- `proto/ash.proto`에 `QueryHost.CancelTurn(CancelTurnRequest) returns (CancelTurnResponse)` 추가
- `QueryHostService`가 `Arc<RwLock<HashMap<String, CancellationToken>>>` 보관 (active session → token)
- `RunTurn` 시작 시 token 등록, 종료/cancel 시 제거
- 같은 세션에 동시 RunTurn 들어오면 **자동으로 이전 token cancel** (M9 브리핑 Q2=a 결정)
- FastAPI: `POST /v1/sessions/{id}/cancel` → gRPC `CancelTurn` 호출 → 200 OK
- HTTP SSE 클라이언트가 연결을 끊으면 자동으로 cancel (axum disconnect signal)

**테스트**:
- gRPC `CancelTurn` 호출 → 진행 중 turn에 token 전파 → SSE에 `cancelled` outcome
- HTTP `POST /v1/sessions/{id}/cancel` → 200 + 같은 결과
- HTTP SSE 끊김 자동 감지 (어려우면 deferred)
- 동시 RunTurn 시 첫 번째 자동 cancel

**예상 작업량**: 작음 (proto 한 RPC, 핸들러 ~50 LoC, 테스트 3~4개)

---

### M9.3 — `WatchSession` SSE

**현재 상태**: M8 `SessionBus`는 publish-only. 구독자 코드 없음.

**구현**:
- `proto/ash.proto`에 `QueryHost.WatchSession(WatchSessionRequest) returns (stream BusEventProto)` 추가
- `BusEvent` Rust enum → protobuf `BusEventProto` 변환 헬퍼 (`crates/api`)
- `SessionBus.subscribe(session_id)` → broadcast::Receiver → tonic Stream으로 변환
- FastAPI: `GET /v1/sessions/{id}/watch` → gRPC stream 열고 SSE로 변환
- 멀티 구독자 지원 (broadcast 특성)

**사용 시나리오**:
```bash
# 터미널 1: TUI에서 채팅
docker exec -it ash-code ash tui
# 터미널 2: 같은 세션 관찰
curl -N http://localhost:8080/v1/sessions/<id>/watch
```

**테스트**:
- gRPC: 두 subscriber가 같은 세션의 같은 이벤트 받음
- HTTP SSE: 같은 시나리오, 한 클라이언트 끊겨도 다른 클라이언트 계속 받음
- TUI에서 발생한 이벤트가 외부 watch SSE에 도달

**예상 작업량**: 중간 (proto, BusEvent ↔ proto 변환, FastAPI 라우터, 통합 테스트)

---

### M9.4 — 보안 정책 + `docs/security.md`

**현재 상태**:
- CORS `allow_origins=["*"]`
- 인증 없음
- API 키 평문 env (acceptable for dev tool)

**M9.4에서 할 것**:
- **CORS 정책 외부화**: env `ASH_CORS_ORIGINS` (`*` 또는 콤마 구분 도메인 리스트)
- **HTTP API에 optional bearer token**: env `ASH_API_TOKEN` 설정 시 모든 `/v1/*` 요청이 `Authorization: Bearer <token>` 헤더 요구. 미설정이면 무인증 (default).
- **시작 시 stderr 경고**: `ASH_API_TOKEN` 미설정 + `ASH_CORS_ORIGINS=*`이면 큰 경고 (Q3=c 결정)
- **`docs/security.md` 작성**:
  - threat model
  - default deployment posture (localhost dev tool)
  - reverse proxy 권장 위치
  - secret 관리
  - sandbox 한계 (컨테이너 내 도구 실행)
  - bash_guard 정책 위치
  - M3+ tool 플러그인 보안 고려사항
  - PostgreSQL 자격증명 관리 (M9.1 후속)

**M9.4에서 안 할 것** (deferred):
- Tool 실행 sandboxing (별도 컨테이너 / seccomp / firejail) — 과대 작업
- TLS termination — reverse proxy 책임
- Audit logging persistence — `logging_middleware`가 stderr만, M10으로 미룸
- Multi-tenant scoping — single-developer harness 전제

**테스트**:
- CORS preflight 요청 처리
- bearer token 활성화/비활성화 양쪽
- 잘못된 토큰 → 401
- 시작 경고 메시지 출력 검증

**예상 작업량**: 중간 (axum/FastAPI 미들웨어 + 문서)

---

### M9.5 — GitHub Actions CI

**현재 상태**: CI 없음. 모든 검증 수동.

**구현**:
- `.github/workflows/ci.yml`:
  - **Rust 잡**: `rust:1.88-slim-bookworm` 컨테이너, `cargo test --workspace`, `cargo clippy -- -D warnings`, `cargo fmt --check`
    - PostgreSQL service container 띄우고 `ASH_TEST_POSTGRES_URL` 주입 → M9.1 통합 테스트도 실제 검증
  - **Python 잡**: `python:3.12-slim-bookworm` + `uv sync --extra dev` + `uv run pytest -q`
  - **Docker 빌드 잡**: `docker compose build ash-code`
  - **OpenAPI 스냅샷**: 컨테이너 부팅 → `curl /openapi.json` → 커밋된 스냅샷과 diff
- `Cargo.lock` 커밋 (현재 gitignore — M9.5에서 제거)
- `Swatinem/rust-cache` action으로 빌드 캐시
- CI 첫 빌드 ~5분, 캐시 후 ~2분 예상

**M9.5에서 안 할 것**:
- 매트릭스 빌드 (Rust 1.88 + Python 3.12 단일)
- 릴리즈 자동화
- Docker registry push

**예상 작업량**: 중간 (yaml + 캐시 튜닝 + 첫 PR에서 디버깅)

---

## M10 — E2E 통합 + 최종 정리

내용은 M9 종료 후 별도 브리핑에서 확정. 후보:

- **풀 스택 E2E 시나리오 스크립트**: compose up → skill 등록 → command 실행 → cancel → watch → DB 조회 한 번에 검증하는 bash 또는 pytest
- **README quickstart 재작성**: 30초 만에 첫 채팅까지 가는 경로
- **`samples/` 디렉토리**: 실전 스킬/커맨드/미들웨어 예시 모음
- **Performance smoke**: 100 turns 연속 실행 후 메모리/DB 사이즈 측정
- **Documentation index**: `docs/` 안의 모든 파일을 카테고리별로 정리한 진입 페이지

---

## M3 deferred 항목 (백로그, 다음 마일스톤 미할당)

이 항목들은 어떤 마일스톤에도 명시적으로 잡혀있지 않고, 필요 시 채워넣는 백로그.

### OpenAI / vLLM / Ollama tool_use 매핑
- M7 사후 패치에서 **Anthropic만** tool_use 매핑 완성
- OpenAI의 `tool_calls` array, Ollama의 모델별 tool 포맷, vLLM의 OpenAI 호환 tool은 여전히 텍스트 스트리밍만 동작
- 영향: 이 provider들로 TUI HITL 트리거 안 됨 (모델이 도구 호출을 못 함)
- 작업량: 중간 — provider별 raw event stream 처리 + tool_use → ChatDelta 변환

### `ToolRegistry` 제3자 Python 플러그인
- proto에 `ToolRegistry` 서비스 정의됨, gRPC stub만 있고 `UNIMPLEMENTED` 반환
- 사용자가 `tools/<name>.py`에 Python tool 클래스 드롭하면 turn 루프가 호출
- 현재는 Rust 빌트인 6개만 동작
- 작업량: 중간 — Python 플러그인 로더 + Rust↔Python tool 호출 경로

### Skill / Command의 `allowed_tools` 강제
- M5/M6에서 `allowed_tools` 필드를 응답에 surface만 함
- turn 루프가 실제로 화이트리스트 강제하지 않음 — 모델이 선언 외 도구 호출 가능
- 강제하려면 turn 루프에 세션 메타데이터 전달 경로 필요 (M8 SessionBus 위에 올릴 수 있음)
- 작업량: 작음~중간

### `OnStreamDelta` 배치 호출
- M8에서 opt-in으로 활성화 (`ASH_HARNESS_STREAM_DELTA=on`)
- 활성화 시 매 토큰마다 fire-and-forget gRPC 호출. 배치 안 함.
- 미래에 실제 소비자(PII redaction 등)가 등장하면 `ASH_HARNESS_STREAM_DELTA_BATCH=128` 같은 knob 추가 가능
- 현재 우선순위 낮음

### `crates/buddy` / `crates/plugins` / `crates/acp` 등
- claurst 레퍼런스 구조에 있던 추가 crate들 — ash-code는 사용 안 함
- 워크스페이스에 등록 안 됨, 빈 디렉토리 없음
- 미래에 plugin marketplace 같은 게 필요해지면 개별 검토

---

## 마일스톤 외 운영 개선 후보

### 마이그레이션 프레임워크
- 현재 `ensure_schema`는 `CREATE … IF NOT EXISTS`. 두 번째 마이그레이션이 생기는 시점에 `refinery` / `sqlx-migrate` 도입 필요
- M9.1 시점에는 단일 테이블이라 불필요
- 트리거: 메시지 정규화, multi-tenant column 추가 등

### Multi-replica 동시성
- 현재 ash-code는 단일 컨테이너 전제
- Horizontal scaling 시:
  - 같은 세션에 두 replica가 동시 write → row-level lock 또는 optimistic concurrency
  - `SessionBus`는 in-process라 replica 간 이벤트 공유 안 됨 → Redis Pub/Sub 또는 PostgreSQL `LISTEN/NOTIFY` 필요
- M9 범위 밖. 운영 요구가 명확해지면 별도 마일스톤.

### Token 사용량 / 비용 추적
- 현재 `TurnFinish.input_tokens`/`output_tokens`만 응답에 포함
- 누적 비용, provider별 집계, 일별/월별 리포트 없음
- 미들웨어로 구현 가능 (`OnTurnEnd` 훅 + DB 또는 file)
- 미래에 대시보드 만들면 그때 정식화

### 로그 영속화
- `logging_middleware`가 stderr에 JSON만 출력
- 영속 audit log는 M9.4 deferred 항목

### 백업/복구 자동화
- `docs/persistence.md`에 `pg_dump` / `pg_restore` 절차 문서화 완료
- 자동 백업 cron, 복구 시나리오 테스트는 운영 단계에서 추가

---

## 알려진 제약 (변경 의도 없음)

다음 항목들은 "버그 아님, 의도된 설계":

- **TUI는 docker 컨테이너 내부에서만 작동** — `docker exec -it ash-code ash tui`. 호스트에서 직접 `ash` 실행은 지원 안 함 (Rust 바이너리 + Python sidecar 둘 다 필요).
- **macOS / Windows Docker Desktop의 inotify 한계** — 볼륨 마운트된 `skills/` `commands/` 변경이 자동 감지 안 됨. `ASH_SKILLS_POLLING=1` 설정 필수. Linux 호스트는 native inotify 동작.
- **TUI의 mid-turn cancel은 Esc 한 번** — 연속 cancel 시 첫 번째만 유효 (이미 끝난 turn에 cancel 보내도 무해)
- **Turn loop max_turns 기본 10** — `ASH_MAX_TURNS` env로 오버라이드. 무한 tool-use loop 방지용.
- **세션 ID 충돌 시 덮어쓰기** — 같은 ID 두 번 만들면 두 번째가 첫 번째 대화 위에 append. 의도된 동작 (continuation 패턴).
- **HITL 승인은 `bash`에만** — 다른 도구는 자동 승인. `crates/tui/src/backend.rs::requires_approval` 한 줄로 확장 가능.

---

## 우선순위 (제안)

1. **M9.2 Cancel** — 작업량 작고 사용자 직접 체감 가능 (HTTP에서 긴 응답 끊기)
2. **M9.4 보안** — production 배포 전 필수
3. **M9.5 CI** — 회귀 방지, 다음 작업 안전성 보장
4. **M9.3 Watch** — 외부 모니터링 수요가 명확해질 때
5. **M10** — 위 4개 끝난 후

OpenAI tool_use 매핑은 사용자가 OpenAI를 주력으로 쓰기 시작하는 시점에 우선순위 상향.
