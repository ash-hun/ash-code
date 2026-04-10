# `Harness.OnStreamDelta` — Why It's Off By Default (M8)

This document records ash-code's stance on the `Harness.OnStreamDelta`
hook: what it is, why it exists in the proto, why it was deferred in
M3, and why M8 chose to ship it as **opt-in via environment variable**
instead of always-on.

## What the hook is

`Harness.OnStreamDelta` is one of the four query-loop hooks declared
in `proto/ash.proto::Harness`:

```proto
service Harness {
  rpc OnTurnStart(TurnContext)  returns (HookDecision);
  rpc OnToolCall(ToolCallEvent) returns (HookDecision);
  rpc OnStreamDelta(DeltaEvent) returns (Empty);     // ← this one
  rpc OnTurnEnd(TurnResult)     returns (Empty);
}
```

It fires whenever the LLM emits a streaming chunk (text token, tool-use
block, finish marker). Where the other three hooks happen at turn
boundaries, `OnStreamDelta` happens **inside** a turn — many times per
second while the response is being streamed.

The Python side of the hook lives in `ashpy/middleware/base.py`:

```python
class Middleware(ABC):
    async def on_stream_delta(self, delta: dict) -> None:
        return None
```

`MiddlewareChain.on_stream_delta` walks every registered middleware in
priority order and awaits each one. There is **no** short-circuit and
**no** decision return value — this hook is observation-only. It cannot
deny or rewrite the stream; if you want that, use `OnToolCall` or
`OnTurnStart`.

## What it would let users build

Hypothetical use cases (none currently shipped in ash-code):

- **PII redaction monitor** — alert when an SSN, API key, or email
  pattern appears in the model output mid-stream.
- **Real-time token counter / budget guard** — accumulate output
  tokens and log a warning when a threshold is crossed.
- **External stream sink** — forward every delta to Kafka, OpenTelemetry,
  or a SIEM as it happens, instead of waiting for `OnTurnEnd`.
- **Latency telemetry** — measure inter-token timing distributions to
  detect provider degradation.

These are all things you might want for **production observability**.
None of them are needed for ash-code's current "developer harness"
posture, which is why no built-in middleware uses `on_stream_delta`.

## Why M3 deferred the call site

The proto contract was added in M1, the Python side was implemented
along with the rest of the harness servicer in M3, and the unit /
integration tests for the chain landed in M3. But the **Rust query
loop never actually invokes the RPC**.

The reason was cost. A typical LLM turn streams 500–2000 text deltas.
At one gRPC round-trip per delta:

- ~1–3 ms per round-trip on loopback
- 1000 deltas × 2 ms ≈ 2 seconds of added latency per turn
- the delta itself is tiny — overhead dominates payload

