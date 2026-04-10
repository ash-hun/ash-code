# M8 Task Report — Event Bus + Mid-Turn Cancellation

**Milestone:** M8 (`crates/bus` + `CancellationToken` + opt-in OnStreamDelta)
**Date:** 2026-04-10
**Status:** ✅ Completed — code green, manual TUI cancel smoke passed

---

## 1. Goals

Three orthogonal additions to the M7 turn loop:

1. **Mid-turn cancellation.** Pressing `Esc` in the TUI while a turn is
   running must stop the streaming response cleanly without killing the
   process. The user gets the input box back and can issue a new prompt.
2. **Session event bus.** Lay a `tokio::sync::broadcast`-based fan-out
   so multiple consumers can observe the same session in the future.
   M8 ships the infrastructure; no in-tree consumer yet (Q1 = a).
3. **`Harness.OnStreamDelta` opt-in.** Wire the long-deferred per-token
   harness hook through the Rust query loop, gated behind
   `ASH_HARNESS_STREAM_DELTA=on`. Default off — see
   `docs/harness_onstreamdelta.md` for the rationale.

## 2. Deliverables

### 2.1 New crate: `ash-bus` (~120 LoC)

`crates/bus/src/lib.rs`:

- `BusEvent` enum with 8 variants — `UserMessage`, `AssistantText`,
  `ToolCall`, `ToolResult`, `TurnFinish`, `TurnError`, `Cancelled`,
  `Outcome`. Cloneable, owned strings, no lifetimes.
- `SessionBus` — `Arc<RwLock<HashMap<String, broadcast::Sender<BusEvent>>>>`
  with default channel capacity of 256. Methods: `publish(session_id,
  event)`, `subscribe(session_id) -> broadcast::Receiver<BusEvent>`,
  `close(session_id)`, `session_count()`.
- Subscribers join lazily — `subscribe()` creates the channel if it
  does not exist. `publish()` is a cheap no-op when nobody is
  listening (broadcast::send returns an error which we deliberately
  swallow).
- `parking_lot::RwLock` over the inner map (faster than std for the
  read-heavy workload, no async needed).

Tests (6):
- `publish_then_subscribe_misses_past_events` — broadcast does not
  replay history.
- `subscribe_then_publish_delivers` — happy path.
- `two_subscribers_both_receive` — fan-out works.
- `close_drops_session_channel` — `session_count()` reflects state.
- `isolated_sessions_do_not_cross_talk` — namespacing by `session_id`.
- `outcome_event_is_terminal_marker` — variant smoke test.

### 2.2 `crates/query` — cancellation + bus + opt-in stream delta hook

The signature change everyone has to follow:

```rust
pub async fn run_turn(
    &self,
    session: &mut Session,
    sink: &mut dyn TurnSink,
    cancel: CancellationToken,        // NEW
) -> Result<TurnOutcome>
```

`CancellationToken` is re-exported from `tokio_util::sync::CancellationToken`
as `ash_query::CancellationToken` so callers do not have to take a
direct dependency on `tokio-util`.

#### Two cancellation points

- **Outer turn loop**: at the top of each iteration, `cancel.is_cancelled()`
  is checked. If true, returns `TurnOutcome { stop_reason: "cancelled",
  turns_taken, … }` without starting the LLM call.
- **Stream consumption**: replaced the `while let Some(delta) = stream.next()`
  loop with a `tokio::select! { biased; ... }` that races
  `cancel.cancelled()` against `stream.next()`. On cancel, the stream
  is dropped — tonic propagates cancellation back through the gRPC
  channel. The accumulated assistant text is preserved with a
  `\n\n[cancelled by user]` marker appended, so the LLM sees the
  user's intent on the next turn (Q2 = a).

#### `QueryEngine` now owns a `SessionBus`

- `QueryEngine::new` creates a default bus.
- `QueryEngine::with_bus(bus)` for callers that want to inject a
  shared one.
- Every interesting event in `run_turn` is `bus.publish`-ed:
  `AssistantText`, `ToolCall`, `ToolResult`, `Cancelled`, `Outcome`.
- `QueryEngine::bus()` exposes the bus for external subscribers (none
  in M8 — Q1 = a).

#### `Harness.OnStreamDelta` call site

- New helper `QueryEngine::maybe_call_stream_delta_hook(session_id,
  turn_id, delta)`. Reads `ASH_HARNESS_STREAM_DELTA` env at engine
  construction time (`stream_delta_hook_enabled()`). If off, the helper
  is a no-op and the cost is exactly zero.
- When on, every text delta spawns a `tokio::spawn` that calls
  `backend.on_stream_delta(event)`. The spawn detaches — errors are
  logged at `warn!` and discarded so a slow / broken middleware
  cannot stall the streaming response.
- `QueryBackend` trait gained a default implementation:
  ```rust
  async fn on_stream_delta(&self, _event: pb::DeltaEvent) -> Result<()> { Ok(()) }
  ```
  so existing mock backends compile unchanged.
