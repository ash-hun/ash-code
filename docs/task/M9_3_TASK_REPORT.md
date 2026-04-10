# M9.3 Task Report — WatchSession SSE

**Sub-milestone:** M9.3 (out of M9.1–9.5)
**Date:** 2026-04-10
**Status:** ✅ Completed

---

## 1. Goal

M8 introduced `SessionBus` as a publish-only in-process event bus.
No subscriber code existed — events were emitted by the query engine
but nobody could consume them externally. M9.3 wires up the subscriber
side so that any HTTP or gRPC client can observe a session's events in
real time:

- A new `QueryHost.WatchSession` gRPC server-streaming RPC
- A new `GET /v1/sessions/{id}/watch` HTTP SSE endpoint
- Multi-subscriber support (multiple watchers on the same session)

Primary use case: a developer runs a chat in the TUI (terminal 1)
while a second terminal or browser tab observes the same session's
events live via `curl -N`.

## 2. Deliverables

### 2.1 Proto — `WatchSession` RPC + `WatchEvent` message

Added to `QueryHost` service in `proto/ash.proto`:

```protobuf
service QueryHost {
  ...
  rpc WatchSession(WatchSessionRequest) returns (stream WatchEvent);  // NEW
  ...
}

message WatchSessionRequest { string session_id = 1; }
message WatchEvent {
  string event_type = 1;     // "user_message" | "assistant_text" | "tool_call"
                             // | "tool_result" | "turn_finish" | "turn_error"
                             // | "cancelled" | "outcome"
  string session_id = 2;
  bytes  payload = 3;        // JSON-encoded event-specific data (UTF-8)
}
```

Design choice: `WatchEvent` uses a single `bytes payload` (JSON) rather
than a protobuf `oneof` per event type. This keeps the proto simple and
avoids a combinatorial explosion of message types for what is an
observation channel, not a control plane. Consumers parse the JSON
payload keyed by `event_type`.

### 2.2 Rust — `bus_event_to_watch` conversion + `watch_session` handler

**Conversion function** (`crates/api/src/lib.rs`):

Maps all 8 `BusEvent` variants to `WatchEvent`:

| BusEvent variant | `event_type` | JSON payload fields |
|---|---|---|
| `UserMessage` | `user_message` | `text` |
| `AssistantText` | `assistant_text` | `text` |
| `ToolCall` | `tool_call` | `id`, `name`, `arguments` |
| `ToolResult` | `tool_result` | `name`, `ok`, `body` |
| `TurnFinish` | `turn_finish` | `stop_reason`, `input_tokens`, `output_tokens` |
| `TurnError` | `turn_error` | `message` |
| `Cancelled` | `cancelled` | `reason` |
| `Outcome` | `outcome` | `stop_reason`, `turns_taken`, `denied` |

**gRPC handler**:

```rust
async fn watch_session(&self, request: Request<WatchSessionRequest>)
    -> Result<Response<Self::WatchSessionStream>, Status>
{
    let mut rx = self.engine.bus().subscribe(&session_id);
    let (tx, out_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => { tx.send(Ok(bus_event_to_watch(...))); }
                Err(RecvError::Lagged(n)) => { warn!("lagged by {n}"); continue; }
                Err(RecvError::Closed) => { break; }
            }
        }
    });
    Ok(Response::new(UnboundedReceiverStream::new(out_rx)))
}
```

Key behaviors:
- **Lagged subscriber**: if a slow consumer falls behind the broadcast
  buffer (256 events), the handler logs a warning and continues from
  the current position rather than disconnecting.
- **Channel closed**: when `SessionBus.close(session_id)` is called,
  the spawn exits cleanly and the gRPC stream terminates.
- **Client disconnect**: when the gRPC client drops the stream,
  `tx.send()` returns `Err` and the spawn exits.

### 2.3 Python — `QueryHostClient.watch_session`

New async generator on the gRPC client wrapper:

```python
async def watch_session(self, session_id: str) -> AsyncIterator[dict]:
    req = ash_pb2.WatchSessionRequest(session_id=session_id)
    async for event in stub.WatchSession(req):
        yield {
            "event_type": event.event_type,
            "session_id": event.session_id,
            "payload": json.loads(event.payload) if event.payload else {},
        }
```

Gracefully yields an error event on `grpc.RpcError`.

### 2.4 FastAPI — `GET /v1/sessions/{id}/watch`

```python
@app.get("/v1/sessions/{session_id}/watch", tags=["sessions"])
async def watch_session(session_id: str):
    async def event_stream():
        async for event in client.watch_session(session_id):
            yield {"event": event["event_type"], "data": json.dumps(event)}
    return EventSourceResponse(event_stream())
```

SSE event names match `event_type` values, so a browser
`EventSource` can attach per-type listeners:

```javascript
const es = new EventSource("/v1/sessions/s1/watch");
es.addEventListener("assistant_text", (e) => { ... });
es.addEventListener("outcome", (e) => { ... });
```

### 2.5 New dependency — `serde_json` for `ash-api`

