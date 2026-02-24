//! File indexer service for workspace documents.
//!
//! Resolves configured glob patterns against the workspace directory and
//! indexes matching files using `doc_index`. Hash-based change detection
//! (already in `doc_index`) avoids re-indexing unchanged files.

use anyhow::Result;
use parking_lot::Mutex as ParkingMutex;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, info, warn};

use super::doc_index;

/// Indexes configured file patterns into the document search tables.
pub struct FileIndexer {
    conn: Arc<ParkingMutex<Connection>>,
    workspace_dir: PathBuf,
    patterns: Vec<String>,
}

impl FileIndexer {
    /// Create a new file indexer.
    ///
    /// `patterns` are glob strings relative to `workspace_dir`
    /// (e.g. `"rooms/*.md"`, `"MEMORY.md"`).
    pub fn new(
        conn: Arc<ParkingMutex<Connection>>,
        workspace_dir: &Path,
        patterns: Vec<String>,
    ) -> Self {
        Self {
            conn,
            workspace_dir: workspace_dir.to_path_buf(),
            patterns,
        }
    }

    /// Index all files matching the configured patterns.
    ///
    /// Returns total number of chunks indexed (0 for unchanged files).
    /// Errors on individual files are logged and skipped.
    pub fn index_configured_files(&self) -> Result<usize> {
        if self.patterns.is_empty() {
            return Ok(0);
        }

        let files = self.resolve_patterns();
        if files.is_empty() {
            debug!("No files matched indexed_paths patterns");
            return Ok(0);
        }

        let db = self.conn.lock();
        let mut total_chunks = 0;

        for path in &files {
            match doc_index::index_file(&db, path, "document") {
                Ok(chunks) => {
                    total_chunks += chunks;
                }
                Err(e) => {
                    warn!(path = %path.display(), "Failed to index file: {e}");
                }
            }
        }

        if total_chunks > 0 {
            info!(
                files = files.len(),
                chunks = total_chunks,
                "File indexing complete"
            );
        }

        Ok(total_chunks)
    }

    /// Resolve all glob patterns against the workspace directory.
    fn resolve_patterns(&self) -> Vec<PathBuf> {
        let mut results = Vec::new();

        for pattern in &self.patterns {
            let full_pattern = self.workspace_dir.join(pattern);
            let pattern_str = full_pattern.to_string_lossy().to_string();

            match glob::glob(&pattern_str) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(path) if path.is_file() => {
                                results.push(path);
                            }
                            Ok(_) => {} // skip directories
                            Err(e) => {
                                debug!("Glob entry error for pattern '{pattern}': {e}");
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("Invalid glob pattern '{pattern}': {e}");
                }
            }
        }

        results.sort();
        results.dedup();
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn test_conn() -> Arc<ParkingMutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .unwrap();
        doc_index::create_doc_tables(&conn).unwrap();
        Arc::new(ParkingMutex::new(conn))
    }

    fn write_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn empty_patterns_indexes_nothing() {
        let dir = TempDir::new().unwrap();
        let conn = test_conn();
        let indexer = FileIndexer::new(conn.clone(), dir.path(), vec![]);

        let chunks = indexer.index_configured_files().unwrap();
        assert_eq!(chunks, 0);
    }

    #[test]
    fn indexes_matching_files() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "README.md", "This is the readme with content.");
        write_file(dir.path(), "notes.txt", "Some text notes here.");

        let conn = test_conn();
        let indexer = FileIndexer::new(
            conn.clone(),
            dir.path(),
            vec!["*.md".to_string()],
        );

        let chunks = indexer.index_configured_files().unwrap();
        assert!(chunks >= 1);

        // Verify only .md was indexed, not .txt
        let db = conn.lock();
        let count = doc_index::count_docs(&db).unwrap();
        assert_eq!(count, chunks);

        let results = doc_index::search_docs(&db, "readme", 10).unwrap();
        assert_eq!(results.len(), 1);

        let results = doc_index::search_docs(&db, "text notes", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn indexes_subdirectory_patterns() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "rooms/general.md", "General room discussion.");
        write_file(dir.path(), "rooms/dev.md", "Development room notes.");
        write_file(dir.path(), "other.md", "Unrelated file with unique xyzzy content.");

        let conn = test_conn();
        let indexer = FileIndexer::new(
            conn.clone(),
            dir.path(),
            vec!["rooms/*.md".to_string()],
        );

        let chunks = indexer.index_configured_files().unwrap();
        assert!(chunks >= 2);

        let db = conn.lock();
        let results = doc_index::search_docs(&db, "discussion", 10).unwrap();
        assert_eq!(results.len(), 1);

        // "other.md" should not be indexed — unique keyword not found
        let results = doc_index::search_docs(&db, "xyzzy", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn skips_unchanged_on_reindex() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "test.md", "Stable content here.");

        let conn = test_conn();
        let indexer = FileIndexer::new(
            conn.clone(),
            dir.path(),
            vec!["*.md".to_string()],
        );

        let chunks1 = indexer.index_configured_files().unwrap();
        assert_eq!(chunks1, 1);

        // Re-index same content — should skip
        let chunks2 = indexer.index_configured_files().unwrap();
        assert_eq!(chunks2, 0);
    }

    #[test]
    fn re_indexes_changed_files() {
        let dir = TempDir::new().unwrap();
        let path = write_file(dir.path(), "test.md", "Original content here.");

        let conn = test_conn();
        let indexer = FileIndexer::new(
            conn.clone(),
            dir.path(),
            vec!["*.md".to_string()],
        );

        indexer.index_configured_files().unwrap();

        // Change content
        std::fs::write(&path, "Updated content now.").unwrap();
        let chunks = indexer.index_configured_files().unwrap();
        assert_eq!(chunks, 1);

        let db = conn.lock();
        let results = doc_index::search_docs(&db, "Updated", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn invalid_pattern_logged_not_fatal() {
        let dir = TempDir::new().unwrap();
        let conn = test_conn();
        let indexer = FileIndexer::new(
            conn.clone(),
            dir.path(),
            vec!["[invalid".to_string()],
        );

        // Should not error, just log warning
        let chunks = indexer.index_configured_files().unwrap();
        assert_eq!(chunks, 0);
    }

    #[test]
    fn no_matching_files_returns_zero() {
        let dir = TempDir::new().unwrap();
        let conn = test_conn();
        let indexer = FileIndexer::new(
            conn.clone(),
            dir.path(),
            vec!["*.xyz".to_string()],
        );

        let chunks = indexer.index_configured_files().unwrap();
        assert_eq!(chunks, 0);
    }

    #[test]
    fn multiple_patterns() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "README.md", "Project readme file.");
        write_file(dir.path(), "MEMORY.md", "Agent memory content.");
        write_file(dir.path(), "notes.txt", "Text notes here.");

        let conn = test_conn();
        let indexer = FileIndexer::new(
            conn.clone(),
            dir.path(),
            vec!["README.md".to_string(), "MEMORY.md".to_string()],
        );

        let chunks = indexer.index_configured_files().unwrap();
        assert_eq!(chunks, 2);

        let db = conn.lock();
        assert_eq!(doc_index::count_docs(&db).unwrap(), 2);
    }
}