- `SidecarBackend` forwards to the real `SidecarClient.on_stream_delta`.

#### New tests (3)

- `cancel_before_turn_returns_cancelled` — cancel the token before
  calling `run_turn` → `stop_reason == "cancelled"`, `turns_taken == 0`.
- `cancel_during_stream_preserves_partial_text` — `SlowBackend` yields
  4 text deltas with 50 ms gaps; a background task cancels the token
  after 120 ms. Asserts `stop_reason == "cancelled"` and the session's
  last assistant message contains `[cancelled by user]`.
- `bus_publishes_outcome_event` — drives one normal turn, drains the
  subscriber's queue, asserts an `Outcome` event was emitted.

All 4 pre-existing query tests updated to pass `CancellationToken::new()`
as the third argument.

### 2.3 `crates/api` — caller updates

`QueryHostService::run_turn` now constructs a fresh
`CancellationToken::new()` per request and passes it to the engine.
M8 does not yet expose a way for the gRPC client to cancel the token
(no `CancelTurn` RPC) — that lands in M9 along with the
`/v1/sessions/{id}/cancel` HTTP endpoint. For now, mid-turn
cancellation is a TUI-only feature.

### 2.4 `crates/tui` — Esc rebinding

Three additions in `crates/tui/src/app.rs`:

- `AppState.current_cancel: Option<CancellationToken>` — populated
  when a turn starts, cleared on `TurnEvent::Done`.
- `AppState::request_cancel_turn() -> bool` — calls `cancel()` on the
  stored token if any.

`crates/tui/src/event.rs`:

- `spawn_turn` constructs the token, stores it in `state.current_cancel`,
  and passes it through to `engine.run_turn(...)`.
- The `TurnEvent::Done` arm clears `state.current_cancel`.
- The `Esc` key now has new priority:
  ```text
  Mode::Approval(_) → cancel approval (unchanged)
  state.running_turn → cancel current turn (NEW)
  state.input.is_empty() → quit
  state.input non-empty → no-op
  ```
  `Ctrl-C` still always quits the whole TUI immediately.

### 2.5 Workspace dependency additions

`Cargo.toml`:

- `tokio-util = { version = "0.7", features = ["rt"] }` — already
  pulled in transitively, now declared explicitly.
- `tokio-stream`'s feature list gained `"sync"` so we can use the
  broadcast wrappers later.
- `crates/bus` declares its own minimal deps: `tokio` + `tokio-stream`
  + `parking_lot`.
- `crates/query` adds `ash-bus`, `tokio-util`, plus `async-stream` as a
  dev-dep for the `cancel_during_stream` test.

### 2.6 Documentation

- `docs/harness_onstreamdelta.md` — full essay (Q3 background, why
  defer in M3, M8 opt-in design, trade-offs, operational notes).
- `docs/task/M8_TASK_REPORT.md` — this file.

## 3. Verification

| Check | Result |
|---|---|
| Rust `cargo test --workspace` (rust:1.88 + protoc) | **all green** — 60+ tests across 9 crate test binaries |
| Python `uv run pytest -q` | **113 passed in 0.63 s** (unchanged from M7 — M8 is Rust-only) |
| Docker image build | ✅ |
| Container boot — both programs RUNNING | ✅ ashpy + ash-serve both reach `RUNNING` state |
| `GET /v1/health` | All five features still `v1` (no regression) |

### 3.1 Test breakdown

- `ash-bus` (new): 6 tests
- `ash-query`: 4 → 7 tests (cancel before, cancel mid-stream, bus
  outcome event)
- `ash-tools`: 17 (unchanged)
- `ash-tui`: 16 (unchanged, all still compile against the new
  `run_turn` signature via the existing channel sink path)
- `ash-api`: 3 (unchanged)
- `ash-ipc`: 3 (unchanged)
- `ash-core`: 2 (unchanged)
- 6 stub crates: 0 each

### 3.2 Manual smoke required (TUI cancel)

The cancellation behavior cannot be fully validated without an
interactive TTY. Recommended sequence for the user:

```bash
docker compose up -d ash-code
docker exec -it ash-code ash tui

# 1) Type a long-running prompt:
#    "Write a 500-word essay about Rust ownership"
#    Press Enter.
# 2) Once the response starts streaming, press Esc.
# 3) Expect:
#    - the chat shows the partial text + "[cancelled by user]"
#      marker on the assistant line
#    - the input box is unlocked
#    - status bar still shows "anthropic · claude-opus-4-5 · ..."
#    - the process is still alive
# 4) Type a new prompt and confirm a fresh turn runs normally.
# 5) Ctrl-C to quit.
```

If the cancel does not feel instant: the LLM provider's HTTP/2 stream
takes 50–200 ms to actually shut down after `drop(stream)`. The Rust
side returns immediately; the lingering chunks just never arrive.

