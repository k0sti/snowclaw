//! Persistent seen-events store backed by SQLite.
//!
//! Tracks which Nostr event IDs have already been processed, so restarts
//! don't reprocess DMs from the subscription lookback window. Also stores
//! per-sender DM conversation history for context continuity.

use anyhow::{Context, Result};
use lru::LruCache;
use parking_lot::Mutex as SyncMutex;
use rusqlite::{params, Connection};
use std::collections::{HashMap, VecDeque};
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Default LRU cache capacity for seen event IDs.
const SEEN_CACHE_CAPACITY: usize = 2000;

/// Default max DM history messages per sender.
const DEFAULT_DM_HISTORY_SIZE: usize = 20;

/// Number of days of seen events to load on startup.
const STARTUP_LOAD_DAYS: u64 = 3;

/// A message in per-sender DM conversation history.
#[derive(Debug, Clone)]
pub struct DmHistoryMessage {
    pub sender_hex: String,
    pub sender_name: String,
    pub content: String,
    pub timestamp: u64,
    pub event_id: String,
    pub is_outgoing: bool,
}

/// Persistent event deduplication + DM conversation history.
///
/// Uses SQLite for durable storage with an in-memory LRU cache for fast lookups.
/// The SQLite connection uses `parking_lot::Mutex` (sync) to avoid holding
/// a non-Send `rusqlite::Connection` guard across `.await` points.
pub struct SeenEventsStore {
    conn: Arc<SyncMutex<Connection>>,
    cache: Arc<SyncMutex<LruCache<String, ()>>>,
    dm_history: Arc<RwLock<HashMap<String, VecDeque<DmHistoryMessage>>>>,
    dm_history_size: usize,
}

impl SeenEventsStore {
    /// Open (or create) the seen-events database at `dir/seen_events.db`.
    pub fn new(dir: &Path, dm_history_size: Option<usize>) -> Result<Self> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create seen-events dir: {}", dir.display()))?;

        let db_path = dir.join("seen_events.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("Failed to open seen-events DB: {}", db_path.display()))?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous  = NORMAL;",
        )?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS seen_events (
                event_id     TEXT PRIMARY KEY,
                kind         INTEGER NOT NULL,
                sender       TEXT NOT NULL,
                processed_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_seen_processed_at
                ON seen_events(processed_at);

            CREATE TABLE IF NOT EXISTS dm_history (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                sender_hex   TEXT NOT NULL,
                sender_name  TEXT NOT NULL,
                content      TEXT NOT NULL,
                timestamp    INTEGER NOT NULL,
                event_id     TEXT NOT NULL,
                is_outgoing  INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_dm_history_sender
                ON dm_history(sender_hex, timestamp);",
        )?;

        let max_size = dm_history_size.unwrap_or(DEFAULT_DM_HISTORY_SIZE);

        let store = Self {
            conn: Arc::new(SyncMutex::new(conn)),
            cache: Arc::new(SyncMutex::new(LruCache::new(
                NonZeroUsize::new(SEEN_CACHE_CAPACITY).unwrap(),
            ))),
            dm_history: Arc::new(RwLock::new(HashMap::new())),
            dm_history_size: max_size,
        };

        Ok(store)
    }

