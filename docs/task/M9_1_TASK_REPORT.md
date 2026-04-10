# M9.1 Task Report — Session Persistence (PostgreSQL)

**Sub-milestone:** M9.1 (out of M9.1–9.5)
**Date:** 2026-04-10
**Status:** ✅ Completed

---

## 1. Goal

Replace the in-process `HashMap<String, Session>` that `QueryHostService`
has used since M4 with a real persistence layer so sessions survive
container restarts. PostgreSQL chosen over SQLite at user request: it
matches production deployments better, supports concurrent access from
multiple ash-code replicas in the future, and lets the same
`SessionStore` impl point at either a bundled dev instance OR an
external managed database.

## 2. Deliverables

### 2.1 New `crates/core::storage` module

Three files:

| File | Contents |
|---|---|
| `storage/mod.rs` | `SessionStore` trait, `SessionRecord`, `StoredMessage`, `SessionSummary`, `build_default()` env-driven factory, `conformance::run_full_suite` shared test helper |
| `storage/memory.rs` | `MemoryStore` — `Mutex<HashMap>` impl. Used for tests + the `ASH_SESSION_STORE=memory` escape hatch |
| `storage/postgres.rs` | `PostgresStore` — `tokio-postgres` + `deadpool-postgres`. Connect-with-retry, advisory-locked schema bootstrap, JSONB round-trip |

`SessionStore` is the only public contract. Both backends are validated
against the **same** `conformance::run_full_suite` so behavior cannot
drift.

Conformance helper deliberately uses **per-test unique row ids**
(`format!("conf-{}", now_ms())`) instead of `assert!(list.len() == N)`
so multiple test runners can share a single PostgreSQL without
trampling each other.

### 2.2 PostgreSQL schema

```sql
CREATE TABLE IF NOT EXISTS sessions (
    id            TEXT PRIMARY KEY,
    provider      TEXT NOT NULL,
    model         TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    messages      JSONB NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sessions_updated_at_ms
    ON sessions (updated_at_ms DESC);
```

Wrapped at runtime in `BEGIN; SELECT pg_advisory_xact_lock(8923457501);
... COMMIT;` so concurrent boots cannot race into a `pg_type_typname_nsp_index`
duplicate-key error. `BIGINT` epoch ms instead of `TIMESTAMPTZ` keeps
the rust serialization story trivial (`i64` round-trips natively).

### 2.3 `tokio-postgres` + `deadpool-postgres`

Q1 = (a). Lightweight, async-native, no compile-time DB requirement
that would complicate CI.

Workspace deps added:
- `tokio-postgres = "0.7"` with `with-serde_json-1`, `with-chrono-0_4` features
- `deadpool-postgres = "0.14"`
- `chrono = "0.4"` (transitive helper for tokio-postgres)

`PostgresStore::new` parses the URL via `tokio_postgres::Config::from_str`,
extracts host/port/user/password/dbname, hands them to
`deadpool_postgres::Config`, builds a Tokio runtime pool. `connect_with_retry`
loops up to 10× × 500 ms while postgres is still booting.

### 2.4 `crates/api::QueryHostService` rewrite

Before: `Arc<RwLock<HashMap<String, Session>>>`.
After: `Arc<dyn SessionStore>`.

- New constructors: `QueryHostService::new(engine, store, provider, model)`
  for production wiring, `new_in_memory(...)` for in-process tests.
- `record_to_session` / `session_to_record` helpers to translate
  between the persistence DTO (`SessionRecord`) and the runtime
  `ash-query::Session`.
- `RunTurn` flow:
  1. `store.get(session_id)` → if found and `!reset`, hydrate from DB
  2. else fresh `Session`
  3. `engine.run_turn(...)`
  4. `store.put(SessionRecord::from(...))` (per-turn write-through)
- `ListSessions` / `GetSession` / `DeleteSession` are now thin
  passthroughs to the store.
- `ash_api::serve(...)` calls `storage::build_default()` at startup,
  which honours `ASH_SESSION_STORE` (`postgres` default → `memory`).

### 2.5 `docker-compose.yml` — `ash-postgres` under `local-db` profile

