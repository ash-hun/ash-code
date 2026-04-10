//! PostgreSQL session store backed by `tokio-postgres` + `deadpool-postgres`.
//!
//! Schema (idempotent, created at startup):
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS sessions (
//!     id            TEXT PRIMARY KEY,
//!     provider      TEXT NOT NULL,
//!     model         TEXT NOT NULL,
//!     created_at_ms BIGINT NOT NULL,
//!     updated_at_ms BIGINT NOT NULL,
//!     messages      JSONB NOT NULL
//! );
//! CREATE INDEX IF NOT EXISTS idx_sessions_updated_at_ms
//!     ON sessions (updated_at_ms DESC);
//! ```
//!
//! The `ASH_POSTGRES_URL` is parsed by `tokio_postgres::Config::from_str`,
//! so any valid libpq connection string works:
//! `postgres://user:pass@host:5432/dbname`,
//! `postgresql://...`, key/value form, etc. External managed databases
//! (RDS, Supabase, Cloud SQL, …) are first-class — the dev compose
//! service `ash-postgres` is just one possible backend.

use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use deadpool_postgres::{Config as PoolConfig, ManagerConfig, Pool, RecyclingMethod, Runtime};
use tokio_postgres::{Config as PgConfig, NoTls};

use super::{SessionRecord, SessionStore, SessionSummary, StoredMessage};

const SCHEMA_SQL: &str = r#"
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
"#;

pub struct PostgresStore {
    pool: Pool,
}

impl PostgresStore {
    /// Build a new store from a libpq-style URL.
    pub fn new(url: &str, pool_size: usize) -> anyhow::Result<Self> {
        let pg_config = PgConfig::from_str(url)
            .map_err(|e| anyhow::anyhow!("invalid ASH_POSTGRES_URL: {e}"))?;

        let mut cfg = PoolConfig::new();
        cfg.dbname = pg_config.get_dbname().map(|s| s.to_string());
        cfg.user = pg_config.get_user().map(|s| s.to_string());
        cfg.password = pg_config
            .get_password()
            .and_then(|p| std::str::from_utf8(p).ok().map(|s| s.to_string()));
        if let Some(host) = pg_config.get_hosts().first() {
            cfg.host = match host {
                tokio_postgres::config::Host::Tcp(h) => Some(h.clone()),
                _ => None,
            };
        }
        if let Some(port) = pg_config.get_ports().first() {
            cfg.port = Some(*port);
        }
        cfg.manager = Some(ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        });
        cfg.pool = Some(deadpool_postgres::PoolConfig::new(pool_size));

        let pool = cfg
            .create_pool(Some(Runtime::Tokio1), NoTls)
            .map_err(|e| anyhow::anyhow!("failed to build pg pool: {e}"))?;
        Ok(Self { pool })
    }

    /// Connect with retry — useful at boot when postgres is still
    /// starting up alongside ash-code in the same compose stack.
    pub async fn connect_with_retry(
        url: &str,
        pool_size: usize,
        attempts: usize,
    ) -> anyhow::Result<Self> {
        let store = Self::new(url, pool_size)?;
        let mut last_err: Option<anyhow::Error> = None;
        for i in 0..attempts {
            match store.pool.get().await {
                Ok(_) => {
                    store.ensure_schema().await?;
                    tracing::info!("postgres connected on attempt {}", i + 1);
                    return Ok(store);
                }
                Err(err) => {
                    tracing::warn!(
                        "postgres connect attempt {}/{} failed: {err:#}",
                        i + 1,
                        attempts
                    );
                    last_err = Some(anyhow::anyhow!("{err}"));
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("postgres unreachable")))
    }
}

#[async_trait]
impl SessionStore for PostgresStore {
    async fn ensure_schema(&self) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        // Wrap the bootstrap in a transaction-scoped advisory lock so
        // concurrent processes (parallel test runners, multi-replica
        // deployments) cannot race each other into a duplicate pg_type
        // entry while CREATE TABLE IF NOT EXISTS is running for the
        // first time. The lock id is an arbitrary stable constant.
        client
            .batch_execute(
                "BEGIN; \
                 SELECT pg_advisory_xact_lock(8923457501); \
                 CREATE TABLE IF NOT EXISTS sessions ( \
                     id            TEXT PRIMARY KEY, \
                     provider      TEXT NOT NULL, \
                     model         TEXT NOT NULL, \
                     created_at_ms BIGINT NOT NULL, \
                     updated_at_ms BIGINT NOT NULL, \
                     messages      JSONB NOT NULL \
                 ); \
                 CREATE INDEX IF NOT EXISTS idx_sessions_updated_at_ms \
                     ON sessions (updated_at_ms DESC); \
                 COMMIT;"
            )
            .await?;
        Ok(())
    }

    async fn get(&self, id: &str) -> anyhow::Result<Option<SessionRecord>> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, provider, model, created_at_ms, updated_at_ms, messages \
                 FROM sessions WHERE id = $1",
                &[&id],
            )
            .await?;
        let Some(row) = row else { return Ok(None) };
        let messages_json: serde_json::Value = row.get(5);
        let messages: Vec<StoredMessage> = serde_json::from_value(messages_json)?;
        Ok(Some(SessionRecord {
            id: row.get(0),
            provider: row.get(1),
            model: row.get(2),
            created_at_ms: row.get(3),
            updated_at_ms: row.get(4),
            messages,
        }))
    }

    async fn put(&self, record: &SessionRecord) -> anyhow::Result<()> {
        let client = self.pool.get().await?;
        let messages_json = serde_json::to_value(&record.messages)?;
        client
            .execute(
                "INSERT INTO sessions (id, provider, model, created_at_ms, updated_at_ms, messages) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (id) DO UPDATE SET \
                    provider      = EXCLUDED.provider, \
                    model         = EXCLUDED.model, \
                    updated_at_ms = EXCLUDED.updated_at_ms, \
                    messages      = EXCLUDED.messages",
                &[
                    &record.id,
                    &record.provider,
                    &record.model,
                    &record.created_at_ms,
                    &record.updated_at_ms,
                    &messages_json,
                ],
            )
            .await?;
        Ok(())
    }

    async fn delete(&self, id: &str) -> anyhow::Result<bool> {
        let client = self.pool.get().await?;
        let n = client.execute("DELETE FROM sessions WHERE id = $1", &[&id]).await?;
        Ok(n > 0)
    }

    async fn list(&self) -> anyhow::Result<Vec<SessionSummary>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, provider, model, jsonb_array_length(messages), updated_at_ms \
                 FROM sessions ORDER BY updated_at_ms DESC",
                &[],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let count: i32 = row.get(3);
                SessionSummary {
                    id: row.get(0),
                    provider: row.get(1),
                    model: row.get(2),
                    message_count: count,
                    updated_at_ms: row.get(4),
                }
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Integration tests against a real PostgreSQL.
//
// These run against whatever URL is exported in `ASH_TEST_POSTGRES_URL`.
// The compose stack already provides one via the `ash-postgres` service:
//
//     docker compose --profile local-db up -d ash-postgres
//     ASH_TEST_POSTGRES_URL=postgres://ash:ash@localhost:5432/ashcode \
//         cargo test -p ash-core
//
// CI is expected to run against a service container or the same compose
// stack. When the env var is unset the tests are skipped (treated as a
// pass) so `cargo test` stays green on machines without postgres.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::conformance;
    use std::sync::Arc;

    fn test_url() -> Option<String> {
        std::env::var("ASH_TEST_POSTGRES_URL").ok()
    }

    fn skip_if_no_db() -> bool {
        if test_url().is_none() {
            eprintln!(
                "SKIP postgres test: set ASH_TEST_POSTGRES_URL to enable \
                 (compose: --profile local-db up -d ash-postgres)"
            );
            true
        } else {
            false
        }
    }

    async fn fresh_store() -> PostgresStore {
        let url = test_url().expect("ASH_TEST_POSTGRES_URL");
        // Each test isolates itself via unique row ids in the
        // conformance helper, so multiple tests can share the same
        // database without TRUNCATE'ing each other's data.
        PostgresStore::connect_with_retry(&url, 4, 10)
            .await
            .expect("connect to test postgres")
    }

    #[tokio::test]
    async fn postgres_store_passes_conformance() {
        if skip_if_no_db() {
            return;
        }
        let store = fresh_store().await;
        let store: Arc<dyn SessionStore> = Arc::new(store);
        conformance::run_full_suite(store).await;
    }

    #[tokio::test]
    async fn ensure_schema_is_idempotent() {
        if skip_if_no_db() {
            return;
        }
        let store = fresh_store().await;
        store.ensure_schema().await.unwrap();
        store.ensure_schema().await.unwrap();
        let now = SessionRecord::now_ms();
        let id = format!("idem-{now}");
        store
            .put(&SessionRecord {
                id: id.clone(),
                provider: "p".to_string(),
                model: "m".to_string(),
                created_at_ms: now,
                updated_at_ms: now,
                messages: vec![],
            })
            .await
            .unwrap();
        assert!(store.get(&id).await.unwrap().is_some());
        let _ = store.delete(&id).await;
    }

    #[tokio::test]
    async fn invalid_url_fails_fast() {
        let result = PostgresStore::new("not a real url", 2);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn jsonb_round_trip_preserves_messages() {
        if skip_if_no_db() {
            return;
        }
        let store = fresh_store().await;
        let now = SessionRecord::now_ms();
        let id = format!("msgs-{now}");
        let record = SessionRecord {
            id: id.clone(),
            provider: "anthropic".to_string(),
            model: "claude-opus-4-5".to_string(),
            created_at_ms: now,
            updated_at_ms: now,
            messages: vec![
                StoredMessage {
                    role: "user".to_string(),
                    content: "한글도 OK".to_string(),
                    tool_call_id: String::new(),
                },
                StoredMessage {
                    role: "assistant".to_string(),
                    content: r#"{"escaped":"json"}"#.to_string(),
                    tool_call_id: "tc-1".to_string(),
                },
            ],
        };
        store.put(&record).await.unwrap();
        let got = store.get(&id).await.unwrap().unwrap();
        assert_eq!(got, record);
        let _ = store.delete(&id).await;
    }
}
