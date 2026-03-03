//! Document/file indexing for workspace files.
//!
//! Indexes file content into SQLite with FTS5 search, using the chunker module
//! for splitting large files. Supports hash-based change detection to avoid
//! re-indexing unchanged files.

use anyhow::{Context, Result};
use chrono::Local;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use tracing::debug;

use super::chunker;

// ── Data structures ──────────────────────────────────────────────

/// An indexed document chunk stored in `indexed_docs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedDoc {
    pub id: String,
    pub path: String,
    pub chunk_index: usize,
    pub content: String,
    pub category: String,
    pub updated_at: String,
    pub hash: Option<String>,
}

/// A search hit from the document index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocHit {
    pub id: String,
    pub path: String,
    pub chunk_index: usize,
    pub content: String,
    pub category: String,
    pub score: f64,
}

// ── Schema ───────────────────────────────────────────────────────

/// Create document index tables and FTS5 in the given connection.
pub fn create_doc_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "-- Indexed documents (workspace files, room files, etc.)
        CREATE TABLE IF NOT EXISTS indexed_docs (
            id TEXT PRIMARY KEY,
            path TEXT NOT NULL,
            chunk_index INTEGER NOT NULL DEFAULT 0,
            content TEXT NOT NULL,
            category TEXT NOT NULL DEFAULT 'document',
            updated_at TEXT NOT NULL,
            hash TEXT,
            embedding BLOB
        );
        CREATE INDEX IF NOT EXISTS idx_docs_path ON indexed_docs(path);
        CREATE INDEX IF NOT EXISTS idx_docs_category ON indexed_docs(category);

        -- FTS5 for documents
        CREATE VIRTUAL TABLE IF NOT EXISTS docs_fts USING fts5(
            path, content,
            content='indexed_docs',
            content_rowid='rowid'
        );

        -- FTS5 sync triggers
        CREATE TRIGGER IF NOT EXISTS indexed_docs_ai AFTER INSERT ON indexed_docs BEGIN
            INSERT INTO docs_fts(rowid, path, content)
            VALUES (new.rowid, new.path, new.content);
        END;
        CREATE TRIGGER IF NOT EXISTS indexed_docs_ad AFTER DELETE ON indexed_docs BEGIN
            INSERT INTO docs_fts(docs_fts, rowid, path, content)
            VALUES ('delete', old.rowid, old.path, old.content);
        END;
        CREATE TRIGGER IF NOT EXISTS indexed_docs_au AFTER UPDATE ON indexed_docs BEGIN
            INSERT INTO docs_fts(docs_fts, rowid, path, content)
            VALUES ('delete', old.rowid, old.path, old.content);
            INSERT INTO docs_fts(rowid, path, content)
            VALUES (new.rowid, new.path, new.content);
        END;",
    )
    .context("failed to create doc index tables")?;

    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────

/// Compute a SHA-256 content hash (truncated to 16 hex chars).
fn content_hash(text: &str) -> String {
    let hash = Sha256::digest(text.as_bytes());
    format!(
        "{:016x}",
        u64::from_be_bytes(
            hash[..8]
                .try_into()
                .expect("SHA-256 always produces >= 8 bytes")
        )
    )
}

/// Max tokens per chunk for the markdown chunker.
const CHUNK_MAX_TOKENS: usize = 512;

// ── Operations ───────────────────────────────────────────────────

/// Index a file's content, chunking it and storing each chunk.
///
/// Uses hash-based change detection: if the file's content hash matches
/// the stored hash, indexing is skipped. Returns the number of chunks indexed.
pub fn index_file(
    conn: &Connection,
    path: &Path,
    category: &str,
) -> Result<usize> {
    let path_str = path.to_string_lossy().to_string();
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read file: {}", path_str))?;

    if content.trim().is_empty() {
        // Remove any existing index for this file
        unindex_file(conn, path)?;
        return Ok(0);
    }

    let hash = content_hash(&content);

    // Check if file is unchanged (compare hash of first chunk)
    let existing_hash: Option<String> = conn
        .query_row(
            "SELECT hash FROM indexed_docs WHERE path = ?1 AND chunk_index = 0",
            params![path_str],
            |row| row.get(0),
        )
        .ok()
        .flatten();

    if existing_hash.as_deref() == Some(&hash) {
        debug!(path = %path_str, "file unchanged, skipping re-index");
        return Ok(0);
    }

    // Remove old chunks for this path
    unindex_file(conn, path)?;

    // Chunk the content
    let chunks = chunker::chunk_markdown(&content, CHUNK_MAX_TOKENS);
    let now = Local::now().to_rfc3339();

    for chunk in &chunks {
        let id = format!("{}:{}", path_str, chunk.index);
        conn.execute(
            "INSERT INTO indexed_docs (id, path, chunk_index, content, category, updated_at, hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, path_str, chunk.index, chunk.content, category, now, hash],
        )?;
    }

    debug!(path = %path_str, chunks = chunks.len(), "indexed file");
    Ok(chunks.len())
}