```yaml
ash-postgres:
  image: postgres:16-alpine
  profiles: ["local-db"]
  environment:
    - POSTGRES_USER=${POSTGRES_USER:-ash}
    - POSTGRES_PASSWORD=${POSTGRES_PASSWORD:-ash}
    - POSTGRES_DB=${POSTGRES_DB:-ashcode}
  volumes:
    - ash-postgres-data:/var/lib/postgresql/data
  healthcheck:
    test: ["CMD-SHELL", "pg_isready -U ${POSTGRES_USER:-ash} ..."]
    ...
  ports:
    - "${POSTGRES_HOST_PORT:-5432}:5432"
```

- **`profiles: ["local-db"]`** — service is opt-in. `docker compose
  up -d ash-code` does NOT start postgres; users on managed databases
  just leave the profile off.
- `ash-code.depends_on.ash-postgres.required: false` makes the
  dependency soft so external-DB workflows do not get blocked.
- Host port 5432 published for `psql` / integration tests.
- Named volume `ash-postgres-data` for durability.

`ash-code` service environment gained:
```yaml
- ASH_SESSION_STORE=${ASH_SESSION_STORE:-postgres}
- ASH_POSTGRES_URL=${ASH_POSTGRES_URL:-postgres://ash:ash@ash-postgres:5432/ashcode}
- ASH_POSTGRES_POOL_SIZE=${ASH_POSTGRES_POOL_SIZE:-8}
```

`.env.example` documents every new knob, including the override path
for external managed DBs.

### 2.6 `docs/persistence.md`

End-user guide: backends, two deployment modes (local-db profile vs
external managed), schema explanation, operational recipes (backup /
restore / inspect / wipe), in-memory escape hatch, integration test
runbook, known M9.1 limits.

## 3. Verification

### 3.1 Test results

| Suite | Result |
|---|---|
| Rust `cargo test --workspace` (no postgres env) | **all green** — postgres integration tests SKIP cleanly with a warning |
| Rust `cargo test --workspace` (with `ASH_TEST_POSTGRES_URL`) | **all green** — 9 ash-core tests including 4 real-postgres CRUD + conformance |
| Python `pytest` | **113 passed** (untouched — M9.1 is Rust-only) |
| Docker image rebuild | ✅ |

`ash-core` test breakdown when postgres is reachable:
- 2 base (`crate_name_is_stable`, `version_is_non_empty`)
- 3 memory (`memory_store_passes_conformance`, `delete_missing_returns_false`, `put_overwrite_keeps_single_entry`)
- 4 postgres (`postgres_store_passes_conformance`, `ensure_schema_is_idempotent`, `invalid_url_fails_fast`, `jsonb_round_trip_preserves_messages`)
- = **9 passed**

### 3.2 End-to-end persistence proof

```bash
$ docker compose --profile local-db up -d ash-postgres ash-code
$ docker logs ash-code | grep -E "(session|QueryHost)"
[ash] session store ready
[ash] QueryHost gRPC listening on 127.0.0.1:50052

$ curl -s -N -X POST http://localhost:8080/v1/chat \
    -d '{"session_id":"persist-m9-1",
         "prompt":"Say exactly: hello persistence",
         "provider":"anthropic","model":"claude-opus-4-5"}'
event: text
data: {"type": "text", "text": "hello"}

event: text
data: {"type": "text", "text": " persistence"}

event: outcome
data: {"type": "outcome", "stop_reason": "end_turn", "turns_taken": 1}

$ docker compose restart ash-code   # postgres stays up
$ curl -s http://localhost:8080/v1/sessions/persist-m9-1
{
  "summary": {
    "id": "persist-m9-1",
    "provider": "anthropic",
    "model": "claude-opus-4-5",
    "message_count": 2
  },
  "messages": [
    {"role":"user","content":"Say exactly: hello persistence",...},
    {"role":"assistant","content":"hello persistence",...}
  ]
}

$ docker exec ash-postgres psql -U ash -d ashcode -c \
    "SELECT id, provider, model, jsonb_array_length(messages) FROM sessions;"
      id      | provider  |      model      | jsonb_array_length
--------------+-----------+-----------------+--------------------
 persist-m9-1 | anthropic | claude-opus-4-5 |                  2
```

The container restart drops every in-memory state. The session
re-emerges from PostgreSQL when the next request hits the DB. ★

## 4. Issues encountered and resolved

