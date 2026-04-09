# grpcio vs grpc.aio — comparison for ash-code

Both shipped by the same `grpcio` PyPI package. They are not different
libraries, just two different server/client programming models on top of
the same C core. ash-code chose `grpc.aio` in M2.

## TL;DR

| Axis | `grpcio` (sync) | `grpc.aio` (async) |
|---|---|---|
| Server API | `grpc.server(ThreadPoolExecutor(...))` | `grpc.aio.server()` |
| Servicer method | `def Ping(self, request, context):` | `async def Ping(self, request, context):` |
| Concurrency model | Thread pool, one thread per in-flight RPC | Single event loop, cooperative asyncio |
| Fits async SDKs (`anthropic`, `openai`, `ollama`) | Awkward — needs bridging | Native — `await` directly |
| Streaming RPC | Generator (`yield`) in a worker thread | `async for` in the event loop |
| Cancellation on client disconnect | Polled via `context.is_active()` | `asyncio.CancelledError` raised into the coroutine |
| Graceful shutdown | `server.stop(grace)` blocks | `await server.stop(grace)` cooperates with the loop |
| CPU overhead per RPC | Higher (thread context switch) | Lower (task switch) |
| Complexity for mixed sync+async code | Hidden footguns (see below) | Pay once at the entry point, then pure async |

## Why ash-code picked `grpc.aio`

Every LLM provider SDK in M2 (`anthropic`, `openai`, `ollama`) exposes an
**async** streaming API:

```python
async with anthropic.AsyncAnthropic().messages.stream(...) as stream:
    async for text in stream.text_stream:
        yield ChatDelta(text=text)
```

If the gRPC servicer were sync (`grpcio`), the only way to consume that
stream is to bridge each awaitable back onto a background event loop:

```python
# sync grpcio path — what we would have to write
import asyncio

class LlmProviderServicer(...):
    def __init__(self):
        self._loop = asyncio.new_event_loop()
        threading.Thread(target=self._loop.run_forever, daemon=True).start()

    def ChatStream(self, request, context):
        async def run():
            async for delta in provider.chat_stream(...):
                yield delta
        # schedule the async generator on the other thread, pull items back
        fut = asyncio.run_coroutine_threadsafe(
            _drain(run()), self._loop
        )
        for item in _blocking_iter(fut):
            yield item
```

The bridge is doable but brittle:

- **Two event loops per process.** One owned by gRPC threads implicitly,
  one owned by us for the SDK clients. Easy to leak tasks on the wrong
  loop.
- **Cancellation is lossy.** When the client disconnects, grpcio sets
  `context.is_active() == False`, but the coroutine on the other loop
  keeps running until the next `await` point. Dangling HTTP connections
  to Anthropic/OpenAI are a real cost.
- **Back-pressure is manual.** You have to bound a queue between the two
  worlds; if the consumer is slower than the producer, either you block
  a gRPC thread or you drop frames.
- **Exceptions cross a thread boundary.** Tracebacks become useless; the
  real stack is on a background thread, the surfaced stack is a generic
  `Future.result()` frame.

With `grpc.aio`, the same code is:

```python
class LlmProviderServicer(ash_pb2_grpc.LlmProviderServicer):
    async def ChatStream(self, request, context):
        async for delta in self.registry.current().chat_stream(request):
            yield delta
```

One loop, one stack, native cancellation.

## What changed in ash-code server.py (M1 → M2)

| M1 (`grpcio` sync) | M2 (`grpc.aio`) |
|---|---|
| `grpc.server(ThreadPoolExecutor(max_workers=8))` | `grpc.aio.server()` |
| `def Ping(self, request, context):` | `async def Ping(self, request, context):` |
| `server.start(); server.wait_for_termination()` | `await server.start(); await server.wait_for_termination()` |
| SIGTERM handler calls `server.stop(grace)` directly | SIGTERM handler sets an `asyncio.Event`; `serve()` awaits it, then awaits `server.stop()` |
| Servicer methods can be mixed with blocking code "for free" | Any blocking call inside a servicer stalls the entire sidecar — wrap with `asyncio.to_thread(...)` |
| Unit tests use `grpc.insecure_channel` | Unit tests use `grpc.aio.insecure_channel` and `async def` test functions with `pytest-asyncio` |

## Remaining gotchas we accept

1. **Blocking code must be wrapped.** `pydantic` validation and TOML parsing
   are fine (fast and CPU-bound), but file I/O in hot paths needs
   `asyncio.to_thread`. Skills/commands watchdog (M5/M6) will live on a
   dedicated thread and signal the loop via a queue.
2. **pytest integration.** `pytest-asyncio` is already in `dev` extras.
   Tests that touch the server use `@pytest.mark.asyncio` and
   `grpc.aio.insecure_channel`.
3. **One event loop.** `ashpy serve` owns the loop. Anything that wants
   its own loop (e.g. a library that calls `asyncio.run` internally) will
   break. We haven't hit one yet; revisit if we do.

## When we might revisit

If a future milestone needs to run **many** short RPCs with heavy CPU work
per call (e.g. large JSON schema validation, local embeddings), the
single-loop model can become a bottleneck. At that point we have two
options:

- Keep `grpc.aio` and offload CPU work to `asyncio.to_thread` or a
  `ProcessPoolExecutor`.
- Switch back to sync `grpcio` for the heavy service only and keep
  `grpc.aio` for the streaming LLM path.

Neither is needed for M2–M8.
