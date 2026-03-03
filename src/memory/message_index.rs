//! Message indexing for Nostr events.
//!
//! Provides selective indexing of Nostr messages into SQLite with FTS5
//! for semantic search. Not all messages are indexed — the `should_index_message`
//! function determines which messages are worth storing.

use anyhow::{Context, Result};
use chrono::Local;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tracing::debug;

// ── Data structures ──────────────────────────────────────────────

/// A message suitable for indexing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexableMessage {
    pub event_id: String,
    pub sender_hex: String,
    pub group_id: Option<String>,
    pub content: String,
    pub created_at: i64,
    pub kind: u32,
}

/// Decision on whether/how to index a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexDecision {
    /// Full index with FTS5 (and optional embedding later).
    Index,
    /// Store in message_index but skip embedding — keyword search only.
    CacheOnly,
    /// Don't store at all.
    Skip,
}

/// A search hit from the message index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageHit {
    pub event_id: String,
    pub sender_hex: String,
    pub group_id: Option<String>,
    pub content: String,
    pub created_at: i64,
    pub kind: u32,
    pub score: f64,
}

// ── Schema ───────────────────────────────────────────────────────

/// Create message index tables and FTS5 in the given connection.
pub fn create_message_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "-- Message index
        CREATE TABLE IF NOT EXISTS message_index (
            event_id TEXT PRIMARY KEY,
            sender_hex TEXT NOT NULL,
            group_id TEXT,
            content TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            kind INTEGER NOT NULL,
            indexed_at TEXT NOT NULL,
            embedding BLOB
        );
        CREATE INDEX IF NOT EXISTS idx_message_sender ON message_index(sender_hex);
        CREATE INDEX IF NOT EXISTS idx_message_group ON message_index(group_id);
        CREATE INDEX IF NOT EXISTS idx_message_created ON message_index(created_at);

        -- FTS5 for messages
        CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
            content, sender_hex, group_id,
            content='message_index',
            content_rowid='rowid'
        );

        -- FTS5 sync triggers
        CREATE TRIGGER IF NOT EXISTS message_index_ai AFTER INSERT ON message_index BEGIN
            INSERT INTO messages_fts(rowid, content, sender_hex, group_id)
            VALUES (new.rowid, new.content, new.sender_hex, COALESCE(new.group_id, ''));
        END;
        CREATE TRIGGER IF NOT EXISTS message_index_ad AFTER DELETE ON message_index BEGIN
            INSERT INTO messages_fts(messages_fts, rowid, content, sender_hex, group_id)
            VALUES ('delete', old.rowid, old.content, old.sender_hex, COALESCE(old.group_id, ''));
        END;
        CREATE TRIGGER IF NOT EXISTS message_index_au AFTER UPDATE ON message_index BEGIN
            INSERT INTO messages_fts(messages_fts, rowid, content, sender_hex, group_id)
            VALUES ('delete', old.rowid, old.content, old.sender_hex, COALESCE(old.group_id, ''));
            INSERT INTO messages_fts(rowid, content, sender_hex, group_id)
            VALUES (new.rowid, new.content, new.sender_hex, COALESCE(new.group_id, ''));
        END;",
    )
    .context("failed to create message index tables")?;

    Ok(())
}

// ── Indexing logic ───────────────────────────────────────────────

/// Minimum content length for full indexing.
const MIN_INDEX_LENGTH: usize = 20;

/// Minimum content length for cache-only storage.
const MIN_CACHE_LENGTH: usize = 5;

/// Determine whether a message should be indexed.
///
/// Strategy:
/// - Skip: very short messages, reactions (kind 7), reposts (kind 6), empty content
/// - CacheOnly: short but non-trivial messages
/// - Index: substantial content (>= 20 chars), DMs (kind 4/1059), mentions
pub fn should_index_message(
    content: &str,
    kind: u32,
    is_bot_mention: bool,
    is_dm: bool,
) -> IndexDecision {
    let trimmed = content.trim();

    // Always skip reactions, reposts, and empty content
    if kind == 7 || kind == 6 || trimmed.is_empty() {
        return IndexDecision::Skip;
    }

    // Always index DMs and bot mentions with any content
    if (is_dm || is_bot_mention) && trimmed.len() >= MIN_CACHE_LENGTH {
        return IndexDecision::Index;
    }

    // Index substantial content
    if trimmed.len() >= MIN_INDEX_LENGTH {
        return IndexDecision::Index;
    }

    // Cache short but non-trivial messages
    if trimmed.len() >= MIN_CACHE_LENGTH {
        return IndexDecision::CacheOnly;
    }

    IndexDecision::Skip
}