`bus_event_to_watch` serializes each event's payload to JSON bytes
using `serde_json::json!` + `serde_json::to_vec`. Already a workspace
dependency; added to `crates/api/Cargo.toml`.

## 3. Verification

### 3.1 Test results

| Suite | Result |
|---|---|
| Rust `cargo test -p ash-api` | **8 passed** (6 existing + 2 new) |
| Docker image rebuild | ✅ |

New test cases:

| Test | What it verifies |
|---|---|
| `watch_session_receives_bus_events` | Subscribe before a turn → watch stream receives `assistant_text` and `outcome` events matching what the query engine publishes |
| `watch_session_two_subscribers` | Two independent `WatchSession` streams on the same session both receive the `outcome` event from a single turn |

Both tests use a 2-second timeout to avoid hanging if no events arrive.

### 3.2 Usage example

```bash
# Terminal 1: start watching
curl -N http://localhost:8080/v1/sessions/demo/watch

# Terminal 2: chat on the same session
curl -s -N -X POST http://localhost:8080/v1/chat \
    -d '{"session_id":"demo","prompt":"hello"}'

# Terminal 1 output:
event: assistant_text
data: {"event_type":"assistant_text","session_id":"demo","payload":{"text":"Hello"}}

event: assistant_text
data: {"event_type":"assistant_text","session_id":"demo","payload":{"text":"!"}}

event: turn_finish
data: {"event_type":"turn_finish","session_id":"demo","payload":{"stop_reason":"end_turn","input_tokens":12,"output_tokens":3}}

event: outcome
data: {"event_type":"outcome","session_id":"demo","payload":{"stop_reason":"end_turn","turns_taken":1,"denied":false}}
```

## 4. Issues encountered and resolved

1. **`user_message` not published by QueryEngine** — Initial test
   asserted `user_message` in the watch stream, but the engine only
   publishes events from the LLM response loop (assistant text, tool
   calls, etc.). The `session.push_user()` call in `QueryHostService`
   does not go through the bus. Corrected the test to assert
   `assistant_text` instead. If `user_message` observation is needed
   in the future, `QueryHostService.run_turn` can publish it before
   spawning the engine task.
2. **`serde_json` dependency** — `bus_event_to_watch` needs JSON
   serialization. `serde_json` was already in the workspace but not
   in `ash-api`'s `Cargo.toml`. Added as a regular dependency (not
   dev-only, since the conversion runs in production code).

## 5. Decisions carried forward

1. **JSON payload over protobuf oneof.** `WatchEvent.payload` is
   `bytes` (JSON) rather than a typed oneof. This is an observation
   stream, not a control plane — schema evolution is easier with JSON,
   and consumers already handle JSON from the SSE layer.
2. **Lagged subscribers continue, not disconnect.** A slow watcher
   that misses events due to broadcast buffer overflow gets a warning
   log and resumes from the current position. This matches the
   "best-effort observation" philosophy — watch is not a durable
   event log.
3. **No replay of past events.** `tokio::sync::broadcast` does not
   buffer history. A watcher only sees events that occur after it
   subscribes. This is intentional: session history is available via
   `GET /v1/sessions/{id}`, and watch is for live observation.
4. **`user_message` events are not currently published to the bus.**
   The bus only sees events from inside `QueryEngine.run_turn`. If
   external watchers need to see user messages, the publish call
   should be added to `QueryHostService.run_turn` before the engine
   spawn.

## 6. Exit criteria — met

- [x] `proto/ash.proto` has `WatchSession` RPC + `WatchEvent` message
- [x] `bus_event_to_watch` converts all 8 `BusEvent` variants
- [x] `watch_session` gRPC handler with lagged/closed handling
- [x] `WatchSessionStream` type alias registered
- [x] `QueryHostClient.watch_session()` Python async generator
- [x] `GET /v1/sessions/{id}/watch` HTTP SSE endpoint
- [x] Multi-subscriber verified by test
- [x] 2 new unit tests, all passing
- [x] Docker image builds successfully
- [x] `docs/task/M9_3_TASK_REPORT.md`

## 7. Changed files

**Added**
- `docs/task/M9_3_TASK_REPORT.md`

**Modified**
- `proto/ash.proto` — `WatchSession` RPC, `WatchSessionRequest`,
  `WatchEvent` messages
- `crates/api/Cargo.toml` — `serde_json` dependency
- `crates/api/src/lib.rs` — `bus_event_to_watch()` conversion,
  `WatchSessionStream` type, `watch_session` handler, 2 new tests
- `ashpy/src/ashpy/api/query_client.py` — `watch_session()` method,
  `json` import
- `ashpy/src/ashpy/api/app.py` — `GET /v1/sessions/{id}/watch`
  SSE endpoint

## 8. Next: M9.4 — Security policy + `docs/security.md`

- CORS policy externalized via `ASH_CORS_ORIGINS` env
- Optional bearer token auth via `ASH_API_TOKEN` env
- Startup warning when running without auth + open CORS
- `docs/security.md` threat model and deployment guidance
