//! Unified cross-type search across all memory tables.
//!
//! Merges results from core memories, social data, messages, and documents
//! into a single ranked list. Each hit type carries its source metadata.

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::doc_index::{self, DocHit};
use super::message_index::{self, MessageHit};
use super::social::{self, SocialNpub};
use super::traits::{MemoryCategory, MemoryEntry};

// ── Data structures ──────────────────────────────────────────────

/// A search hit from any memory subsystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UnifiedHit {
    /// Hit from the core `memories` table.
    Memory(MemoryEntry),
    /// Hit from `social_npubs`.
    Social(SocialNpub),
    /// Hit from `message_index`.
    Message(MessageHit),
    /// Hit from `indexed_docs`.
    Document(DocHit),
}

impl UnifiedHit {
    /// Score for ranking. Higher is better.
    pub fn score(&self) -> f64 {
        match self {
            Self::Memory(e) => e.score.unwrap_or(0.0),
            Self::Social(_) => 1.0, // FTS5 results from social are pre-ranked
            Self::Message(m) => m.score,
            Self::Document(d) => d.score,
        }
    }

    /// Human-readable source label.
    pub fn source(&self) -> &'static str {
        match self {
            Self::Memory(_) => "memory",
            Self::Social(_) => "social",
            Self::Message(_) => "message",
            Self::Document(_) => "document",
        }
    }

    /// Content snippet for display.
    pub fn snippet(&self) -> &str {
        match self {
            Self::Memory(e) => &e.content,
            Self::Social(s) => &s.display_name,
            Self::Message(m) => &m.content,
            Self::Document(d) => &d.content,
        }
    }
}

// ── Search ───────────────────────────────────────────────────────