// ── CRUD operations ──────────────────────────────────────────────

/// Index a message into the message_index table.
pub fn index_message(conn: &Connection, msg: &IndexableMessage) -> Result<()> {
    let now = Local::now().to_rfc3339();

    conn.execute(
        "INSERT OR REPLACE INTO message_index
            (event_id, sender_hex, group_id, content, created_at, kind, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            msg.event_id,
            msg.sender_hex,
            msg.group_id,
            msg.content,
            msg.created_at,
            msg.kind,
            now,
        ],
    )?;

    debug!(event_id = %msg.event_id, kind = msg.kind, "indexed message");
    Ok(())
}

/// Search messages using FTS5.
pub fn search_messages(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<MessageHit>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }

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
        "SELECT m.event_id, m.sender_hex, m.group_id, m.content, m.created_at, m.kind,
                bm25(messages_fts) as score
         FROM messages_fts f
         JOIN message_index m ON m.rowid = f.rowid
         WHERE messages_fts MATCH ?1
         ORDER BY score
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(params![fts_query, limit_i64], |row| {
        let score: f64 = row.get(6)?;
        Ok(MessageHit {
            event_id: row.get(0)?,
            sender_hex: row.get(1)?,
            group_id: row.get(2)?,
            content: row.get(3)?,
            created_at: row.get(4)?,
            kind: row.get(5)?,
            score: -score, // BM25 returns negative (lower = better), negate for ranking
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Count indexed messages.
pub fn count_messages(conn: &Connection) -> Result<usize> {
    let count: i64 =
        conn.query_row("SELECT COUNT(*) FROM message_index", [], |row| row.get(0))?;
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    Ok(count as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .unwrap();
        create_message_tables(&conn).unwrap();
        conn
    }

    fn sample_message(id: &str, content: &str) -> IndexableMessage {
        IndexableMessage {
            event_id: id.to_string(),
            sender_hex: "aabb".to_string(),
            group_id: Some("test_group".to_string()),
            content: content.to_string(),
            created_at: 1000,
            kind: 1,
        }
    }

    // ── Schema tests ─────────────────────────────────────────────

    #[test]
    fn create_tables_idempotent() {
        let conn = test_conn();
        create_message_tables(&conn).unwrap();
    }

    #[test]
    fn tables_exist() {
        let conn = test_conn();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='message_index'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    // ── should_index_message tests ──────────────────────────────

    #[test]
    fn skip_reactions() {
        assert_eq!(
            should_index_message("👍", 7, false, false),
            IndexDecision::Skip
        );
    }

    #[test]
    fn skip_reposts() {
        assert_eq!(
            should_index_message("repost content", 6, false, false),
            IndexDecision::Skip
        );
    }

    #[test]
    fn skip_empty_content() {
        assert_eq!(
            should_index_message("", 1, false, false),
            IndexDecision::Skip
        );
        assert_eq!(
            should_index_message("   ", 1, false, false),
            IndexDecision::Skip
        );
    }

    #[test]
    fn skip_very_short_content() {
        assert_eq!(
            should_index_message("hi", 1, false, false),
            IndexDecision::Skip
        );
    }

    #[test]
    fn cache_only_short_content() {
        assert_eq!(
            should_index_message("hello world", 1, false, false),
            IndexDecision::CacheOnly
        );
    }

    #[test]
    fn index_substantial_content() {
        assert_eq!(
            should_index_message("This is a substantial message with enough content", 1, false, false),
            IndexDecision::Index
        );
    }

    #[test]
    fn index_dm_with_content() {
        assert_eq!(
            should_index_message("short DM", 4, false, true),
            IndexDecision::Index
        );
    }

    #[test]
    fn index_bot_mention() {
        assert_eq!(
            should_index_message("hey bot!", 1, true, false),
            IndexDecision::Index
        );
    }

    #[test]
    fn skip_dm_empty_content() {
        assert_eq!(
            should_index_message("", 4, false, true),
            IndexDecision::Skip
        );
    }

    // ── Index and search tests ──────────────────────────────────

    #[test]
    fn index_and_search_message() {
        let conn = test_conn();
        let msg = sample_message("evt1", "Rust is a systems programming language");
        index_message(&conn, &msg).unwrap();

        let results = search_messages(&conn, "Rust programming", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].event_id, "evt1");
        assert!(results[0].score > 0.0);
    }

    #[test]
    fn search_no_match() {
        let conn = test_conn();
        index_message(
            &conn,
            &sample_message("evt1", "Rust is a systems programming language"),
        )
        .unwrap();

        let results = search_messages(&conn, "javascript", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_empty_query() {
        let conn = test_conn();
        index_message(&conn, &sample_message("evt1", "some content here")).unwrap();
        assert!(search_messages(&conn, "", 10).unwrap().is_empty());
        assert!(search_messages(&conn, "   ", 10).unwrap().is_empty());
    }

    #[test]
    fn search_respects_limit() {
        let conn = test_conn();
        for i in 0..20 {
            index_message(
                &conn,
                &sample_message(&format!("evt_{i}"), &format!("common keyword message {i}")),
            )
            .unwrap();
        }

        let results = search_messages(&conn, "common keyword", 5).unwrap();
        assert!(results.len() <= 5);
    }

    #[test]
    fn index_message_upsert() {
        let conn = test_conn();
        let msg = sample_message("evt1", "original content");
        index_message(&conn, &msg).unwrap();

        let updated = IndexableMessage {
            content: "updated content".to_string(),
            ..msg
        };
        index_message(&conn, &updated).unwrap();

        assert_eq!(count_messages(&conn).unwrap(), 1);

        let results = search_messages(&conn, "updated", 10).unwrap();
        assert_eq!(results.len(), 1);

        let results = search_messages(&conn, "original", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn count_messages_works() {
        let conn = test_conn();
        assert_eq!(count_messages(&conn).unwrap(), 0);

        index_message(&conn, &sample_message("evt1", "message one content here")).unwrap();
        index_message(&conn, &sample_message("evt2", "message two content here")).unwrap();
        assert_eq!(count_messages(&conn).unwrap(), 2);
    }

    #[test]
    fn message_with_no_group() {
        let conn = test_conn();
        let msg = IndexableMessage {
            group_id: None,
            ..sample_message("evt1", "DM content without group context")
        };
        index_message(&conn, &msg).unwrap();

        let results = search_messages(&conn, "DM content", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].group_id.is_none());
    }

    // ── FTS5 special chars ──────────────────────────────────────

    #[test]
    fn search_with_special_chars() {
        let conn = test_conn();
        index_message(
            &conn,
            &sample_message("evt1", "function call test with parens"),
        )
        .unwrap();
        // Should not crash
        let results = search_messages(&conn, "function()", 10).unwrap();
        assert!(results.len() <= 10);
    }

    #[test]
    fn search_with_double_quotes_does_not_error() {
        let conn = test_conn();
        index_message(
            &conn,
            &sample_message("evt1", "Rust is a systems programming language"),
        )
        .unwrap();

        // Should not crash on embedded double quotes
        let results = search_messages(&conn, r#"say "hello" world"#, 10).unwrap();
        assert!(results.len() <= 10);
    }

    #[test]
    fn search_unicode_content() {
        let conn = test_conn();
        index_message(
            &conn,
            &sample_message("evt1", "Unicode content with development notes"),
        )
        .unwrap();
        let results = search_messages(&conn, "development", 10).unwrap();
        assert!(!results.is_empty());
        assert!(results[0].content.contains("Unicode"));
    }
}