1. **Initial design used `testcontainers-rs`** — would have required
   docker-in-docker inside the rust build container. User correctly
   pointed out this is unnecessary when compose already provides a
   real postgres service. Rewrote the integration tests to consume
   `ASH_TEST_POSTGRES_URL` and skip cleanly when unset. Removed
   testcontainers from `Cargo.toml`.
2. **`pg_advisory_xact_lock` introduced after race**. Two parallel
   test workers hit `CREATE INDEX IF NOT EXISTS` simultaneously on a
   fresh DB and got `ERROR: duplicate key value violates unique
   constraint "pg_type_typname_nsp_index"`. Fixed by wrapping the
   schema bootstrap in a transaction with an advisory lock.
3. **`TRUNCATE sessions` between tests** broke parallel test
   isolation in a different way — one test would truncate while
   another was still inserting. Replaced with **per-test unique row
   ids** (`format!("conf-{now}-alpha")`) and removed the TRUNCATE.
   Cleaner: tests own only their own ids and clean them up at the
   end. Other rows in the same DB are ignored.
4. **`depends_on.ash-postgres.required: false`** — needed because
   the local-db profile is opt-in. Without `required: false`,
   omitting the profile would block ash-code from starting with a
   "service not found" error.
5. **Test ordering** — original assertion `assert_eq!(listed.len(),
   2)` failed because the conformance test shares a DB with other
   tests. Replaced with `filter(|s| owned ids).len() == 2` so the
   helper only asserts on the records it owns.

## 5. Decisions carried forward

1. **PostgreSQL is the production backend.** Memory mode is for tests
   and minimal demos only. The default `ASH_SESSION_STORE=postgres`
   in compose makes this explicit.
2. **`ASH_POSTGRES_URL` is the single source of truth.** The compose
   `ash-postgres` service is just one possible value; managed DBs
   work the same way.
3. **Local dev DB is `--profile local-db` opt-in.** External-DB
   deployments do not pay for a service they will not use.
4. **JSONB messages, single table.** Kept simple in M9.1. Normalize
   later if a real workload demands it.
5. **Advisory-locked schema bootstrap.** Future migrations should
   use the same lock id (or an evolution of it) to stay race-free.
6. **Tests are SKIP-not-fail when no DB env is set.** Keeps `cargo
   test` green for everyone, while still making it trivial to run
   real verification when needed.

## 6. Exit criteria — met

- [x] `crates/core/src/storage/{mod, memory, postgres}.rs`
- [x] `SessionStore` trait + `MemoryStore` + `PostgresStore`
- [x] `QueryHostService` consumes `Arc<dyn SessionStore>`
- [x] `ash_api::serve` calls `storage::build_default()` at startup
- [x] `docker-compose.yml` has `ash-postgres` under `local-db` profile
- [x] `.env.example` documents all new env vars
- [x] Connection retry on startup
- [x] Idempotent schema bootstrap with advisory lock
- [x] Memory + postgres conformance tests
- [x] **E2E**: container restart preserves session
- [x] `docs/persistence.md`
- [x] `docs/task/M9_1_TASK_REPORT.md`

## 7. Changed files

**Added**
- `crates/core/src/storage/mod.rs`
- `crates/core/src/storage/memory.rs`
- `crates/core/src/storage/postgres.rs`
- `docs/persistence.md`
- `docs/task/M9_1_TASK_REPORT.md`

**Modified**
- `Cargo.toml` — workspace deps `tokio-postgres`, `deadpool-postgres`,
  `chrono`
- `crates/core/Cargo.toml` — full dep set
- `crates/core/src/lib.rs` — re-export `storage` module
- `crates/api/Cargo.toml` — already depends on ash-core, no edit
- `crates/api/src/lib.rs` — `Arc<dyn SessionStore>` everywhere,
  `record_to_session` / `session_to_record` helpers,
  `serve(...)` uses `storage::build_default()`
- `docker-compose.yml` — `ash-postgres` service, env additions, soft
  dependency
- `.env.example` — documented all new knobs

## 8. Next: M9.2 — Cancel HTTP/gRPC

- New `QueryHost.CancelTurn` RPC
- New `POST /v1/sessions/{id}/cancel` HTTP endpoint
- `Arc<RwLock<HashMap<String, CancellationToken>>>` per active turn
- Q2 (M9 brief) decision: same-session new RunTurn auto-cancels the
  previous one
- Tests for both gRPC and HTTP cancel paths