/// Index raw content (not from a file) with a virtual path identifier.
///
/// Useful for indexing in-memory content like room descriptions or generated text.
pub fn index_content(
    conn: &Connection,
    virtual_path: &str,
    content: &str,
    category: &str,
) -> Result<usize> {
    if content.trim().is_empty() {
        unindex_path(conn, virtual_path)?;
        return Ok(0);
    }

    let hash = content_hash(content);

    let existing_hash: Option<String> = conn
        .query_row(
            "SELECT hash FROM indexed_docs WHERE path = ?1 AND chunk_index = 0",
            params![virtual_path],
            |row| row.get(0),
        )
        .ok()
        .flatten();

    if existing_hash.as_deref() == Some(&hash) {
        return Ok(0);
    }

    unindex_path(conn, virtual_path)?;

    let chunks = chunker::chunk_markdown(content, CHUNK_MAX_TOKENS);
    let now = Local::now().to_rfc3339();

    for chunk in &chunks {
        let id = format!("{}:{}", virtual_path, chunk.index);
        conn.execute(
            "INSERT INTO indexed_docs (id, path, chunk_index, content, category, updated_at, hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, virtual_path, chunk.index, chunk.content, category, now, hash],
        )?;
    }

    Ok(chunks.len())
}

/// Remove a file from the index by path.
pub fn unindex_file(conn: &Connection, path: &Path) -> Result<()> {
    let path_str = path.to_string_lossy().to_string();
    unindex_path(conn, &path_str)
}

/// Remove all chunks for a given path string.
fn unindex_path(conn: &Connection, path: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM indexed_docs WHERE path = ?1",
        params![path],
    )?;
    Ok(())
}