    /// Load recent seen event IDs (last N days) into the LRU cache.
    /// Also load DM history into memory.
    pub async fn load_recent(&self) -> Result<()> {
        let cutoff = now_secs() - (STARTUP_LOAD_DAYS * 86400);

        // Collect all data from SQLite synchronously (no .await while holding conn)
        let (ids, rows) = {
            let conn = self.conn.lock();

            let mut stmt = conn
                .prepare("SELECT event_id FROM seen_events WHERE processed_at >= ?1")?;
            let ids: Vec<String> = stmt
                .query_map(params![cutoff as i64], |row| row.get(0))?
                .filter_map(|r| r.ok())
                .collect();

            let mut stmt2 = conn.prepare(
                "SELECT sender_hex, sender_name, content, timestamp, event_id, is_outgoing
                 FROM dm_history
                 WHERE timestamp >= ?1
                 ORDER BY timestamp ASC",
            )?;
            let rows: Vec<DmHistoryMessage> = stmt2
                .query_map(params![cutoff as i64], |row| {
                    Ok(DmHistoryMessage {
                        sender_hex: row.get(0)?,
                        sender_name: row.get(1)?,
                        content: row.get(2)?,
                        timestamp: row.get(3)?,
                        event_id: row.get(4)?,
                        is_outgoing: row.get::<_, i32>(5)? != 0,
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();

            (ids, rows)
        };

        let count = ids.len();
        {
            let mut cache = self.cache.lock();
            for id in ids {
                cache.put(id, ());
            }
        }

        let mut dm_history = self.dm_history.write().await;
        for msg in rows {
            let key = msg.sender_hex.clone();
            let buf = dm_history
                .entry(key)
                .or_insert_with(VecDeque::new);
            buf.push_back(msg);
            while buf.len() > self.dm_history_size {
                buf.pop_front();
            }
        }

        tracing::info!(
            "Loaded {} seen event IDs and {} DM conversations from SQLite",
            count,
            dm_history.len()
        );
        Ok(())
    }

    /// Check whether an event has already been seen (cache first, then SQLite).
    pub async fn is_seen(&self, event_id: &str) -> bool {
        // Fast path: check LRU cache (sync lock, no await)
        if self.cache.lock().contains(event_id) {
            return true;
        }

        // Slow path: check SQLite (sync lock, no await)
        let found = {
            let conn = self.conn.lock();
            conn.query_row(
                "SELECT 1 FROM seen_events WHERE event_id = ?1",
                params![event_id],
                |_| Ok(true),
            )
            .unwrap_or(false)
        };

        if found {
            self.cache.lock().put(event_id.to_string(), ());
        }

        found
    }

    /// Mark an event as seen (insert into both cache and SQLite).
    pub async fn mark_seen(&self, event_id: &str, kind: u16, sender: &str) {
        self.cache.lock().put(event_id.to_string(), ());

        let conn = self.conn.lock();
        if let Err(e) = conn.execute(
            "INSERT OR IGNORE INTO seen_events (event_id, kind, sender, processed_at) VALUES (?1, ?2, ?3, ?4)",
            params![event_id, kind as i64, sender, now_secs() as i64],
        ) {
            tracing::warn!("Failed to persist seen event {}: {e}", &event_id[..8.min(event_id.len())]);
        }
    }

    /// Mark multiple events as seen in a batch (for backfill).
    pub async fn mark_seen_batch(&self, events: &[(String, u16, String)]) {
        {
            let mut cache = self.cache.lock();
            for (id, _, _) in events {
                cache.put(id.clone(), ());
            }
        }

        let conn = self.conn.lock();
        let tx = match conn.unchecked_transaction() {
            Ok(tx) => tx,
            Err(e) => {
                tracing::warn!("Failed to start batch transaction: {e}");
                return;
            }
        };

        for (id, kind, sender) in events {
            if let Err(e) = tx.execute(
                "INSERT OR IGNORE INTO seen_events (event_id, kind, sender, processed_at) VALUES (?1, ?2, ?3, ?4)",
                params![id, *kind as i64, sender, now_secs() as i64],
            ) {
                tracing::warn!("Failed to batch-insert seen event: {e}");
            }
        }

        if let Err(e) = tx.commit() {
            tracing::warn!("Failed to commit seen-events batch: {e}");
        }
    }

    /// Add a DM message to per-sender conversation history (both memory and SQLite).
    pub async fn push_dm_history(&self, msg: DmHistoryMessage) {
        let sender_key = msg.sender_hex.clone();

        // Persist to SQLite (sync lock, no await)
        {
            let conn = self.conn.lock();
            if let Err(e) = conn.execute(
                "INSERT INTO dm_history (sender_hex, sender_name, content, timestamp, event_id, is_outgoing)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    msg.sender_hex,
                    msg.sender_name,
                    msg.content,
                    msg.timestamp as i64,
                    msg.event_id,
                    msg.is_outgoing as i32,
                ],
            ) {
                tracing::warn!("Failed to persist DM history: {e}");
            }
        }

        // Add to in-memory ring buffer
        let mut history = self.dm_history.write().await;
        let buf = history
            .entry(sender_key)
            .or_insert_with(VecDeque::new);
        buf.push_back(msg);
        while buf.len() > self.dm_history_size {
            buf.pop_front();
        }
    }

    /// Format DM conversation history for a given sender as LLM context.
    /// Excludes the current event to avoid duplication.
    pub async fn format_dm_context(&self, sender_hex: &str, exclude_event_id: &str) -> String {
        let history = self.dm_history.read().await;
        let buf = match history.get(sender_hex) {
            Some(b) if !b.is_empty() => b,
            _ => return String::new(),
        };

        let mut ctx = String::from("[Recent DM conversation context]\n");
        let mut has_content = false;

        for msg in buf.iter() {
            if msg.event_id == exclude_event_id {
                continue;
            }
            let direction = if msg.is_outgoing { "you" } else { &msg.sender_name };
            ctx.push_str(&format!("<{}> {}\n", direction, msg.content));
            has_content = true;
        }

        if !has_content {
            return String::new();
        }
        ctx.push('\n');
        ctx
    }

    /// Prune old entries from SQLite (older than N days).
    pub async fn prune(&self, older_than_days: u64) -> Result<()> {
        let cutoff = now_secs() - (older_than_days * 86400);
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM seen_events WHERE processed_at < ?1",
            params![cutoff as i64],
        )?;
        conn.execute(
            "DELETE FROM dm_history WHERE timestamp < ?1",
            params![cutoff as i64],
        )?;
        Ok(())
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store() -> (SeenEventsStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let store = SeenEventsStore::new(dir.path(), Some(5)).unwrap();
        (store, dir)
    }

    #[tokio::test]
    async fn mark_and_check_seen() {
        let (store, _dir) = test_store();

        assert!(!store.is_seen("abc123").await);
        store.mark_seen("abc123", 1059, "sender_hex").await;
        assert!(store.is_seen("abc123").await);
    }

    #[tokio::test]
    async fn duplicate_insert_is_ignored() {
        let (store, _dir) = test_store();

        store.mark_seen("abc123", 1059, "sender_hex").await;
        store.mark_seen("abc123", 1059, "sender_hex").await;
        assert!(store.is_seen("abc123").await);
    }

    #[tokio::test]
    async fn batch_mark_seen() {
        let (store, _dir) = test_store();

        let batch = vec![
            ("ev1".to_string(), 1059u16, "s1".to_string()),
            ("ev2".to_string(), 1059u16, "s2".to_string()),
            ("ev3".to_string(), 4u16, "s3".to_string()),
        ];
        store.mark_seen_batch(&batch).await;

        assert!(store.is_seen("ev1").await);
        assert!(store.is_seen("ev2").await);
        assert!(store.is_seen("ev3").await);
        assert!(!store.is_seen("ev4").await);
    }

    #[tokio::test]
    async fn dm_history_ring_buffer() {
        let (store, _dir) = test_store();

        // Push 7 messages (buffer size = 5)
        for i in 0..7 {
            store.push_dm_history(DmHistoryMessage {
                sender_hex: "sender1".to_string(),
                sender_name: "Alice".to_string(),
                content: format!("msg {i}"),
                timestamp: 1000 + i,
                event_id: format!("ev{i}"),
                is_outgoing: i % 2 == 0,
            }).await;
        }

        let history = store.dm_history.read().await;
        let buf = history.get("sender1").unwrap();
        assert_eq!(buf.len(), 5);
        // Oldest should be msg 2 (0 and 1 evicted)
        assert_eq!(buf.front().unwrap().content, "msg 2");
        assert_eq!(buf.back().unwrap().content, "msg 6");
    }

    #[tokio::test]
    async fn format_dm_context_excludes_current() {
        let (store, _dir) = test_store();

        store.push_dm_history(DmHistoryMessage {
            sender_hex: "sender1".to_string(),
            sender_name: "Alice".to_string(),
            content: "hello".to_string(),
            timestamp: 1000,
            event_id: "ev1".to_string(),
            is_outgoing: false,
        }).await;
        store.push_dm_history(DmHistoryMessage {
            sender_hex: "sender1".to_string(),
            sender_name: "Alice".to_string(),
            content: "hi back".to_string(),
            timestamp: 1001,
            event_id: "ev2".to_string(),
            is_outgoing: true,
        }).await;
        store.push_dm_history(DmHistoryMessage {
            sender_hex: "sender1".to_string(),
            sender_name: "Alice".to_string(),
            content: "current msg".to_string(),
            timestamp: 1002,
            event_id: "ev3".to_string(),
            is_outgoing: false,
        }).await;

        let ctx = store.format_dm_context("sender1", "ev3").await;
        assert!(ctx.contains("<Alice> hello"));
        assert!(ctx.contains("<you> hi back"));
        assert!(!ctx.contains("current msg"));
    }

    #[tokio::test]
    async fn empty_context_returns_empty_string() {
        let (store, _dir) = test_store();
        let ctx = store.format_dm_context("nobody", "ev1").await;
        assert!(ctx.is_empty());
    }

    #[tokio::test]
    async fn load_recent_populates_cache_and_history() {
        let dir = TempDir::new().unwrap();

        // Create store, add data, drop it
        {
            let store = SeenEventsStore::new(dir.path(), Some(10)).unwrap();
            store.mark_seen("persistent_ev", 1059, "sender_hex").await;
            store.push_dm_history(DmHistoryMessage {
                sender_hex: "sender1".to_string(),
                sender_name: "Alice".to_string(),
                content: "persisted msg".to_string(),
                timestamp: now_secs(),
                event_id: "persistent_ev".to_string(),
                is_outgoing: false,
            }).await;
        }

        // Reopen: cache is empty until load_recent
        let store = SeenEventsStore::new(dir.path(), Some(10)).unwrap();
        assert!(!store.cache.lock().contains("persistent_ev"));

        store.load_recent().await.unwrap();

        assert!(store.cache.lock().contains("persistent_ev"));
        let history = store.dm_history.read().await;
        assert_eq!(history.get("sender1").unwrap().len(), 1);
    }

    #[tokio::test]
    async fn prune_removes_old_entries() {
        let (store, _dir) = test_store();

        // Insert with old timestamp directly
        {
            let conn = store.conn.lock();
            conn.execute(
                "INSERT INTO seen_events (event_id, kind, sender, processed_at) VALUES (?1, ?2, ?3, ?4)",
                params!["old_ev", 1059i64, "sender", 1000i64],
            ).unwrap();
            conn.execute(
                "INSERT INTO dm_history (sender_hex, sender_name, content, timestamp, event_id, is_outgoing) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params!["sender", "Alice", "old msg", 1000i64, "old_ev", 0i32],
            ).unwrap();
        }

        store.prune(1).await.unwrap();

        let conn = store.conn.lock();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM seen_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
