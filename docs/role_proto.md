# Role of `proto/`

One-line summary: **the contract that defines what words Rust and Python
exchange, and in what shape.** Language-neutral, compiled into each side's
native code at build time.

## Why it exists

ash-code runs as **two processes** inside the same container:

- Rust `ash` — TUI, core loop, HTTP API
- Python `ashpy` — skills, commands, LLM providers

They share a container but not a memory space, so they cannot call each
other's functions directly. They talk over gRPC, and gRPC needs both sides
to agree on the **exact shape** of every message. That agreement lives in
`proto/ash.proto`, and the directory that holds it is `proto/`.

## How it works

```
            proto/ash.proto   ← the single human-authored source
                 │
       ┌─────────┴─────────┐
       ▼                   ▼
  protoc (Rust)      protoc (Python)
       │                   │
       ▼                   ▼
  tonic-build         grpc_tools.protoc
       │                   │
       ▼                   ▼
  target/.../ash.v1.rs    ashpy/_generated/ash_pb2.py
  (included by            (imported by
   crates/ipc)             ashpy.server)
```

The actual flow in ash-code:

1. **Human** writes `service Health { rpc Ping(...) returns (...); }` in
   `proto/ash.proto`.
2. **Rust build time**: `crates/ipc/build.rs` invokes `tonic-build`, which
   runs `protoc` and emits Rust types, client stubs, and server traits.
   `crates/ipc/src/lib.rs` pulls them in with
   `tonic::include_proto!("ash.v1")`.
3. **Python build time** (Dockerfile `py-builder` stage):
   `ashpy/_codegen.py` calls `grpc_tools.protoc` to emit
   `ashpy/_generated/ash_pb2.py` and `ash_pb2_grpc.py`.
4. **Runtime**: Rust calls `SidecarClient.ping()` → tonic serializes the
   request as protobuf → TCP to the Python sidecar → grpcio deserializes →
   `HealthServicer.Ping()` runs → the response travels back the same way.

Because both sides generate their code from the **same `.proto` file**,
type mismatches are structurally impossible. Rust cannot use a
`PingRequest.client` field that Python does not also know about — the
generators would diverge and the build would fail first.

## Why keep the `.proto` source instead of the generated files

1. **Single source of truth.** One place to edit; both languages update
   automatically. No drift between Rust and Python stubs.
2. **Language neutrality.** Need a Go client later? A TypeScript client?
   Run `protoc` with a different plugin on the same file. The source
   never moves.
3. **Reviewable diffs.** `.proto` is terse, human-readable, and PR-diffs
   are trivial. Generated `.rs` and `.py` are thousands of lines of
   machine output that nobody should be reviewing by eye.
4. **Generated artifacts are not committed.** In ash-code the Rust side
   lands under `target/` and the Python side under
   `ashpy/src/ashpy/_generated/` (gitignored). Only `.proto` is in git.

## What `proto/` currently contains

```
proto/
└── ash.proto   # 6 services, 30+ message types
```

The services defined inside:

| Service | Purpose | Status |
|---|---|---|
| `Health` | Liveness + version handshake | **live** (M1) |
| `LlmProvider` | Provider abstraction + `ChatStream` | planned (M2) |
| `SkillRegistry` | SKILL.md discovery + hot-reload `Watch` | planned (M5) |
| `CommandRegistry` | Slash-command registry | planned (M6) |
| `Harness` | Turn-loop hooks / middleware control plane | planned (M3) |
| `ToolRegistry` | Third-party tool plugins | planned (M3+) |

As each milestone ships, the corresponding entry in
`Health.Ping.PingResponse.features` flips from `"planned"` to `"v1"`, so
clients can detect which services are actually live without issuing every
RPC.

## Analogy

`proto/ash.proto` is a **treaty between two countries**. The Rust country
calls it `SidecarClient::ping()`, the Python country calls it
`HealthStub.Ping()`, but both those names translate to the same
`Health.Ping` clause in the treaty. Amend the treaty and both countries
immediately receive new translations (generated code).

That is why `proto/` is the most important boundary in ash-code: as long
as it stays stable, Rust and Python can stay completely ignorant of each
other's implementation and still work together.

## Practical rules for editing `proto/ash.proto`

- **Additive changes are safe.** Adding a new RPC, a new message, or a
  new optional field does not break existing clients.
- **Never renumber or repurpose a field tag.** Protobuf wire format
  depends on the numeric tags; changing `int32 foo = 3;` to
  `string foo = 3;` corrupts every peer that has not been rebuilt.
- **Removing a field?** Mark the tag `reserved` so it cannot be reused
  accidentally.
- **Breaking changes bump the package.** If an incompatible redesign is
  needed, introduce `package ash.v2;` alongside `ash.v1;` and migrate
  services one by one. The `api_version` string in
  `PingResponse.api_version` is the sniff point clients use to decide
  which namespace to speak.
- **After any edit**: rebuild the Docker image so both
  `tonic-build` and `grpc_tools.protoc` regenerate their stubs in lock
  step. Running only one side is the fastest way to ship a bug.
