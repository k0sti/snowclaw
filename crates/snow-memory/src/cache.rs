//! TTL cache layer for remote relay memories.
//!
//! Wraps SqliteMemoryIndex with TTL-based invalidation and
//! deduplication via supersedes chains.

use crate::search::SqliteMemoryIndex;
use crate::types::Memory;
use rusqlite::Result as SqlResult;
use std::path::Path;

/// Memory cache with TTL eviction.
pub struct MemoryCache {
    index: SqliteMemoryIndex,
    /// Default TTL in seconds for cached memories.
    pub ttl_secs: u64,
}

impl MemoryCache {
    /// Open a cache backed by a SQLite file.
    pub fn open(path: &Path, ttl_secs: u64) -> SqlResult<Self> {
        let index = SqliteMemoryIndex::open(path)?;
        Ok(Self { index, ttl_secs })
    }

    /// Open an in-memory cache (for testing).
    pub fn open_in_memory(ttl_secs: u64) -> SqlResult<Self> {
        let index = SqliteMemoryIndex::open_in_memory()?;
        Ok(Self { index, ttl_secs })
    }

    /// Cache a memory from a relay event.
    /// If this memory supersedes an existing one, the old one is kept
    /// but the new one takes priority in search results.
    pub fn cache_memory(&self, memory: &Memory, event_json: Option<&str>) -> SqlResult<()> {
        self.index.upsert(memory, event_json)
    }

    /// Get a cached memory by ID.
    pub fn get(&self, id: &str) -> SqlResult<Option<Memory>> {
        self.index.get(id)
    }

    /// Search cached memories.
    pub fn search(
        &self,
        query: &str,
        tier_filter: Option<&str>,
        limit: usize,
    ) -> SqlResult<Vec<(Memory, f64)>> {
        self.index.search(query, tier_filter, limit)
    }

    /// Evict memories older than the configured TTL.
    pub fn evict_stale(&self) -> SqlResult<usize> {
        self.index.evict_stale(self.ttl_secs)
    }

    /// Get total cached memory count.
    pub fn count(&self) -> SqlResult<usize> {
        self.index.count()
    }

    /// Get a reference to the underlying index for advanced queries.
    pub fn index(&self) -> &SqliteMemoryIndex {
        &self.index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Memory, MemoryTier};

    fn make_memory(id: &str, topic: &str, summary: &str) -> Memory {
        Memory {
            id: id.to_string(),
            tier: MemoryTier::Public,
            topic: topic.to_string(),
            summary: summary.to_string(),
            detail: String::new(),
            context: None,
            source: "test".to_string(),
            model: "test/model".to_string(),
            confidence: 0.8,
            supersedes: None,
            version: 1,
            tags: vec![],
            created_at: 1700000000,
        }
    }

    #[test]
    fn test_cache_and_retrieve() {
        let cache = MemoryCache::open_in_memory(3600).unwrap();
        let m = make_memory("a1", "test/topic", "Test memory content");
        cache.cache_memory(&m, None).unwrap();

        let got = cache.get("a1").unwrap().unwrap();
        assert_eq!(got.summary, "Test memory content");
        assert_eq!(cache.count().unwrap(), 1);
    }

    #[test]
    fn test_cache_supersedes() {
        let cache = MemoryCache::open_in_memory(3600).unwrap();
        let m1 = make_memory("v1", "topic", "First version");
        cache.cache_memory(&m1, None).unwrap();

        let mut m2 = make_memory("v2", "topic", "Updated version");
        m2.supersedes = Some("v1".to_string());
        m2.version = 2;
        cache.cache_memory(&m2, None).unwrap();

        assert_eq!(cache.count().unwrap(), 2);
        let v2 = cache.get("v2").unwrap().unwrap();
        assert_eq!(v2.supersedes, Some("v1".to_string()));
    }
}