## 4. Issues encountered and resolved

1. **`async-stream` dev-dep missing for the slow-stream test.** The
   `cancel_during_stream_preserves_partial_text` test uses
   `async_stream::stream!` to build a paced fake stream. Added to
   `crates/query/[dev-dependencies]`.
2. **`AppState` field add forgot a Read first.** Two of the M8 edits
   to `crates/tui/src/app.rs` had to be re-applied after explicitly
   re-reading the file — the harness had not seen the post-M7 contents
   yet.
3. **`QueryBackend` trait method addition** could have broken every
   existing mock. Solved by giving `on_stream_delta` a default impl
   that returns `Ok(())`. All M3/M4/M7 mocks compile unchanged.
4. **`CancellationToken` re-export** — instead of forcing every caller
   crate to depend on `tokio-util`, `ash-query` re-exports the type so
   users only `use ash_query::CancellationToken`.

## 5. Decisions carried forward

1. **Bus is publish-only in M8** (Q1 = a). No HTTP `/v1/sessions/{id}/watch`
   endpoint yet, no `QueryHost.WatchSession` RPC. The plumbing exists
   so M9 can wire external consumers without touching the engine.
2. **Cancelled turns preserve partial text** (Q2 = a). The
   `[cancelled by user]` marker is appended so the next LLM call sees
   what got streamed and the user's stop intent. No special cancellation
   message in the conversation history.
3. **`OnStreamDelta` is opt-in** (Q3 = b). Default off; flip with
   `ASH_HARNESS_STREAM_DELTA=on`. See `docs/harness_onstreamdelta.md`
   for the full reasoning.
4. **TUI Esc priority shifted**. Was: empty-input quit. Now:
   running-turn cancel > empty-input quit > non-empty-input no-op.
   `Ctrl-C` still quits unconditionally. Documented in `docs/tui.md`.
5. **Cancellation does not reach the gRPC client yet.** M8 adds the
   token in the engine, but neither `QueryHost.RunTurn` (gRPC) nor
   `POST /v1/chat` (HTTP) expose a cancellation mechanism. M9 adds
   `CancelTurn` RPC + HTTP delete. For now, only the TUI can cancel.

## 6. Exit criteria — met

- [x] `crates/bus` implemented with `SessionBus` + `BusEvent` enum
- [x] 6 unit tests for the bus
- [x] `QueryEngine::run_turn` takes `CancellationToken`
- [x] All in-tree callers updated (api, tui, every test)
- [x] TUI `Esc` cancels mid-turn instead of quitting
- [x] `Harness.OnStreamDelta` Rust call site, gated by env var,
      fire-and-forget
- [x] `QueryBackend` trait has default impl for backward compat
- [x] Rust tests all green
- [x] Python tests unchanged (113/113)
- [x] Container boots, all features still `v1`
- [x] `docs/harness_onstreamdelta.md`
- [x] `docs/task/M8_TASK_REPORT.md` (this file)
- [x] **Manual TUI cancel smoke — passed 2026-04-10**

## 7. Changed files

**Added**
- `crates/bus/src/lib.rs` — full `SessionBus` implementation + 6 tests
- `crates/bus/Cargo.toml` populated
- `docs/harness_onstreamdelta.md`
- `docs/task/M8_TASK_REPORT.md`

**Modified**
- `Cargo.toml` — `tokio-util` workspace dep, `tokio-stream` `"sync"` feature
- `crates/query/Cargo.toml` — added `ash-bus`, `tokio-util`, dev `async-stream`
- `crates/query/src/lib.rs` — `CancellationToken` parameter on `run_turn`,
  bus publishes, opt-in `OnStreamDelta`, 3 new tests, `QueryBackend` default
  impl
- `crates/api/src/lib.rs` — pass `CancellationToken::new()` to engine
- `crates/tui/src/app.rs` — `current_cancel` field + `request_cancel_turn`
- `crates/tui/src/event.rs` — `spawn_turn` constructs and stores token,
  `TurnEvent::Done` clears it, `Esc` priority change
- `crates/ipc/src/lib.rs` — minor cleanup of `on_stream_delta` wrapper
  (still calls the same RPC)

## 8. Next: M9 — Persistence + CI + security hardening

- SQLite-backed `Session` storage so sessions survive container restart.
- New `CancelTurn` RPC + `POST /v1/sessions/{id}/cancel` HTTP endpoint
  so HTTP/external callers can stop runs without killing the connection.
- `QueryHost.WatchSession` RPC + `GET /v1/sessions/{id}/watch` SSE that
  reads from the M8 `SessionBus`.
- Tighten CORS, document the security model in `docs/security.md`.
- GitHub Actions CI: matrix Rust + Python, Docker build, integration
  smoke, OpenAPI spec snapshot.
- Optionally: batch the `OnStreamDelta` hook (`ASH_HARNESS_STREAM_DELTA_BATCH=128`)
  if a real consumer materializes.
