//! Layered search over locally cached memories using SQLite FTS5.

use crate::config::MemoryConfig;
use crate::ranking::rank_memories;
use crate::types::{Memory, MemoryTier, SearchResult, SourcePreference};
use rusqlite::{params, Connection, Result as SqlResult};
use std::path::Path;

/// SQLite-backed memory index with FTS5 full-text search.
pub struct SqliteMemoryIndex {
    conn: Connection,
}

impl SqliteMemoryIndex {
    /// Open or create a memory index database.
    pub fn open(path: &Path) -> SqlResult<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memories (
                id TEXT PRIMARY KEY,
                tier TEXT NOT NULL,
                topic TEXT NOT NULL,
                summary TEXT NOT NULL,
                detail TEXT NOT NULL DEFAULT '',
                context TEXT,
                source TEXT NOT NULL,
                model TEXT NOT NULL DEFAULT '',
                confidence REAL NOT NULL DEFAULT 0.5,
                supersedes TEXT,
                version INTEGER NOT NULL DEFAULT 1,
                tags TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                event_json TEXT,
                cached_at INTEGER NOT NULL DEFAULT (unixepoch())
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                summary, detail, tags,
                content='memories',
                content_rowid='rowid'
            );

            CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
                INSERT INTO memories_fts(rowid, summary, detail, tags)
                VALUES (new.rowid, new.summary, new.detail, new.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, summary, detail, tags)
                VALUES ('delete', old.rowid, old.summary, old.detail, old.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, summary, detail, tags)
                VALUES ('delete', old.rowid, old.summary, old.detail, old.tags);
                INSERT INTO memories_fts(rowid, summary, detail, tags)
                VALUES (new.rowid, new.summary, new.detail, new.tags);
            END;",
        )?;

        Ok(Self { conn })
    }

    /// Open an in-memory database (for testing).
    pub fn open_in_memory() -> SqlResult<Self> {
        Self::open(Path::new(":memory:"))
    }

    /// Insert or update a memory.
    pub fn upsert(&self, memory: &Memory, event_json: Option<&str>) -> SqlResult<()> {
        let tier_str = memory.tier.to_string();
        let tags_str = memory.tags.join(",");

        self.conn.execute(
            "INSERT INTO memories (id, tier, topic, summary, detail, context, source, model, confidence, supersedes, version, tags, created_at, event_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(id) DO UPDATE SET
                summary=excluded.summary, detail=excluded.detail, context=excluded.context,
                model=excluded.model, confidence=excluded.confidence, supersedes=excluded.supersedes,
                version=excluded.version, tags=excluded.tags, event_json=excluded.event_json,
                cached_at=unixepoch()",
            params![
                memory.id, tier_str, memory.topic, memory.summary, memory.detail,
                memory.context, memory.source, memory.model, memory.confidence,
                memory.supersedes, memory.version, tags_str, memory.created_at, event_json,
            ],
        )?;
        Ok(())
    }

    /// Full-text search with optional tier filter.
    pub fn search(
        &self,
        query: &str,
        tier_filter: Option<&str>,
        limit: usize,
    ) -> SqlResult<Vec<(Memory, f64)>> {
        let tier_pattern = tier_filter.map(|t| format!("{}%", t));

        let (sql, use_tier) = if tier_pattern.is_some() {
            ("SELECT m.id, m.tier, m.topic, m.summary, m.detail, m.context, m.source, m.model,
                    m.confidence, m.supersedes, m.version, m.tags, m.created_at,
                    bm25(memories_fts) as rank
             FROM memories_fts f
             JOIN memories m ON m.rowid = f.rowid
             WHERE memories_fts MATCH ?1 AND m.tier LIKE ?2
             ORDER BY rank
             LIMIT ?3", true)
        } else {
            ("SELECT m.id, m.tier, m.topic, m.summary, m.detail, m.context, m.source, m.model,
                    m.confidence, m.supersedes, m.version, m.tags, m.created_at,
                    bm25(memories_fts) as rank
             FROM memories_fts f
             JOIN memories m ON m.rowid = f.rowid
             WHERE memories_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2", false)
        };

        let mut stmt = self.conn.prepare(sql)?;
        let mut results = Vec::new();

        if use_tier {
            let tp = tier_pattern.as_deref().unwrap_or("");
            let mut rows = stmt.query(params![query, tp, limit as i64])?;
            while let Some(row) = rows.next()? {
                if let Ok(mr) = Self::row_to_memory_rank(row) {
                    results.push(mr);
                }
            }
        } else {
            let mut rows = stmt.query(params![query, limit as i64])?;
            while let Some(row) = rows.next()? {
                if let Ok(mr) = Self::row_to_memory_rank(row) {
                    results.push(mr);
                }
            }
        }

        Ok(results)
    }

    /// Search and apply trust ranking using the full MemoryConfig.
    pub fn ranked_search(
        &self,
        query: &str,
        tier_filter: Option<&str>,
        config: &MemoryConfig,
        limit: usize,
    ) -> SqlResult<Vec<SearchResult>> {
        let raw = self.search(query, tier_filter, limit * 3)?;

        let pairs: Vec<(Memory, f64)> = raw
            .into_iter()
            .map(|(memory, bm25_score)| {
                let relevance = (-bm25_score).max(0.0).min(1.0);
                (memory, relevance)
            })
            .collect();

        let mut ranked = rank_memories(pairs, config);
        ranked.truncate(limit);
        Ok(ranked)
    }

    /// Get a memory by ID.
    pub fn get(&self, id: &str) -> SqlResult<Option<Memory>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, tier, topic, summary, detail, context, source, model,
                    confidence, supersedes, version, tags, created_at
             FROM memories WHERE id = ?1",
        )?;

        let mut rows = stmt.query_map(params![id], |row| {
            Self::row_to_memory(row)
        })?;

        match rows.next() {
            Some(Ok(m)) => Ok(Some(m)),
            _ => Ok(None),
        }
    }

    /// Delete memories older than `max_age_secs`.
    pub fn evict_stale(&self, max_age_secs: u64) -> SqlResult<usize> {
        let cutoff = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            - max_age_secs;

        let count = self.conn.execute(
            "DELETE FROM memories WHERE cached_at < ?1",
            params![cutoff as i64],
        )?;
        Ok(count)
    }

    /// Count total memories.
    pub fn count(&self) -> SqlResult<usize> {
        self.conn
            .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get::<_, usize>(0))
    }

    fn row_to_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
        let tier_str: String = row.get(1)?;
        let tags_str: String = row.get(11)?;

        Ok(Memory {
            id: row.get(0)?,
            tier: parse_tier(&tier_str),
            topic: row.get(2)?,
            summary: row.get(3)?,
            detail: row.get(4)?,
            context: row.get(5)?,
            source: row.get(6)?,
            model: row.get(7)?,
            confidence: row.get(8)?,
            supersedes: row.get(9)?,
            version: row.get(10)?,
            tags: tags_str.split(',').filter(|s| !s.is_empty()).map(String::from).collect(),
            created_at: row.get(12)?,
        })
    }

    fn row_to_memory_rank(row: &rusqlite::Row<'_>) -> rusqlite::Result<(Memory, f64)> {
        let memory = Self::row_to_memory(row)?;
        let rank: f64 = row.get(13)?;
        Ok((memory, rank))
    }
}