/// Search across all memory types and merge results by relevance.
///
/// Queries:
/// 1. Core memories (via `memories_fts`)
/// 2. Social data (via `social_fts`)
/// 3. Messages (via `messages_fts`)
/// 4. Documents (via `docs_fts`)
///
/// Results are merged and sorted by score (descending), then truncated to `limit`.
/// Each subsystem search is best-effort — if a table doesn't exist yet,
/// that source is silently skipped.
pub fn unified_recall(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<UnifiedHit>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }

    let per_source_limit = limit * 2; // fetch more per-source to improve merge quality
    let mut hits: Vec<UnifiedHit> = Vec::new();

    // 1. Core memories
    if let Ok(entries) = search_memories_fts(conn, query, per_source_limit) {
        for entry in entries {
            hits.push(UnifiedHit::Memory(entry));
        }
    }

    // 2. Social data
    if let Ok(npubs) = social::search_social(conn, query, per_source_limit) {
        for npub in npubs {
            hits.push(UnifiedHit::Social(npub));
        }
    }

    // 3. Messages
    if let Ok(messages) = message_index::search_messages(conn, query, per_source_limit) {
        for msg in messages {
            hits.push(UnifiedHit::Message(msg));
        }
    }

    // 4. Documents
    if let Ok(docs) = doc_index::search_docs(conn, query, per_source_limit) {
        for doc in docs {
            hits.push(UnifiedHit::Document(doc));
        }
    }

    // Sort by score descending
    hits.sort_by(|a, b| {
        b.score()
            .partial_cmp(&a.score())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    hits.truncate(limit);
    Ok(hits)
}

/// FTS5 search over the core `memories` table.
///
/// This is a standalone search function that doesn't require the full `SqliteMemory`
/// struct — it operates directly on the connection.
fn search_memories_fts(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<MemoryEntry>> {
    let fts_query: String = query
        .split_whitespace()
        .map(|w| {
            let clean = w.replace('"', "");
            format!("\"{clean}\"")
        })
        .filter(|w| w != "\"\"")
        .collect::<Vec<_>>()
        .join(" OR ");

    if fts_query.is_empty() {
        return Ok(Vec::new());
    }

    #[allow(clippy::cast_possible_wrap)]
    let limit_i64 = limit as i64;

    let mut stmt = conn.prepare(
        "SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id,
                bm25(memories_fts) as score
         FROM memories_fts f
         JOIN memories m ON m.rowid = f.rowid
         WHERE memories_fts MATCH ?1
         ORDER BY score
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(rusqlite::params![fts_query, limit_i64], |row| {
        let score: f64 = row.get(6)?;
        let cat_str: String = row.get(3)?;
        let category = match cat_str.as_str() {
            "core" => MemoryCategory::Core,
            "daily" => MemoryCategory::Daily,
            "conversation" => MemoryCategory::Conversation,
            other => MemoryCategory::Custom(other.to_string()),
        };
        Ok(MemoryEntry {
            id: row.get(0)?,
            key: row.get(1)?,
            content: row.get(2)?,
            category,
            timestamp: row.get(4)?,
            session_id: row.get(5)?,
            score: Some(-score), // BM25: lower = better, negate
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::doc_index;
    use super::super::message_index::{self, IndexableMessage};
    use super::super::social::{self, SocialNpub};
    use super::super::traits::MemoryCategory;
    use tempfile::TempDir;

    /// Create the core memories table + FTS5 inline (avoids calling private init_schema).
    fn create_memories_tables(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memories (
                id          TEXT PRIMARY KEY,
                key         TEXT NOT NULL UNIQUE,
                content     TEXT NOT NULL,
                category    TEXT NOT NULL DEFAULT 'core',
                embedding   BLOB,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                session_id  TEXT
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                key, content, content=memories, content_rowid=rowid
            );
            CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
                INSERT INTO memories_fts(rowid, key, content)
                VALUES (new.rowid, new.key, new.content);
            END;
            CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, key, content)
                VALUES ('delete', old.rowid, old.key, old.content);
            END;
            CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, key, content)
                VALUES ('delete', old.rowid, old.key, old.content);
                INSERT INTO memories_fts(rowid, key, content)
                VALUES (new.rowid, new.key, new.content);
            END;",
        )
        .unwrap();
    }

    /// Set up a full test environment with all tables.
    fn test_env() -> (TempDir, Connection) {
        let tmp = TempDir::new().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .unwrap();

        // Create all table schemas
        create_memories_tables(&conn);
        social::create_social_tables(&conn).unwrap();
        message_index::create_message_tables(&conn).unwrap();
        doc_index::create_doc_tables(&conn).unwrap();

        (tmp, conn)
    }

    // ── Basic tests ─────────────────────────────────────────────

    #[test]
    fn empty_query_returns_empty() {
        let (_tmp, conn) = test_env();
        assert!(unified_recall(&conn, "", 10).unwrap().is_empty());
        assert!(unified_recall(&conn, "   ", 10).unwrap().is_empty());
    }

    #[test]
    fn no_data_returns_empty() {
        let (_tmp, conn) = test_env();
        let results = unified_recall(&conn, "anything", 10).unwrap();
        assert!(results.is_empty());
    }

    // ── Cross-type search tests ─────────────────────────────────

    #[test]
    fn finds_core_memories() {
        let (_tmp, conn) = test_env();

        // Insert a core memory directly
        conn.execute(
            "INSERT INTO memories (id, key, content, category, created_at, updated_at)
             VALUES ('m1', 'test_key', 'Rust programming language', 'core', '2025-01-01', '2025-01-01')",
            [],
        )
        .unwrap();

        let results = unified_recall(&conn, "Rust programming", 10).unwrap();
        assert!(!results.is_empty());
        assert!(matches!(results[0], UnifiedHit::Memory(_)));
    }

    #[test]
    fn finds_social_data() {
        let (_tmp, conn) = test_env();

        social::upsert_npub(
            &conn,
            &SocialNpub {
                hex_pubkey: "aabb".to_string(),
                display_name: "RustDeveloper".to_string(),
                first_seen: 1000,
                first_seen_group: None,
                last_interaction: 2000,
                profile_json: None,
                name_history_json: None,
                notes_json: None,
                owner_notes_json: None,
                preferences_json: None,
                is_owner: false,
            },
        )
        .unwrap();

        let results = unified_recall(&conn, "RustDeveloper", 10).unwrap();
        assert!(!results.is_empty());
        assert!(results.iter().any(|h| matches!(h, UnifiedHit::Social(_))));
    }

    #[test]
    fn finds_messages() {
        let (_tmp, conn) = test_env();

        message_index::index_message(
            &conn,
            &IndexableMessage {
                event_id: "evt1".to_string(),
                sender_hex: "aabb".to_string(),
                group_id: None,
                content: "Rust async runtime discussion".to_string(),
                created_at: 1000,
                kind: 1,
            },
        )
        .unwrap();

        let results = unified_recall(&conn, "async runtime", 10).unwrap();
        assert!(!results.is_empty());
        assert!(results.iter().any(|h| matches!(h, UnifiedHit::Message(_))));
    }

    #[test]
    fn finds_documents() {
        let (_tmp, conn) = test_env();

        doc_index::index_content(
            &conn,
            "v://test.md",
            "Rust systems programming guide",
            "document",
        )
        .unwrap();

        let results = unified_recall(&conn, "systems programming", 10).unwrap();
        assert!(!results.is_empty());
        assert!(results.iter().any(|h| matches!(h, UnifiedHit::Document(_))));
    }

    #[test]
    fn merges_results_from_multiple_sources() {
        let (_tmp, conn) = test_env();

        // Add data to each source with "Rust" keyword
        conn.execute(
            "INSERT INTO memories (id, key, content, category, created_at, updated_at)
             VALUES ('m1', 'lang', 'Rust is my favorite language', 'core', '2025-01-01', '2025-01-01')",
            [],
        )
        .unwrap();

        social::upsert_npub(
            &conn,
            &SocialNpub {
                hex_pubkey: "aabb".to_string(),
                display_name: "Rustacean".to_string(),
                first_seen: 1000,
                first_seen_group: None,
                last_interaction: 2000,
                profile_json: None,
                name_history_json: None,
                notes_json: None,
                owner_notes_json: None,
                preferences_json: None,
                is_owner: false,
            },
        )
        .unwrap();

        message_index::index_message(
            &conn,
            &IndexableMessage {
                event_id: "evt1".to_string(),
                sender_hex: "aabb".to_string(),
                group_id: None,
                content: "Rust performance benchmarks".to_string(),
                created_at: 1000,
                kind: 1,
            },
        )
        .unwrap();

        doc_index::index_content(
            &conn,
            "v://guide.md",
            "Getting started with Rust programming",
            "document",
        )
        .unwrap();

        let results = unified_recall(&conn, "Rust", 10).unwrap();
        assert!(results.len() >= 3, "Expected at least 3 results from different sources, got {}", results.len());

        // Verify we have hits from multiple sources
        let sources: Vec<&str> = results.iter().map(|h| h.source()).collect();
        assert!(sources.contains(&"memory"), "Should contain memory hits");
        assert!(sources.contains(&"message"), "Should contain message hits");
        assert!(sources.contains(&"document"), "Should contain document hits");
    }

    #[test]
    fn respects_limit() {
        let (_tmp, conn) = test_env();

        for i in 0..20 {
            conn.execute(
                "INSERT INTO memories (id, key, content, category, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'core', '2025-01-01', '2025-01-01')",
                rusqlite::params![
                    format!("m{i}"),
                    format!("key_{i}"),
                    format!("common keyword data item {i}")
                ],
            )
            .unwrap();
        }

        let results = unified_recall(&conn, "common keyword", 5).unwrap();
        assert!(results.len() <= 5);
    }

    #[test]
    fn results_sorted_by_score() {
        let (_tmp, conn) = test_env();

        conn.execute(
            "INSERT INTO memories (id, key, content, category, created_at, updated_at)
             VALUES ('m1', 'k1', 'Rust language', 'core', '2025-01-01', '2025-01-01')",
            [],
        )
        .unwrap();

        doc_index::index_content(
            &conn,
            "v://doc.md",
            "Rust language programming",
            "document",
        )
        .unwrap();

        let results = unified_recall(&conn, "Rust language", 10).unwrap();
        // Verify descending score order
        for window in results.windows(2) {
            assert!(
                window[0].score() >= window[1].score(),
                "Results should be sorted by score descending"
            );
        }
    }

    // ── UnifiedHit methods ──────────────────────────────────────

    #[test]
    fn unified_hit_source_labels() {
        let memory_hit = UnifiedHit::Memory(MemoryEntry {
            id: "m1".into(),
            key: "k1".into(),
            content: "test".into(),
            category: MemoryCategory::Core,
            timestamp: "2025-01-01".into(),
            session_id: None,
            score: Some(1.0),
        });
        assert_eq!(memory_hit.source(), "memory");
        assert_eq!(memory_hit.snippet(), "test");

        let social_hit = UnifiedHit::Social(SocialNpub {
            hex_pubkey: "aabb".into(),
            display_name: "Alice".into(),
            first_seen: 1000,
            first_seen_group: None,
            last_interaction: 2000,
            profile_json: None,
            name_history_json: None,
            notes_json: None,
            owner_notes_json: None,
            preferences_json: None,
            is_owner: false,
        });
        assert_eq!(social_hit.source(), "social");
        assert_eq!(social_hit.snippet(), "Alice");
    }

    #[test]
    fn search_with_double_quotes_does_not_error() {
        let (_tmp, conn) = test_env();

        conn.execute(
            "INSERT INTO memories (id, key, content, category, created_at, updated_at)
             VALUES ('m1', 'k1', 'test data here', 'core', '2025-01-01', '2025-01-01')",
            [],
        )
        .unwrap();

        // Should not crash on embedded double quotes
        let results = unified_recall(&conn, r#"say "hello" world"#, 10).unwrap();
        assert!(results.len() <= 10);
    }

    // ── Graceful degradation ────────────────────────────────────

    #[test]
    fn works_with_only_some_tables() {
        // Only create core memory tables — other searches should silently fail
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
        create_memories_tables(&conn);

        conn.execute(
            "INSERT INTO memories (id, key, content, category, created_at, updated_at)
             VALUES ('m1', 'k1', 'test data here', 'core', '2025-01-01', '2025-01-01')",
            [],
        )
        .unwrap();

        // Should still find core memory results despite missing social/message/doc tables
        let results = unified_recall(&conn, "test data", 10).unwrap();
        assert!(!results.is_empty());
    }
}
