//! In-memory session store. Used for tests and the `ASH_SESSION_STORE=memory`
//! escape hatch when there is no PostgreSQL handy.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use super::{SessionRecord, SessionStore, SessionSummary};

#[derive(Default)]
pub struct MemoryStore {
    inner: Mutex<HashMap<String, SessionRecord>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SessionStore for MemoryStore {
    async fn ensure_schema(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn get(&self, id: &str) -> anyhow::Result<Option<SessionRecord>> {
        Ok(self.inner.lock().unwrap().get(id).cloned())
    }

    async fn put(&self, record: &SessionRecord) -> anyhow::Result<()> {
        self.inner
            .lock()
            .unwrap()
            .insert(record.id.clone(), record.clone());
        Ok(())
    }

    async fn delete(&self, id: &str) -> anyhow::Result<bool> {
        Ok(self.inner.lock().unwrap().remove(id).is_some())
    }

    async fn list(&self) -> anyhow::Result<Vec<SessionSummary>> {
        let mut entries: Vec<SessionSummary> = self
            .inner
            .lock()
            .unwrap()
            .values()
            .map(|r| SessionSummary {
                id: r.id.clone(),
                provider: r.provider.clone(),
                model: r.model.clone(),
                message_count: r.messages.len() as i32,
                updated_at_ms: r.updated_at_ms,
            })
            .collect();
        entries.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms));
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::conformance;
    use std::sync::Arc;

    #[tokio::test]
    async fn memory_store_passes_conformance() {
        let store: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());
        conformance::run_full_suite(store).await;
    }

    #[tokio::test]
    async fn delete_missing_returns_false() {
        let store = MemoryStore::new();
        store.ensure_schema().await.unwrap();
        assert!(!store.delete("nothing").await.unwrap());
    }

    #[tokio::test]
    async fn put_overwrite_keeps_single_entry() {
        let store = MemoryStore::new();
        let now = SessionRecord::now_ms();
        let a = SessionRecord {
            id: "x".to_string(),
            provider: "p".to_string(),
            model: "m".to_string(),
            created_at_ms: now,
            updated_at_ms: now,
            messages: vec![],
        };
        store.put(&a).await.unwrap();
        store.put(&a).await.unwrap();
        assert_eq!(store.list().await.unwrap().len(), 1);
    }
}
