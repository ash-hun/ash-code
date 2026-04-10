# M9.2 Task Report — Cancel HTTP / gRPC

**Sub-milestone:** M9.2 (out of M9.1–9.5)
**Date:** 2026-04-10
**Status:** ✅ Completed

---

## 1. Goal

M8 introduced `CancellationToken` for TUI-initiated mid-turn
cancellation, but HTTP and gRPC clients had no way to stop an
in-flight turn. M9.2 exposes cancel to every client surface:

- A new `QueryHost.CancelTurn` gRPC RPC
- A new `POST /v1/sessions/{id}/cancel` HTTP endpoint
- Automatic cancellation of the previous turn when the same session
  receives a concurrent `RunTurn` (M9 briefing Q2=a decision)

## 2. Deliverables

### 2.1 Proto — `CancelTurn` RPC + messages

Added to `QueryHost` service in `proto/ash.proto`:

```protobuf
service QueryHost {
  rpc RunTurn(RunTurnRequest) returns (stream TurnDelta);
  rpc CancelTurn(CancelTurnRequest) returns (CancelTurnResponse);  // NEW
  ...
}

message CancelTurnRequest { string session_id = 1; }
message CancelTurnResponse {
  bool ok = 1;
  string message = 2;   // "cancelled" or "no active turn"
}
```

Semantics: `ok=true` means a token was found and cancelled.
`ok=false` means no active turn exists for that session (not an error —
the turn may have already finished).

### 2.2 Rust — `QueryHostService` token map

`QueryHostService` gained a new field:

```rust
active_tokens: Arc<RwLock<HashMap<String, CancellationToken>>>
```

**Token lifecycle:**

1. **Registration** — Before `tokio::spawn`, `run_turn` acquires a
   write lock and inserts `(session_id, cancel.clone())`. If a
   previous token exists for the same session, it is `.cancel()`-ed
   immediately (concurrent RunTurn auto-cancel).
2. **Propagation** — The cloned token is passed into
   `engine.run_turn(...)`. The query engine checks `is_cancelled()`
   at every loop iteration and inside `tokio::select!` during stream
   consumption (existing M8 behavior).
3. **Cleanup** — On both the success and error paths inside the
   spawned task, the token is removed from the map. The success path
   checks identity (via `is_cancelled()` state) to avoid removing a
   newer token that replaced it.

**`cancel_turn` handler:**

```rust
async fn cancel_turn(&self, request: Request<CancelTurnRequest>)
    -> Result<Response<CancelTurnResponse>, Status>
{
    let map = self.active_tokens.read().await;
    if let Some(token) = map.get(&session_id) {
        token.cancel();
        // ok=true, message="cancelled"
    } else {
        // ok=false, message="no active turn"
    }
}
```

Read lock only — the spawned task's cleanup removes the entry later.

### 2.3 Python — `QueryHostClient.cancel_turn`

New method on the gRPC client wrapper:

```python
async def cancel_turn(self, session_id: str) -> dict:
    resp = await stub.CancelTurn(CancelTurnRequest(session_id=session_id))
    return {"ok": resp.ok, "message": resp.message}
```

### 2.4 FastAPI — `POST /v1/sessions/{id}/cancel`

```python
@app.post("/v1/sessions/{session_id}/cancel",
          response_model=CancelTurnResponse, tags=["sessions"])
async def cancel_turn(session_id: str) -> CancelTurnResponse:
    result = await client.cancel_turn(session_id)
    return CancelTurnResponse(**result)
```

Returns 200 in all cases. The `ok` field tells the caller whether a
turn was actually cancelled. This is intentional: "no active turn" is
not an error condition.

### 2.5 Pydantic schema

```python
class CancelTurnResponse(BaseModel):
    ok: bool
    message: str
```

## 3. Verification

### 3.1 Test results

| Suite | Result |
|---|---|
| Rust `cargo test -p ash-api` | **6 passed** (3 existing + 3 new) |
| Docker image rebuild | ✅ |

New test cases:

| Test | What it verifies |
|---|---|
| `cancel_turn_cancels_active_turn` | Start a slow turn (10 s delay backend), call `CancelTurn` → `ok=true, message="cancelled"` |
| `cancel_turn_no_active_turn` | Call `CancelTurn` on a session with no running turn → `ok=false, message="no active turn"` |
| `concurrent_run_turn_cancels_previous` | Start a slow turn, then start a second turn on the same session. The first stream receives a `cancelled` outcome/error event |