/// Search documents using FTS5.
pub fn search_docs(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<DocHit>> {
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
        "SELECT d.id, d.path, d.chunk_index, d.content, d.category, bm25(docs_fts) as score
         FROM docs_fts f
         JOIN indexed_docs d ON d.rowid = f.rowid
         WHERE docs_fts MATCH ?1
         ORDER BY score
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(params![fts_query, limit_i64], |row| {
        let score: f64 = row.get(5)?;
        Ok(DocHit {
            id: row.get(0)?,
            path: row.get(1)?,
            chunk_index: row.get(2)?,
            content: row.get(3)?,
            category: row.get(4)?,
            score: -score, // BM25: lower = better, negate for ranking
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Count total indexed document chunks.
pub fn count_docs(conn: &Connection) -> Result<usize> {
    let count: i64 =
        conn.query_row("SELECT COUNT(*) FROM indexed_docs", [], |row| row.get(0))?;
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    Ok(count as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .unwrap();
        create_doc_tables(&conn).unwrap();
        conn
    }

    fn write_temp_file(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    // ── Schema tests ─────────────────────────────────────────────

    #[test]
    fn create_tables_idempotent() {
        let conn = test_conn();
        create_doc_tables(&conn).unwrap();
    }

    #[test]
    fn tables_exist() {
        let conn = test_conn();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='indexed_docs'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    // ── index_file tests ────────────────────────────────────────

    #[test]
    fn index_small_file() {
        let conn = test_conn();
        let dir = TempDir::new().unwrap();
        let path = write_temp_file(dir.path(), "test.md", "Hello world, this is a test file.");

        let chunks = index_file(&conn, &path, "document").unwrap();
        assert_eq!(chunks, 1);
        assert_eq!(count_docs(&conn).unwrap(), 1);
    }

    #[test]
    fn index_large_file_creates_multiple_chunks() {
        let conn = test_conn();
        let dir = TempDir::new().unwrap();

        // Create content large enough to require multiple chunks
        let mut content = String::new();
        for i in 0..100 {
            use std::fmt::Write;
            let _ = writeln!(
                content,
                "## Section {i}\n\nThis is paragraph {i} with enough content to make it substantial."
            );
        }

        let path = write_temp_file(dir.path(), "big.md", &content);
        let chunks = index_file(&conn, &path, "document").unwrap();
        assert!(chunks > 1, "Expected multiple chunks, got {chunks}");
        assert_eq!(count_docs(&conn).unwrap(), chunks);
    }

    #[test]
    fn index_empty_file_removes_existing() {
        let conn = test_conn();
        let dir = TempDir::new().unwrap();
        let path = write_temp_file(dir.path(), "test.md", "some content here");
        index_file(&conn, &path, "document").unwrap();
        assert_eq!(count_docs(&conn).unwrap(), 1);

        // Overwrite with empty content
        std::fs::write(&path, "").unwrap();
        let chunks = index_file(&conn, &path, "document").unwrap();
        assert_eq!(chunks, 0);
        assert_eq!(count_docs(&conn).unwrap(), 0);
    }

    #[test]
    fn index_unchanged_file_skips() {
        let conn = test_conn();
        let dir = TempDir::new().unwrap();
        let path = write_temp_file(dir.path(), "test.md", "stable content here");

        let chunks1 = index_file(&conn, &path, "document").unwrap();
        assert_eq!(chunks1, 1);

        // Re-index same content — should skip
        let chunks2 = index_file(&conn, &path, "document").unwrap();
        assert_eq!(chunks2, 0);
    }

    #[test]
    fn index_changed_file_re_indexes() {
        let conn = test_conn();
        let dir = TempDir::new().unwrap();
        let path = write_temp_file(dir.path(), "test.md", "alpha original content");
        index_file(&conn, &path, "document").unwrap();

        // Change content
        std::fs::write(&path, "beta replacement content").unwrap();
        let chunks = index_file(&conn, &path, "document").unwrap();
        assert_eq!(chunks, 1);

        // Should find new content
        let results = search_docs(&conn, "replacement", 10).unwrap();
        assert_eq!(results.len(), 1);

        // Should not find old-only keyword
        let results = search_docs(&conn, "original", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn index_nonexistent_file_errors() {
        let conn = test_conn();
        let result = index_file(&conn, Path::new("/nonexistent/path.md"), "document");
        assert!(result.is_err());
    }

    // ── unindex_file tests ──────────────────────────────────────

    #[test]
    fn unindex_file_removes_all_chunks() {
        let conn = test_conn();
        let dir = TempDir::new().unwrap();
        let path = write_temp_file(dir.path(), "test.md", "content to remove");
        index_file(&conn, &path, "document").unwrap();
        assert_eq!(count_docs(&conn).unwrap(), 1);

        unindex_file(&conn, &path).unwrap();
        assert_eq!(count_docs(&conn).unwrap(), 0);
    }

    #[test]
    fn unindex_nonexistent_is_noop() {
        let conn = test_conn();
        unindex_file(&conn, Path::new("/nonexistent/path.md")).unwrap();
    }

    // ── index_content tests ─────────────────────────────────────

    #[test]
    fn index_content_works() {
        let conn = test_conn();
        let chunks = index_content(&conn, "virtual://room/test", "Room description here", "room")
            .unwrap();
        assert_eq!(chunks, 1);

        let results = search_docs(&conn, "Room description", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].category, "room");
    }

    #[test]
    fn index_content_change_detection() {
        let conn = test_conn();
        index_content(&conn, "v://test", "content v1", "note").unwrap();

        // Same content — skip
        let chunks = index_content(&conn, "v://test", "content v1", "note").unwrap();
        assert_eq!(chunks, 0);

        // Changed content — re-index
        let chunks = index_content(&conn, "v://test", "content v2", "note").unwrap();
        assert_eq!(chunks, 1);
    }

    // ── search_docs tests ───────────────────────────────────────

    #[test]
    fn search_finds_content() {
        let conn = test_conn();
        let dir = TempDir::new().unwrap();
        let path = write_temp_file(dir.path(), "rust.md", "Rust is a systems programming language");
        index_file(&conn, &path, "document").unwrap();

        let results = search_docs(&conn, "Rust programming", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].score > 0.0);
    }

    #[test]
    fn search_empty_query() {
        let conn = test_conn();
        assert!(search_docs(&conn, "", 10).unwrap().is_empty());
        assert!(search_docs(&conn, "   ", 10).unwrap().is_empty());
    }

    #[test]
    fn search_no_match() {
        let conn = test_conn();
        let dir = TempDir::new().unwrap();
        let path = write_temp_file(dir.path(), "test.md", "Rust content");
        index_file(&conn, &path, "document").unwrap();

        let results = search_docs(&conn, "javascript", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_respects_limit() {
        let conn = test_conn();
        for i in 0..20 {
            index_content(
                &conn,
                &format!("v://doc_{i}"),
                &format!("common keyword document number {i}"),
                "document",
            )
            .unwrap();
        }

        let results = search_docs(&conn, "common keyword", 5).unwrap();
        assert!(results.len() <= 5);
    }

    #[test]
    fn search_with_double_quotes_does_not_error() {
        let conn = test_conn();
        index_content(
            &conn,
            "v://test.md",
            "Rust systems programming guide content",
            "document",
        )
        .unwrap();

        // Should not crash on embedded double quotes
        let results = search_docs(&conn, r#"say "hello" world"#, 10).unwrap();
        assert!(results.len() <= 10);
    }

    // ── Hash tests ──────────────────────────────────────────────

    #[test]
    fn content_hash_deterministic() {
        assert_eq!(content_hash("hello"), content_hash("hello"));
    }

    #[test]
    fn content_hash_different_inputs() {
        assert_ne!(content_hash("hello"), content_hash("world"));
    }

    #[test]
    fn content_hash_format() {
        let h = content_hash("test");
        assert_eq!(h.len(), 16);
    }

    // ── Unicode ─────────────────────────────────────────────────

    #[test]
    fn index_and_search_unicode() {
        let conn = test_conn();
        index_content(
            &conn,
            "v://unicode",
            "Unicode document with development notes",
            "document",
        )
        .unwrap();

        let results = search_docs(&conn, "development", 10).unwrap();
        assert!(!results.is_empty());
        assert!(results[0].content.contains("Unicode"));
    }
}
