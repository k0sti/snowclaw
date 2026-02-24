use rusqlite::{Connection, Result as RusqliteResult, params};
use anyhow::{Result, Context};
use nostr_sdk::{Event, EventId, PublicKey};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct EventCache {
    db_path: std::path::PathBuf,
}

#[derive(Debug, Clone)]
pub struct CachedEvent {
    pub id: String,
    pub pubkey: String,
    pub created_at: i64,
    pub kind: u16,
    pub tags: String,
    pub content: String,
    pub sig: String,
    pub group_name: Option<String>,
    pub stored_at: String,
}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub total_events: i64,
    pub by_kind: std::collections::HashMap<u16, i64>,
    pub by_group: std::collections::HashMap<String, i64>,
    pub recent_24h: i64,
}

impl EventCache {
    pub async fn new(db_path: &Path) -> Result<Self> {
        let cache = Self {
            db_path: db_path.to_path_buf(),
        };
        cache.init_db().await?;
        Ok(cache)
    }

    async fn init_db(&self) -> Result<()> {
        let conn = Connection::open(&self.db_path)?;
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS events (
                id TEXT PRIMARY KEY,
                pubkey TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                kind INTEGER NOT NULL,
                tags TEXT NOT NULL,
                content TEXT NOT NULL,
                sig TEXT NOT NULL,
                group_name TEXT,
                stored_at TEXT NOT NULL
            )
            "#,
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_events_pubkey ON events(pubkey)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_events_kind ON events(kind)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_events_group ON events(group_name)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_events_created_at ON events(created_at)",
            [],
        )?;

        Ok(())
    }

    pub async fn store_event(&self, event: &Event, group_name: Option<&str>) -> Result<()> {
        let conn = Connection::open(&self.db_path)?;
        let stored_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
        
        conn.execute(
            r#"
            INSERT OR REPLACE INTO events 
            (id, pubkey, created_at, kind, tags, content, sig, group_name, stored_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                event.id.to_hex(),
                event.pubkey.to_hex(),
                event.created_at.as_secs() as i64,
                event.kind.as_u16(),
                serde_json::to_string(&event.tags).unwrap_or_default(),
                event.content,
                event.sig.to_hex(),
                group_name,
                stored_at
            ],
        )?;

        Ok(())
    }

    pub async fn get_event(&self, event_id: &EventId) -> Result<Option<CachedEvent>> {
        let conn = Connection::open(&self.db_path)?;
        let mut stmt = conn.prepare(
            "SELECT id, pubkey, created_at, kind, tags, content, sig, group_name, stored_at FROM events WHERE id = ?1"
        )?;

        let event_id_hex = event_id.to_hex();
        let row = stmt.query_row(params![event_id_hex], |row| {
            Ok(CachedEvent {
                id: row.get(0)?,
                pubkey: row.get(1)?,
                created_at: row.get(2)?,
                kind: row.get(3)?,
                tags: row.get(4)?,
                content: row.get(5)?,
                sig: row.get(6)?,
                group_name: row.get(7)?,
                stored_at: row.get(8)?,
            })
        });

        match row {
            Ok(event) => Ok(Some(event)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn get_stats(&self) -> Result<CacheStats> {
        let conn = Connection::open(&self.db_path)?;
        
        let total_events: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events",
            [],
            |row| row.get(0)
        )?;

        Ok(CacheStats {
            total_events,
            by_kind: std::collections::HashMap::new(), // Simplified for now
            by_group: std::collections::HashMap::new(), // Simplified for now
            recent_24h: 0, // Simplified for now
        })
    }
}