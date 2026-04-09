# ash-code 개념 정리 — Rust가 뭐 하고 Python이 뭐 하는지

> 이 문서는 Rust를 모르는 개발자를 대상으로 ash-code의 두 언어 경계를
> 실생활 비유 수준으로 풀어 씁니다. 코드 예시는 참고용이고, 핵심은
> **"왜 이렇게 나눴는가"** 와 **"각 언어가 담당하는 것이 무엇인가"** 입니다.

---

## 한 줄 요약

- **Rust** = **엔진룸**. 빠르고, 안정적이고, 한번 만들면 건드릴 일 적은 뼈대.
- **Python** = **조종석**. 유연하고, 자주 수정되고, 사용자/개발자가 실제로
  만지는 표면.

두 언어는 같은 컨테이너 안에서 **별도 프로세스**로 돌아가고,
**gRPC**라는 타입 안전한 전화선으로 대화합니다. 어느 한쪽을 꺼도 다른 쪽은
그 사실을 안전하게 인지하고 멈춥니다.

---

## 전체 그림 (한 장)

```
┌──────────────────────── ash-code container ────────────────────────────┐
│                                                                         │
│  ┌─────────────── Rust 프로세스들 ──────────────┐                        │
│  │                                               │                      │
│  │  [ash TUI]         (M7 예정 — 사용자가       │                      │
│  │                      터미널에서 직접 본다)    │                      │
│  │                                               │                      │
│  │  [ash CLI]         (ash doctor, ash llm chat  │                      │
│  │                     같은 한방 명령)           │                      │
│  │                                               │                      │
│  │  [ash serve]       (QueryHost gRPC 서버,      │◄─┐                   │
│  │                     Python FastAPI가 호출)    │  │ gRPC :50052       │
│  │                                               │  │                   │
│  │  내부 라이브러리:                              │  │                   │
│  │    crates/query    turn 루프 (M3)             │  │                   │
│  │    crates/tools    bash/file/grep/glob (M3)   │  │                   │
│  │    crates/ipc      Python 사이드카 gRPC 클라  │  │                   │
│  │                                               │  │                   │
│  └────────────────────────┬──────────────────────┘  │                   │
│                           │                         │                   │
│                  gRPC :50051 (안쪽 방향)              │                   │
│                           │                         │                   │
│  ┌────────────────────────▼──────────────────────┐  │                   │
│  │ Python 프로세스 (ashpy serve)                 │  │                   │
│  │ = gRPC 서버 + FastAPI 웹 서버, 같은 이벤트 루프  │──┘                   │
│  │                                               │                      │
│  │  FastAPI HTTP :8080 ──► /v1/chat, /docs, ...  │◄── 호스트 브라우저    │
│  │                                               │                      │
│  │  gRPC 서비스들 (Rust가 호출):                   │                      │
│  │    LlmProvider     (M2) — 4종 LLM 어댑터       │                      │
│  │    Harness         (M3) — 미들웨어 훅          │                      │
│  │    SkillRegistry   (M5 예정)                  │                      │
│  │    CommandRegistry (M6 예정)                  │                      │
│  └───────────────────────────────────────────────┘                      │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

- 바깥 세계(호스트 브라우저, `curl`, 외부 스크립트)에 노출되는 포트는
  **단 하나** — `8080` (Python FastAPI).
- `:50051`, `:50052`는 컨테이너 안에서만 쓰는 내부 전화선.

---

## Rust가 맡는 부분 — "엔진룸"

Rust는 **자주 바뀌지 않고, 빠르게 돌아야 하고, 안전하게 검증되어야 하는**
코드를 담당합니다.

### 1. Turn 루프 (`crates/query`)
사용자가 "파일 X를 읽어서 Y를 고쳐줘"라고 말하면, 실제로는 아래 과정이
여러 번 반복됩니다:

1. LLM에게 현재까지의 대화를 보냄
2. LLM이 "file_read를 이 경로로 실행해줘"라고 답함
3. 우리가 file_read를 진짜 실행
4. 결과를 LLM에게 다시 보냄
5. LLM이 다시 요청하거나, 최종 답을 줌

이 **루프 엔진**이 `crates/query`에 있습니다. 왜 Rust에 넣었느냐:
- 여러 도구가 병렬로 돌아가야 함 → Rust의 `tokio` 비동기 런타임이
  최적
- 턴 수 제한(10회), 중간 취소, 타임아웃 같은 불변 조건을 컴파일 타임에
  검증할 수 있음
- 사용자가 자주 손대는 영역이 아님 (턴 루프 로직 자체는 한 번 만들면
  거의 안정적)

### 2. 빌트인 도구들 (`crates/tools`)
`bash`, `file_read`, `file_write`, `file_edit`, `grep`, `glob` — 6개.
이걸 Rust로 만든 이유:
- 파일 시스템과 프로세스를 직접 건드리는 영역이라 **성능**이 실제로
  차이가 남. 큰 디렉토리 `grep`은 Python보다 몇 배 빠릅니다.
- `rm -rf /` 같은 파괴적 명령을 막는 **최후의 안전망**을 여기에
  심었습니다. Rust는 실행을 거절하는 게 확실합니다.
- 이 6개는 거의 안 바뀝니다. 사용자가 추가하는 도구는 M3+에서
  "플러그인" 경로로 Python에서 처리할 예정.

### 3. TUI (`crates/tui`, M7 예정)
터미널 UI (채팅창, 입력창, 진행 표시줄). Rust의 `ratatui`라는 라이브러리가
터미널을 픽셀 수준으로 제어합니다. 초당 수십 프레임을 부드럽게 그리려면
Python보다 Rust가 유리합니다.

### 4. IPC 클라이언트 (`crates/ipc`)
Python 사이드카(`:50051`)에 전화 거는 쪽. "LLM에게 이 프롬프트로
스트리밍 호출해줘", "이 도구 호출을 훅에 돌려봐" 같은 요청을 gRPC로
보냅니다.

### 5. `ash serve` = QueryHost gRPC 서버 (M4)
Rust가 여기서는 드물게 **gRPC 서버** 역할도 합니다. Python FastAPI가
`/v1/chat`에서 "턴 돌려줘"라고 호출하면, 이 서버가 받아서 `crates/query`
엔진에 위임합니다. 포트 `:50052`.

---

## Python이 맡는 부분 — "조종석"

Python은 **자주 바뀌고, 사용자가 직접 편집하고, 생태계가 풍부해야 하는**
부분을 담당합니다.

### 1. LLM Provider 어댑터 (`ashpy/providers/`, M2)
Anthropic, OpenAI, vLLM, Ollama — 4종. 왜 Python으로?
- 공식 SDK가 전부 Python이 가장 잘 되어 있음 (`anthropic`, `openai`,
  `ollama`).
- **새 provider 추가가 파일 하나 드롭으로 끝남**. `providers/my_new.py`에
  `LlmProvider` 클래스를 하나 만들어 넣으면 ash-code가 자동 발견해서
  씁니다. Rust였다면 재컴파일이 필요했을 겁니다.

### 2. 미들웨어 체인 (`ashpy/middleware/`, M3)
개발자가 "bash 도구 호출 중 `sudo` 들어간 건 막아줘", "모든 턴을
로그에 JSON으로 찍어줘" 같은 **정책**을 만드는 곳.
- 현재 내장 2종: `logging_middleware`(관측), `bash_guard_middleware`
  (위험 명령 차단).
- 사용자는 `/root/.ash/middleware/*.py`에 파이썬 파일만 드롭하면 됩니다.
  재시작 한 번으로 반영.
- Python의 동적 로딩(`importlib`), 느슨한 타입 체계가 "정책을 빠르게
  시도하고 버리기"에 잘 맞습니다.

### 3. 커스텀 Skills (`ashpy/skills/`, M5 예정)
`SKILL.md` 파일을 떨어뜨리면 에이전트가 새 능력을 얻는 기능. watchdog으로
파일 변경을 감지해서 **실시간 반영**.

### 4. 커스텀 Commands (`ashpy/commands/`, M6 예정)
`/review`, `/commit` 같은 슬래시 명령. TOML + Jinja2 템플릿으로
정의. 역시 파일 드롭으로 추가.

### 5. FastAPI HTTP 레이어 (`ashpy/api/`, M4)
호스트 브라우저/외부 스크립트가 ash-code를 부를 수 있게 여는 문.
- `/v1/chat` (SSE 스트리밍), `/v1/llm/providers`, `/v1/sessions`,
  `/docs` (Swagger UI)
- FastAPI의 최대 강점 — Pydantic 모델이 곧 자동 OpenAPI 스키마.
  엔드포인트 추가가 Python 파일 하나 추가로 끝.

---

## 왜 두 언어로 나눴는가

### 이유 1: 각 언어의 강점을 활용
| 영역 | Rust가 유리 | Python이 유리 |
|---|---|---|
| 성능 (파일 I/O, 정규식, 프로세스) | ✅ | ❌ |
| 터미널 UI 렌더링 | ✅ | ❌ (가능하지만 느림) |
| 타입 안정성 (불변 조건 강제) | ✅ | ❌ |
| LLM SDK 생태계 | ❌ | ✅ (공식 SDK 전부 Python) |
| HTTP + Swagger 자동생성 | ❌ (수동 어노테이션) | ✅ (FastAPI/Pydantic) |
| 플러그인 동적 로딩 | ❌ (재컴파일 필요) | ✅ (`importlib`) |
| 사용자가 편집하는 빈도 | 낮아야 함 | 높아야 함 |

### 이유 2: 책임 분리
> **Rust는 "엔진"을 만든다. Python은 "조종석과 정책"을 만든다.**

엔진을 자주 뜯어고치면 고장납니다. 반면 조종석은 운전자마다 다르게
배치하고 싶을 겁니다. ash-code는 **사용자가 자주 커스터마이징할
것으로 예상되는 것들을 모두 Python 쪽에 몰아넣어서**, Rust 쪽은 한번
만들어 두면 오래 가도록 설계했습니다.

### 이유 3: 재컴파일 없는 커스터마이징
Rust는 코드를 바꾸면 **바이너리를 다시 컴파일**해야 합니다. Docker
환경에서는 "이미지 rebuild → 컨테이너 재시작" 사이클이라 최소 몇십
초 걸립니다.

반면 Python은:
- Skill 파일 저장 → watchdog이 감지 → 즉시 반영 (재시작도 필요 없음)
- Middleware 파일 수정 → `supervisorctl restart ashpy` 1회 → 반영
- Provider 파일 추가 → 재시작 1회 → 반영

**사용자가 자주 건드리는 영역일수록 Python 쪽에 있어야** 이 속도
차이가 체감됩니다.

---

## 두 언어는 어떻게 대화하는가 — gRPC

두 프로세스는 메모리를 공유하지 않습니다. 대화 방법이 필요합니다.
ash-code는 **gRPC**를 씁니다. 왜?

- 언어 독립적: `.proto` 파일 하나만 정의하면 양쪽에서 똑같은 타입의
  코드가 자동 생성됨.
- 스트리밍 지원: LLM 토큰이 한 글자씩 흘러오는 걸 그대로 흘려보낼 수
  있음.
- 타입 안전: 필드 이름/타입이 맞지 않으면 컴파일 실패 또는 런타임
  즉시 에러 (무성 실패 없음).

### 두 방향의 gRPC

ash-code는 특이하게 **양방향** gRPC를 씁니다. 보통은 한 쪽이 서버,
한 쪽이 클라이언트인데, 여기서는 둘 다 서로에게 서버이자 클라이언트
입니다.

```
(1) Rust → Python 방향  (:50051)
    ── LlmProvider.ChatStream ─►  (LLM에게 스트리밍 호출)
    ── Harness.OnTurnStart   ─►  (턴 시작 훅)
    ── Harness.OnToolCall    ─►  (도구 호출 훅 — ALLOW/DENY)
    ── SkillRegistry.Invoke  ─►  (M5 예정)
    ── CommandRegistry.Run   ─►  (M6 예정)

(2) Python → Rust 방향  (:50052)
    ── QueryHost.RunTurn     ─►  (턴 루프를 돌려줘)
    ── QueryHost.ListSessions ─►  (세션 목록)
```

**왜 이렇게 양방향인가?**
- 방향 (1): "Python이 가진 것을 Rust가 써야 할 때" — LLM 호출, 정책 훅.
  Python은 이런 걸 관리하는 주체이므로 서버 역할.
- 방향 (2): "Rust가 가진 것을 Python이 써야 할 때" — M4에서 HTTP API를
  FastAPI에 줬기 때문에, FastAPI가 턴을 돌리려면 Rust의 `QueryEngine`에
  요청해야 함. 그래서 이 방향에서는 Rust가 서버.

혼동되면 이렇게 기억하세요:
> **"내가 가진 걸 남이 써야 할 때, 나는 서버가 된다."**

---

## 구체적 예시: `/v1/chat` 요청 하나가 어떻게 흘러가는가

사용자가 브라우저에서 `curl http://localhost:8080/v1/chat -d '{"prompt":"..."}'`
하면:

```
[1] 호스트 브라우저 → docker 포트 매핑 → 컨테이너 :8080
      (호스트에서 보이는 유일한 포트)

[2] Python uvicorn이 요청을 받음
      → FastAPI 라우터가 ChatRequest Pydantic 모델로 검증
      → /v1/chat 핸들러 실행

[3] 핸들러가 QueryHostClient.run_turn(...)를 호출
      → Python → Rust gRPC :50052

[4] Rust ash serve (QueryHostService)가 요청 받음
      → Session을 HashMap에서 꺼내고
      → QueryEngine.run_turn(session, sink) 호출
      → 턴 루프 시작

[5] Rust QueryEngine이 "LLM 호출 필요" 판단
      → SidecarBackend를 통해 Rust → Python gRPC :50051 호출
      → LlmProviderServicer.ChatStream 실행

[6] Python이 anthropic SDK로 진짜 API 호출
      → 토큰이 스트리밍으로 돌아옴
      → 각 토큰을 ChatDelta로 Rust에게 스트리밍 반환

[7] Rust QueryEngine이 토큰을 받으면서 동시에
      → Harness.OnTurnStart 훅 호출 (Python middleware chain)
      → 토큰이 오면 TurnSink에 넣고
      → Sink는 mpsc 채널로 TurnDelta를 QueryHost gRPC 응답 스트림에 보냄

[8] Python FastAPI 핸들러가 Rust gRPC 응답 스트림을 async for로 읽으며
      → 각 TurnDelta를 SSE 이벤트로 변환
      → uvicorn이 HTTP 응답 본문에 event-stream 형식으로 작성

[9] 호스트 브라우저가 SSE 이벤트를 받아 화면에 표시
```

**4개 프로세스 경계, 2개 gRPC 왕복.**

느려 보이지만 실제론 loopback 통신이라 각 홉이 1~3ms. 대부분의 시간은
[6] 단계의 Anthropic API 호출(100~1000ms)이 먹습니다. 아키텍처 오버헤드는
무시할 수준입니다.

---

## 각 마일스톤별 어느 쪽이 어디에 뭘 추가했나

| 마일스톤 | Rust 추가 | Python 추가 |
|---|---|---|
| M0 (scaffold) | 빈 crate 8개 + CLI 스켈레톤 | sidecar skeleton |
| M1 (gRPC) | tonic 배선 + `SidecarClient` | `grpc.aio` 서버 + Health.Ping 실구현 |
| M2 (providers) | `SidecarClient.chat_stream` | 4종 provider 어댑터 + 플러그인 로더 |
| M3 (turn loop) | `crates/query` turn 루프 + 6개 도구 | middleware chain + HarnessServicer |
| M4 (HTTP API) | `crates/api` QueryHost gRPC 서버 | FastAPI + Swagger + `/v1/*` 엔드포인트 |
| M5 (skills, 예정) | — | SKILL.md 로더 + watchdog + 라우터 |
| M6 (commands, 예정) | — | TOML + Jinja2 로더 + 라우터 |
| M7 (TUI, 예정) | `crates/tui` 실장 | — |
| M8 (event bus, 예정) | `crates/bus` 실장 | `OnStreamDelta` 활용 |
| M9 (CI + 문서) | CI 스크립트 | 보안 정책, 세션 DB |

**패턴이 보이죠?**
- M5/M6 같은 "사용자 확장 기능"은 **전부 Python만** 만집니다.
- M7 TUI는 터미널 성능이 중요해서 **Rust만** 만집니다.
- M3 같은 "턴 로직"은 양쪽이 협업 (엔진=Rust, 정책=Python).

---

## "그럼 나는 어디를 고쳐야 할까?"

개발자가 ash-code를 확장/수정할 때 결정 트리:

1. **새 LLM provider 추가?** → `ashpy/src/ashpy/providers/` 에 Python
   파일 한 개.
2. **새 스킬 추가?** → `skills/<name>/SKILL.md` 파일 하나.
3. **새 슬래시 명령?** → `commands/<name>.toml` 파일 하나.
4. **정책 (어떤 명령을 차단할지) 변경?** → `ashpy/src/ashpy/middleware/`
   에 Python 클래스 추가.
5. **새 HTTP 엔드포인트?** → `ashpy/src/ashpy/api/app.py`에 라우터 추가.
6. **새 빌트인 도구 (예: `http_fetch`)?** → 성능이 크게 중요하지 않다면
   M3+에서 Python 플러그인으로 (현재 아직 미구현). 성능이 정말
   중요하다면 `crates/tools/src/` 에 Rust로.
7. **턴 루프 동작 변경 (예: `max_turns` 기본값)?** → `crates/query/src/lib.rs`
   (Rust). 드물게 손대는 영역.
8. **TUI 버튼 추가?** → `crates/tui/src/` (Rust, M7 이후).

**기억할 규칙**: *처음엔 Python에서 시도*하세요. Python으로 못 할 것
같은 성능/안전성 요구가 나오면 그때 Rust로 내립니다. "엔진은 나중에
만들고, 조종석부터 만져라"가 ash-code의 철학입니다.

---

## 한 문장으로 요약

> **Rust는 ash-code의 뼈대다. Python은 살과 옷이다.**
>
> 뼈대는 단단하고 자주 안 바꾼다. 살과 옷은 매일 갈아입는다.
> 두 층은 gRPC라는 신경망으로 이어져 있어서, 옷을 갈아입어도 뼈대는
> 무너지지 않는다.