The user-perceived effect would be noticeable streaming jank ("why is
the response stuttering?") for zero benefit, because no middleware was
actually consuming the events. M3 traded "spec completeness" for
"interactive smoothness".

## What M8 changed

M8 enables the call site, but **off by default**.

### Mechanism

A new environment variable controls whether the Rust turn loop calls
`Harness.OnStreamDelta` at all:

```
ASH_HARNESS_STREAM_DELTA=on    # enable
ASH_HARNESS_STREAM_DELTA=off   # disable (default)
ASH_HARNESS_STREAM_DELTA=      # disable
```

When disabled, `crates/query::QueryEngine::run_turn` simply skips the
RPC and the cost is exactly zero.

When enabled, every text delta produces one `Harness.OnStreamDelta`
call. The call is **fire-and-forget** from the loop's perspective —
spawned with `tokio::spawn`, errors logged at `warn!` and discarded.
The streaming response continues regardless of how slow or how broken
the middleware chain is. This guarantees that a misbehaving middleware
cannot stall a turn.

### Why opt-in instead of always-on with batching

The pre-vote alternative was to always call the hook but batch
deltas — e.g. every 128 characters or 100 ms — so that a typical turn
produced 10 calls instead of 1000. We considered three options:

| Option | Default | Cost when no consumer | Latency per call |
|---|---|---|---|
| (a) Always on, batched (128 chars / 100 ms) | enabled | ~10 RPCs/turn paid for nothing | low (debounced) |
| **(b) Opt-in via env (chosen)** | disabled | **zero** | per-token when enabled |
| (c) Always on, per-token | enabled | 500–2000 RPCs/turn | high |

We picked (b) because:

1. **No middleware in the tree consumes `on_stream_delta` today.**
   The two built-ins (`logging`, `bash_guard`) only override the turn-
   boundary hooks. Paying *any* per-turn cost for a feature with zero
   in-tree consumers is pure overhead.
2. **Opt-in is honest.** A user who sets
   `ASH_HARNESS_STREAM_DELTA=on` is explicitly saying "I have a
   middleware that needs this; I accept the latency". A user who
   doesn't set it gets the same fast streaming they had in M3–M7.
3. **Easy to flip later.** If a future ash-code release ships a
   middleware that needs `on_stream_delta`, a one-line default change
   in the env handler turns this into option (a) without any caller
   updates. Going the other direction (a → b) is harder because
   existing users would notice the change.
4. **Consistent with M3's reasoning.** M3 deferred for cost; M8
   honours that reason while still completing the interface.

### Trade-offs we accept

- **A user who writes a `on_stream_delta` middleware and forgets to
  set the env var** will be confused when their middleware never
  fires. We mitigate this by:
  - Logging the env var state at sidecar startup
    (`[ashpy] OnStreamDelta hook: enabled` / `disabled`).
  - Documenting the requirement in `docs/extensibility.md` next to
    the `Middleware` ABC reference and in this file.
- **No batching when enabled.** When `ASH_HARNESS_STREAM_DELTA=on`, you
  get per-token RPCs. If that turns out too slow, M9 may add a
  `ASH_HARNESS_STREAM_DELTA_BATCH=128` knob. We deliberately did not
  ship batching in M8 to keep the M8 surface small.

## Operational notes

### Verifying the state

```bash
docker exec ash-code env | grep ASH_HARNESS_STREAM_DELTA
docker logs ash-code 2>&1 | grep "OnStreamDelta hook"
```

The startup log line tells you which mode the sidecar booted in.

### Enabling in compose

```yaml
# docker-compose.yml
services:
  ash-code:
    environment:
      - ASH_HARNESS_STREAM_DELTA=on
```

Then `docker compose up -d --force-recreate ash-code`.

### What "fire and forget" means in practice

```rust
// crates/query/src/lib.rs (sketch)
if self.stream_delta_enabled {
    let backend = self.backend.clone();
    let evt = build_delta_event(...);
    tokio::spawn(async move {
        if let Err(err) = backend.on_stream_delta(evt).await {
            tracing::warn!("on_stream_delta failed: {err:#}");
        }
    });
}
```

- The spawned task races with the next stream delta. Order of
  delivery to the middleware chain is **not** guaranteed under load.
- If the middleware panics or hangs, the turn loop is unaffected.
- There is no back-pressure: a slow middleware will simply queue
  spawned tasks. A future M9 enhancement could add a bounded channel
  if this becomes a problem.

## Summary

| Aspect | Value |
|---|---|
| Proto contract | `Harness.OnStreamDelta(DeltaEvent) returns (Empty)` |
| Python side (`MiddlewareChain.on_stream_delta`) | Implemented since M3 |
| Rust side (`QueryEngine::run_turn` call site) | Implemented in M8, **disabled by default** |
| Toggle | `ASH_HARNESS_STREAM_DELTA=on` |
| Delivery semantics | Fire-and-forget, per-token, no batching |
| Decision return | Observation-only, no allow/deny/rewrite |
| Built-in consumers | None |
| Future direction | M9 may add batching knob if a real use case emerges |

The hook exists so that **the day a user wants real-time stream
observability, they can have it without changing the proto**. Until
that day, ash-code does not pay for it.
