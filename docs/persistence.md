# ash-code Session Persistence

Sessions (full transcript: user prompts, assistant responses, tool
calls and results) are persisted to a PostgreSQL database. ash-code
itself is stateless across restarts — the data lives in the DB.

## Backends

| `ASH_SESSION_STORE` | Use case |
|---|---|
| `postgres` (default) | Production and dev. All session writes go through `tokio-postgres`. |
| `memory` | Tests, ephemeral demos, no external DB available. State is lost when the container exits. |

The backend is selected once at boot from the `ASH_SESSION_STORE`
environment variable. Switching it requires a container restart.

## Connecting to PostgreSQL

The connection URL is read from `ASH_POSTGRES_URL`. Anything that
`tokio_postgres::Config::from_str` accepts works:

```
postgres://user:password@host:5432/dbname
postgresql://user@host/dbname?sslmode=require
host=db user=ash password=ash dbname=ashcode
```

`ASH_POSTGRES_POOL_SIZE` controls the deadpool max connections (default 8).

## Two deployment modes

### A) Local dev — bundled `ash-postgres` service

The compose stack ships a Postgres 16 service under the **`local-db`
profile**. It is *not* started by default — you opt in:

```bash
docker compose --profile local-db up -d
```

This brings up:

- `ash-postgres` (postgres:16-alpine, healthchecked, named volume
  `ash-postgres-data`, host port 5432 published for tools like `psql`)
- `ash-code` (waits for ash-postgres health)

Default credentials in `.env.example`:

```
POSTGRES_USER=ash
POSTGRES_PASSWORD=ash
POSTGRES_DB=ashcode
POSTGRES_HOST_PORT=5432
ASH_POSTGRES_URL=postgres://ash:ash@ash-postgres:5432/ashcode
```

The `ASH_POSTGRES_URL` default uses the **service name** `ash-postgres`,
which works because both containers join the same compose network.

### B) Production / external managed Postgres

Point `ASH_POSTGRES_URL` at your managed database (RDS, Supabase,
Cloud SQL, Aiven, …) and **do not start the local-db profile**:

```bash
# .env
ASH_POSTGRES_URL=postgres://ash:hunter2@db.acme.internal:5432/ashcode

# bring up only ash-code
docker compose up -d ash-code
```

ash-code does not care where the database lives. Every backend goes
through the same `tokio-postgres` driver and the same schema. The
`ash-postgres` compose service is purely a developer convenience, not
a hard dependency. `depends_on.ash-postgres.required: false` makes
the dependency soft.

## Schema

Created on first boot via `PostgresStore::ensure_schema()`. Wrapped
in a Postgres advisory lock so multiple ash-code processes (or
parallel test runners) cannot race each other into a duplicate
pg_type error:

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

`messages` is the entire transcript serialized as JSONB. M9.1 keeps
the schema deliberately flat. Future milestones may normalize into a
separate `messages` table when search or per-message updates become
useful.

### Why `BIGINT` epoch ms instead of `TIMESTAMPTZ`?

`tokio-postgres` round-trips `i64` cleanly without forcing every
consumer crate to enable the `chrono` type extras. Display layers can
format ms-since-epoch into whatever timezone they need.

## Operational recipes

### Connect from your host
```bash
docker compose --profile local-db up -d ash-postgres
psql -h localhost -p 5432 -U ash -d ashcode
```

### Inspect a session
```sql
SELECT id, provider, model, jsonb_array_length(messages) AS turns,
       to_timestamp(updated_at_ms / 1000) AS updated
FROM sessions
ORDER BY updated_at_ms DESC
LIMIT 20;

SELECT messages FROM sessions WHERE id = 'persist-m9-1';
```

### Wipe everything
```bash
docker compose --profile local-db down -v        # deletes the volume
docker compose --profile local-db up -d          # fresh DB
```

Or, without dropping the volume:
```sql
TRUNCATE sessions;
```

### Backup
```bash
docker exec ash-postgres pg_dump -U ash -Fc ashcode > sessions.dump
```

### Restore
```bash
cat sessions.dump | docker exec -i ash-postgres pg_restore -U ash -d ashcode --clean --if-exists
```

## In-memory mode

When you do not want a database at all (CI smoke runs, quick demos,
proving that ash-code boots clean):

```bash
ASH_SESSION_STORE=memory docker compose up -d ash-code
```

Behavior matches PostgreSQL semantically (same `SessionStore` trait),
but everything dies when the container stops.

## Integration tests

`crates/core/src/storage/postgres.rs` ships four integration tests
that run against a real PostgreSQL when `ASH_TEST_POSTGRES_URL` is set:

- `postgres_store_passes_conformance` — full CRUD round-trip via the
  shared `conformance::run_full_suite` helper (also used by
  `MemoryStore`).
- `ensure_schema_is_idempotent`
- `invalid_url_fails_fast`
- `jsonb_round_trip_preserves_messages`

When `ASH_TEST_POSTGRES_URL` is **unset**, the postgres tests print a
`SKIP` line and pass. This keeps `cargo test` green on machines
without Postgres while CI / dev workflows do real verification.

Run them locally:

```bash
docker compose --profile local-db up -d ash-postgres
docker run --rm -v "$PWD":/w -w /w \
  --network host \
  -e ASH_TEST_POSTGRES_URL=postgres://ash:ash@localhost:5432/ashcode \
  rust:1.88-slim-bookworm bash -c "
    apt-get update -qq && apt-get install -y -qq protobuf-compiler >/dev/null
    cargo test -p ash-core
  "
```

## Known limits (M9.1)

- **No migrations framework yet.** `ensure_schema` is `CREATE … IF
  NOT EXISTS`. Schema evolution will be added when there is an
  actual second migration to run.
- **Single-table layout.** Long sessions store the whole transcript
  in one JSONB blob. Read/write of huge sessions (> ~10k messages)
  is proportional to the whole blob.
- **No multi-tenant scoping.** All sessions live in one table with no
  user / org column. ash-code's threat model is "single-developer
  harness on localhost"; multi-tenant deployment is M9.4 security
  work.
- **Per-turn write-through.** A turn that crashes mid-stream commits
  the user message but not the partial assistant response. Partial
  state would need a different schema and is deferred.