fn parse_tier(s: &str) -> MemoryTier {
    if s == "public" {
        MemoryTier::Public
    } else if let Some(id) = s.strip_prefix("group:") {
        MemoryTier::Group(id.to_string())
    } else if let Some(pk) = s.strip_prefix("private:") {
        MemoryTier::Private(pk.to_string())
    } else {
        MemoryTier::Public
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_memory(id: &str, topic: &str, summary: &str, source: &str) -> Memory {
        Memory {
            id: id.to_string(),
            tier: MemoryTier::Public,
            topic: topic.to_string(),
            summary: summary.to_string(),
            detail: String::new(),
            context: None,
            source: source.to_string(),
            model: "test/model".to_string(),
            confidence: 0.8,
            supersedes: None,
            version: 1,
            tags: vec!["rust".to_string()],
            created_at: 1700000000,
        }
    }

    #[test]
    fn test_upsert_and_get() {
        let idx = SqliteMemoryIndex::open_in_memory().unwrap();
        let m = make_memory("abc123", "rust/errors", "How to handle errors in Rust", "deadbeef");
        idx.upsert(&m, None).unwrap();

        let got = idx.get("abc123").unwrap().unwrap();
        assert_eq!(got.topic, "rust/errors");
        assert_eq!(got.summary, "How to handle errors in Rust");
    }

    #[test]
    fn test_fts_search() {
        let idx = SqliteMemoryIndex::open_in_memory().unwrap();
        idx.upsert(&make_memory("1", "rust/errors", "Error handling with Result type", "aaa"), None).unwrap();
        idx.upsert(&make_memory("2", "nostr/nip44", "NIP-44 encryption for private messages", "bbb"), None).unwrap();
        idx.upsert(&make_memory("3", "rust/async", "Async runtime with tokio", "aaa"), None).unwrap();

        let results = idx.search("error handling", None, 10).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].0.id, "1");
    }

    #[test]
    fn test_tier_filter() {
        let idx = SqliteMemoryIndex::open_in_memory().unwrap();
        let mut m1 = make_memory("1", "t1", "public memory about rust", "aaa");
        m1.tier = MemoryTier::Public;
        let mut m2 = make_memory("2", "t2", "group memory about rust", "bbb");
        m2.tier = MemoryTier::Group("team".to_string());

        idx.upsert(&m1, None).unwrap();
        idx.upsert(&m2, None).unwrap();

        let public = idx.search("rust", Some("public"), 10).unwrap();
        assert_eq!(public.len(), 1);
        assert_eq!(public[0].0.id, "1");
    }

    #[test]
    fn test_count_and_evict() {
        let idx = SqliteMemoryIndex::open_in_memory().unwrap();
        idx.upsert(&make_memory("1", "t", "test", "a"), None).unwrap();
        idx.upsert(&make_memory("2", "t", "test2", "a"), None).unwrap();
        assert_eq!(idx.count().unwrap(), 2);
    }
}
