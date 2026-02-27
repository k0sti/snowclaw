//! Collective memory backend — wraps `snow-memory` `SqliteMemoryIndex`
//! to implement the agent runtime `Memory` trait.
//!
//! This backend stores memories as `snow_memory::Memory` entries in a
//! SQLite FTS5 index and ranks recall results using source trust and
//! model-tier weighting from the configured `CollectiveMemoryConfig`.

use super::traits::{Memory, MemoryCategory, MemoryEntry};
use crate::config::snowclaw_schema::CollectiveMemoryConfig;
use async_trait::async_trait;
use parking_lot::Mutex;
use snow_memory::SqliteMemoryIndex;
use snow_memory::types::{Memory as SnowMemory, MemoryTier};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Collective memory backend backed by `snow-memory` `SqliteMemoryIndex`.
pub struct CollectiveMemory {
    index: Mutex<SqliteMemoryIndex>,
    config: CollectiveMemoryConfig,
    #[allow(dead_code)]
    db_path: PathBuf,
}

impl CollectiveMemory {
    /// Create a new collective memory backend.
    ///
    /// `workspace_dir` is used to resolve relative `db_path` values from config.
    pub fn new(
        workspace_dir: &Path,
        config: &CollectiveMemoryConfig,
    ) -> anyhow::Result<Self> {
        let db_path = if Path::new(&config.db_path).is_absolute() {
            PathBuf::from(&config.db_path)
        } else {
            workspace_dir.join(&config.db_path)
        };

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let index = SqliteMemoryIndex::open(&db_path)
            .map_err(|e| anyhow::anyhow!("failed to open collective memory DB: {e}"))?;

        Ok(Self {
            index: Mutex::new(index),
            config: config.clone(),
            db_path,
        })
    }

    /// Create with an in-memory database (for testing).
    pub fn new_in_memory(config: &CollectiveMemoryConfig) -> anyhow::Result<Self> {
        let index = SqliteMemoryIndex::open_in_memory()
            .map_err(|e| anyhow::anyhow!("failed to open in-memory collective DB: {e}"))?;

        Ok(Self {
            index: Mutex::new(index),
            config: config.clone(),
            db_path: PathBuf::from(":memory:"),
        })
    }
}

/// Convert a `MemoryCategory` to a `MemoryTier`.
fn category_to_tier(category: &MemoryCategory) -> MemoryTier {
    match category {
        MemoryCategory::Core => MemoryTier::Public,
        MemoryCategory::Daily => MemoryTier::Public,
        MemoryCategory::Conversation => MemoryTier::Private("self".to_string()),
        MemoryCategory::Custom(name) if name == "group" => {
            MemoryTier::Group("default".to_string())
        }
        MemoryCategory::Custom(_) => MemoryTier::Public,
    }
}

/// Convert a `MemoryTier` back to a `MemoryCategory`.
fn tier_to_category(tier: &MemoryTier) -> MemoryCategory {
    match tier {
        MemoryTier::Public => MemoryCategory::Core,
        MemoryTier::Group(id) => MemoryCategory::Custom(format!("group:{id}")),
        MemoryTier::Private(_) => MemoryCategory::Conversation,
    }
}

