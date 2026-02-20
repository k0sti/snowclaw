//! Nostr-backed memory backend.
//!
//! Implements the [`Memory`] trait using a local JSON file store
//! (`nostr_memory.json`) for per-key memory persistence. This backend
//! is designed for Nostr-native agents that want a simple, file-based
//! memory tied to their workspace.
//!
//! Future: relay-backed persistence via NIP-78 kind 30078 events.

use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use super::traits::{Memory, MemoryCategory, MemoryEntry};

/// In-memory store persisted as JSON.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct NostrMemoryStore {
    entries: HashMap<String, MemoryEntry>,
}

/// Nostr memory backend implementing the [`Memory`] trait.
///
/// Stores entries in a JSON file in the workspace directory.
/// Suitable for small-to-medium memory sizes typical of agent workloads.
pub struct NostrMemory {
    store: Arc<RwLock<NostrMemoryStore>>,
    persist_path: PathBuf,
}

impl NostrMemory {
    pub fn new(workspace_dir: &Path) -> Self {
        let persist_path = workspace_dir.join("nostr_agent_memory.json");
        let store = if persist_path.exists() {
            match std::fs::read_to_string(&persist_path) {
                Ok(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
                    warn!("Failed to parse nostr_agent_memory.json: {e}; starting fresh");
                    NostrMemoryStore::default()
                }),
                Err(e) => {
                    warn!("Failed to read nostr_agent_memory.json: {e}; starting fresh");
                    NostrMemoryStore::default()
                }
            }
        } else {
            NostrMemoryStore::default()
        };

        let count = store.entries.len();
        if count > 0 {
            debug!("Loaded nostr agent memory: {count} entries");
        }

        Self {
            store: Arc::new(RwLock::new(store)),
            persist_path,
        }
    }

    /// Persist to disk.
    async fn flush(&self) -> anyhow::Result<()> {
        let store = self.store.read().await;
        let json = serde_json::to_string_pretty(&*store)?;
        drop(store);

        if let Some(parent) = self.persist_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.persist_path, json)?;
        Ok(())
    }
}

#[async_trait]
impl Memory for NostrMemory {
    fn name(&self) -> &str {
        "nostr"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let entry = MemoryEntry {
            id: format!("nostr-{}", chrono::Utc::now().timestamp_millis()),
            key: key.to_string(),
            content: content.to_string(),
            category,
            timestamp: now,
            session_id: session_id.map(String::from),
            score: None,
        };

        {
            let mut store = self.store.write().await;
            store.entries.insert(key.to_string(), entry);
        }

        self.flush().await?;
        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let store = self.store.read().await;
        let query_lower = query.to_lowercase();

        let mut results: Vec<MemoryEntry> = store
            .entries
            .values()
            .filter(|e| {
                if let Some(sid) = session_id {
                    if e.session_id.as_deref() != Some(sid) {
                        return false;
                    }
                }
                e.key.to_lowercase().contains(&query_lower)
                    || e.content.to_lowercase().contains(&query_lower)
            })
            .cloned()
            .collect();

        // Sort by timestamp descending (most recent first)
        results.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        results.truncate(limit);

        Ok(results)
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let store = self.store.read().await;
        Ok(store.entries.get(key).cloned())
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let store = self.store.read().await;
        let entries: Vec<MemoryEntry> = store
            .entries
            .values()
            .filter(|e| {
                if let Some(cat) = category {
                    if &e.category != cat {
                        return false;
                    }
                }
                if let Some(sid) = session_id {
                    if e.session_id.as_deref() != Some(sid) {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();
        Ok(entries)
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        let removed = {
            let mut store = self.store.write().await;
            store.entries.remove(key).is_some()
        };
        if removed {
            self.flush().await?;
        }
        Ok(removed)
    }

    async fn count(&self) -> anyhow::Result<usize> {
        let store = self.store.read().await;
        Ok(store.entries.len())
    }

    async fn health_check(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn store_and_recall() {
        let tmp = TempDir::new().unwrap();
        let mem = NostrMemory::new(tmp.path());

        mem.store("lang", "Rust is fast", MemoryCategory::Core, None)
            .await
            .unwrap();

        let results = mem.recall("rust", 10, None).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "lang");
        assert!(results[0].content.contains("Rust"));
    }

    #[tokio::test]
    async fn get_and_forget() {
        let tmp = TempDir::new().unwrap();
        let mem = NostrMemory::new(tmp.path());

        mem.store("key1", "value1", MemoryCategory::Core, None)
            .await
            .unwrap();

        let entry = mem.get("key1").await.unwrap();
        assert!(entry.is_some());

        let removed = mem.forget("key1").await.unwrap();
        assert!(removed);

        let entry = mem.get("key1").await.unwrap();
        assert!(entry.is_none());
    }

    #[tokio::test]
    async fn count_and_list() {
        let tmp = TempDir::new().unwrap();
        let mem = NostrMemory::new(tmp.path());

        mem.store("a", "aa", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b", "bb", MemoryCategory::Daily, None)
            .await
            .unwrap();

        assert_eq!(mem.count().await.unwrap(), 2);

        let core = mem.list(Some(&MemoryCategory::Core), None).await.unwrap();
        assert_eq!(core.len(), 1);
    }

    #[tokio::test]
    async fn persistence_roundtrip() {
        let tmp = TempDir::new().unwrap();

        {
            let mem = NostrMemory::new(tmp.path());
            mem.store("persist", "data", MemoryCategory::Core, None)
                .await
                .unwrap();
        }

        let mem2 = NostrMemory::new(tmp.path());
        let entry = mem2.get("persist").await.unwrap();
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().content, "data");
    }

    #[tokio::test]
    async fn name_is_nostr() {
        let tmp = TempDir::new().unwrap();
        let mem = NostrMemory::new(tmp.path());
        assert_eq!(mem.name(), "nostr");
    }

    #[tokio::test]
    async fn health_check_is_true() {
        let tmp = TempDir::new().unwrap();
        let mem = NostrMemory::new(tmp.path());
        assert!(mem.health_check().await);
    }
}