The `SlowBackend` test helper uses `async-stream` to introduce a
configurable delay, enabling reliable cancel-before-completion
scenarios without flaky timing.

### 3.2 Usage example

```bash
# Terminal 1: start a long chat
curl -s -N -X POST http://localhost:8080/v1/chat \
    -d '{"session_id":"cancel-demo","prompt":"Write a 1000 word essay"}'

# Terminal 2: cancel it mid-stream
curl -s -X POST http://localhost:8080/v1/sessions/cancel-demo/cancel
# → {"ok": true, "message": "cancelled"}

# Terminal 1 SSE stream ends with:
# event: outcome
# data: {"type":"outcome","stop_reason":"cancelled","turns_taken":1,...}
```

## 4. Issues encountered and resolved

1. **Token registration timing** — Initial implementation created the
   `CancellationToken` inside `tokio::spawn`, making it invisible to
   external callers until the task started. Moved token creation and
   map insertion to _before_ the spawn, ensuring `CancelTurn` can
   reach the token immediately after `RunTurn` returns.
2. **Error-path token leak** — The early-return path on engine error
   did not clean up the token from the active map. Added
   `tokens.write().await.remove(&sid)` to the error branch.
3. **Token identity on cleanup** — When a concurrent `RunTurn`
   replaces the token in the map, the finishing first turn should not
   remove the new token. The success cleanup compares
   `is_cancelled()` state to decide whether the entry is still "ours".

## 5. Decisions carried forward

1. **`CancelTurn` returns 200 even when no turn is active.** This
   avoids 404/409 ambiguity and makes idempotent fire-and-forget
   cancellation safe for clients.
2. **Same-session concurrent `RunTurn` auto-cancels the previous
   one.** This is the M9 briefing Q2=a decision. No explicit
   `CancelTurn` call needed — just send a new `RunTurn`.
3. **SSE client disconnect auto-cancel is deferred.** Detecting axum
   SSE disconnects reliably requires changes in the streaming layer.
   Deferred to a future milestone if demand arises.
4. **Read lock for `cancel_turn`, write lock for registration.**
   `CancellationToken::cancel()` is safe to call through a shared
   reference, so the cancel handler only needs a read lock, avoiding
   contention with concurrent `RunTurn` registrations.

## 6. Exit criteria — met

- [x] `proto/ash.proto` has `CancelTurn` RPC + messages
- [x] `QueryHostService` holds `active_tokens` map
- [x] Token registered before spawn, cleaned up on both success/error
- [x] Concurrent `RunTurn` auto-cancels previous turn
- [x] `CancelTurn` gRPC handler implemented
- [x] `QueryHostClient.cancel_turn()` Python method
- [x] `POST /v1/sessions/{id}/cancel` HTTP endpoint
- [x] `CancelTurnResponse` Pydantic schema
- [x] 3 new unit tests, all passing
- [x] Docker image builds successfully
- [x] `docs/task/M9_2_TASK_REPORT.md`

## 7. Changed files

**Added**
- `docs/task/M9_2_TASK_REPORT.md`

**Modified**
- `proto/ash.proto` — `CancelTurn` RPC, `CancelTurnRequest`,
  `CancelTurnResponse` messages
- `crates/api/Cargo.toml` — `async-stream` dev-dependency
- `crates/api/src/lib.rs` — `active_tokens` field,
  `cancel_turn` handler, token lifecycle in `run_turn`,
  `SlowBackend` test helper, 3 new tests
- `ashpy/src/ashpy/api/query_client.py` — `cancel_turn()` method
- `ashpy/src/ashpy/api/schemas.py` — `CancelTurnResponse` schema
- `ashpy/src/ashpy/api/app.py` — `POST /v1/sessions/{id}/cancel`
  endpoint, `CancelTurnResponse` import

## 8. Next: M9.3 — `WatchSession` SSE

- New `QueryHost.WatchSession` gRPC server-streaming RPC
- `BusEvent` Rust enum → protobuf `BusEventProto` conversion
- `SessionBus.subscribe(session_id)` → tonic Stream
- `GET /v1/sessions/{id}/watch` HTTP SSE endpoint
- Multi-subscriber support via `broadcast` channel