/// Convert a `SnowMemory` to a runtime `MemoryEntry`.
fn snow_to_entry(m: &SnowMemory, score: Option<f64>) -> MemoryEntry {
    MemoryEntry {
        id: m.id.clone(),
        key: m.topic.clone(),
        content: if m.detail.is_empty() {
            m.summary.clone()
        } else {
            format!("{}\n\n{}", m.summary, m.detail)
        },
        category: tier_to_category(&m.tier),
        timestamp: chrono::DateTime::from_timestamp(m.created_at as i64, 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default(),
        session_id: None,
        score,
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[async_trait]
impl Memory for CollectiveMemory {
    fn name(&self) -> &str {
        "collective"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        _session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let id = Uuid::new_v4().to_string();
        let (summary, detail) = match content.split_once("\n\n") {
            Some((s, d)) => (s.to_string(), d.to_string()),
            None => (content.to_string(), String::new()),
        };

        let memory = SnowMemory {
            id,
            tier: category_to_tier(&category),
            topic: key.to_string(),
            summary,
            detail,
            context: None,
            source: "self".to_string(),
            model: String::new(),
            confidence: 0.8,
            supersedes: None,
            version: 1,
            tags: vec![],
            created_at: now_unix(),
        };

        let idx = self.index.lock();
        idx.upsert(&memory, None)
            .map_err(|e| anyhow::anyhow!("collective store failed: {e}"))?;
        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        _session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let sm_config = self.config.to_snow_memory_config();
        let idx = self.index.lock();
        let results = idx
            .ranked_search(query, None, &sm_config, limit)
            .map_err(|e| anyhow::anyhow!("collective recall failed: {e}"))?;

        Ok(results
            .iter()
            .map(|r| snow_to_entry(&r.memory, Some(r.effective_score)))
            .collect())
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let idx = self.index.lock();
        let result = idx
            .get(key)
            .map_err(|e| anyhow::anyhow!("collective get failed: {e}"))?;
        Ok(result.as_ref().map(|m| snow_to_entry(m, None)))
    }

    async fn list(
        &self,
        _category: Option<&MemoryCategory>,
        _session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        // SqliteMemoryIndex doesn't have a list-all method.
        // Use a wildcard FTS search to return recent entries.
        let idx = self.index.lock();
        // FTS5 doesn't support listing all; return empty for now.
        // Callers typically use recall() for search-based access.
        let _ = idx;
        Ok(vec![])
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        // SqliteMemoryIndex doesn't expose delete-by-id directly.
        // We check if the key exists as an ID first.
        let idx = self.index.lock();
        let exists = idx
            .get(key)
            .map_err(|e| anyhow::anyhow!("collective forget lookup failed: {e}"))?
            .is_some();
        if !exists {
            return Ok(false);
        }
        // Direct SQL delete through the connection is not exposed.
        // For now, return false indicating deletion is not supported.
        // TODO: expose delete in SqliteMemoryIndex upstream.
        Ok(false)
    }

    async fn count(&self) -> anyhow::Result<usize> {
        let idx = self.index.lock();
        idx.count()
            .map_err(|e| anyhow::anyhow!("collective count failed: {e}"))
    }

    async fn health_check(&self) -> bool {
        self.index.lock().count().is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> CollectiveMemoryConfig {
        CollectiveMemoryConfig::default()
    }

    #[tokio::test]
    async fn store_and_count() {
        let mem = CollectiveMemory::new_in_memory(&test_config()).unwrap();
        assert_eq!(mem.count().await.unwrap(), 0);

        mem.store("rust/errors", "How to handle errors in Rust", MemoryCategory::Core, None)
            .await
            .unwrap();
        assert_eq!(mem.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn store_and_recall() {
        let mut cfg = test_config();
        // Add self as a trusted source so ranking doesn't zero out
        cfg.source_preferences.push(
            crate::config::snowclaw_schema::CollectiveSourceEntry {
                npub: Some("self".to_string()),
                group: None,
                trust: 1.0,
            },
        );

        let mem = CollectiveMemory::new_in_memory(&cfg).unwrap();

        mem.store(
            "rust/errors",
            "Error handling with Result and Option types",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();

        mem.store(
            "nostr/nip44",
            "NIP-44 encryption for private messages",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();

        let results = mem.recall("error handling", 10, None).await.unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].key, "rust/errors");
        assert!(results[0].score.unwrap() > 0.0);
    }

    #[tokio::test]
    async fn health_check_passes() {
        let mem = CollectiveMemory::new_in_memory(&test_config()).unwrap();
        assert!(mem.health_check().await);
    }

    #[tokio::test]
    async fn name_returns_collective() {
        let mem = CollectiveMemory::new_in_memory(&test_config()).unwrap();
        assert_eq!(mem.name(), "collective");
    }
}
