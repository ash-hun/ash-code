//! Session storage abstraction.
//!
//! Two implementations ship in M9.1:
//!
//! * [`memory::MemoryStore`] — in-process `HashMap`. Used by tests and
//!   when `ASH_SESSION_STORE=memory` (no external DB needed).
//! * [`postgres::PostgresStore`] — production backend. Connects to any
//!   PostgreSQL server reachable via the `ASH_POSTGRES_URL` connection
//!   string. The compose stack ships an `ash-postgres` service for dev
//!   convenience under the `local-db` profile, but external managed
//!   databases (RDS, Supabase, Cloud SQL, …) work the same way — just
//!   point the env at them.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod memory;
pub mod postgres;

pub use memory::MemoryStore;
pub use postgres::PostgresStore;

/// Plain old data: one persisted message inside a session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub tool_call_id: String,
}

/// Plain old data: one persisted session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub messages: Vec<StoredMessage>,
}

impl SessionRecord {
    pub fn now_ms() -> i64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

/// Lightweight summary for `list` results.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub message_count: i32,
    pub updated_at_ms: i64,
}

/// The contract every storage backend implements.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Initialize storage: create tables, run migrations, etc. Idempotent.
    async fn ensure_schema(&self) -> anyhow::Result<()>;

    async fn get(&self, id: &str) -> anyhow::Result<Option<SessionRecord>>;
    async fn put(&self, record: &SessionRecord) -> anyhow::Result<()>;
    async fn delete(&self, id: &str) -> anyhow::Result<bool>;
    async fn list(&self) -> anyhow::Result<Vec<SessionSummary>>;
}

/// Pick a backend from the `ASH_SESSION_STORE` env var.
///
/// Order of resolution:
/// 1. `ASH_SESSION_STORE=memory` → in-process `MemoryStore`.
/// 2. `ASH_SESSION_STORE=postgres` (default) → `PostgresStore` connected
///    via `ASH_POSTGRES_URL`. The URL is required when this branch
///    runs; missing means an explicit error.
pub async fn build_default() -> anyhow::Result<Arc<dyn SessionStore>> {
    let kind = std::env::var("ASH_SESSION_STORE")
        .unwrap_or_else(|_| "postgres".to_string())
        .to_lowercase();
    match kind.as_str() {
        "memory" | "mem" => {
            tracing::info!("session store: memory");
            Ok(Arc::new(MemoryStore::new()))
        }
        "postgres" | "pg" => {
            let url = std::env::var("ASH_POSTGRES_URL").map_err(|_| {
                anyhow::anyhow!(
                    "ASH_SESSION_STORE=postgres requires ASH_POSTGRES_URL to be set"
                )
            })?;
            let pool_size = std::env::var("ASH_POSTGRES_POOL_SIZE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(8);
            tracing::info!("session store: postgres (pool={pool_size})");
            let store = PostgresStore::connect_with_retry(&url, pool_size, 10).await?;
            Ok(Arc::new(store))
        }
        other => Err(anyhow::anyhow!(
            "unknown ASH_SESSION_STORE value: {other:?}"
        )),
    }
}

/// Reusable conformance test helper.
///
/// Each backend test calls this with a fresh `Arc<dyn SessionStore>`.
/// Both `MemoryStore` and `PostgresStore` are validated against the
/// same expectations so behavior cannot drift between them.
///
/// Note: this helper avoids any "list size == N" assertion because
/// shared backends (the dev postgres reused across parallel tests)
/// would race. It only asserts the records it owns by id.
#[cfg(test)]
pub(crate) mod conformance {
    use super::*;

    pub async fn run_full_suite(store: Arc<dyn SessionStore>) {
        store.ensure_schema().await.unwrap();

        // Use a unique id prefix so the test does not collide with
        // anything else hitting the same backend in parallel.
        let prefix = format!("conf-{}", SessionRecord::now_ms());
        let id_a = format!("{prefix}-alpha");
        let id_b = format!("{prefix}-beta");

        // Make sure our slots are empty even if a previous run crashed.
        let _ = store.delete(&id_a).await;
        let _ = store.delete(&id_b).await;

        assert!(store.get("nope-prefix-no-such-key").await.unwrap().is_none());

        let now = SessionRecord::now_ms();
        let a = SessionRecord {
            id: id_a.clone(),
            provider: "anthropic".to_string(),
            model: "claude-opus-4-5".to_string(),
            created_at_ms: now,
            updated_at_ms: now,
            messages: vec![StoredMessage {
                role: "user".to_string(),
                content: "hello".to_string(),
                tool_call_id: String::new(),
            }],
        };
        let b = SessionRecord {
            id: id_b.clone(),
            provider: "openai".to_string(),
            model: "gpt-4.1-mini".to_string(),
            created_at_ms: now + 100,
            updated_at_ms: now + 200,
            messages: vec![],
        };
        store.put(&a).await.unwrap();
        store.put(&b).await.unwrap();

        // Round-trip get.
        let got = store.get(&id_a).await.unwrap().unwrap();
        assert_eq!(got, a);

        // Both records visible in the listing, beta sorts before alpha
        // by updated_at desc. We do not assert total length — other
        // tests may share the table.
        let listed = store.list().await.unwrap();
        let mut owned: Vec<_> = listed
            .into_iter()
            .filter(|s| s.id == id_a || s.id == id_b)
            .collect();
        owned.sort_by(|x, y| y.updated_at_ms.cmp(&x.updated_at_ms));
        assert_eq!(owned.len(), 2);
        assert_eq!(owned[0].id, id_b);
        assert_eq!(owned[1].id, id_a);
        assert_eq!(owned[0].message_count, 0);
        assert_eq!(owned[1].message_count, 1);

        // Update overwrites alpha so it becomes the most recent.
        let mut a2 = a.clone();
        a2.messages.push(StoredMessage {
            role: "assistant".to_string(),
            content: "hi back".to_string(),
            tool_call_id: String::new(),
        });
        a2.updated_at_ms = now + 500;
        store.put(&a2).await.unwrap();
        let listed = store.list().await.unwrap();
        let mut owned: Vec<_> = listed
            .into_iter()
            .filter(|s| s.id == id_a || s.id == id_b)
            .collect();
        owned.sort_by(|x, y| y.updated_at_ms.cmp(&x.updated_at_ms));
        assert_eq!(owned[0].id, id_a);
        assert_eq!(owned[0].message_count, 2);

        // Delete is idempotent semantics: true once, false after.
        assert!(store.delete(&id_a).await.unwrap());
        assert!(!store.delete(&id_a).await.unwrap());
        assert!(store.get(&id_a).await.unwrap().is_none());
        assert!(store.delete(&id_b).await.unwrap());
    }
}
