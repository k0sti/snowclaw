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
    pub async fn new(db_path: impl AsRef<Path>) -> Result<Self> {
        let cache = Self {
            db_path: db_path.as_ref().to_path_buf(),
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
                event.sig.to_string(),
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

    pub async fn has_by_hex(&self, event_id_hex: &str) -> Result<bool> {
        let conn = Connection::open(&self.db_path)?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE id = ?1",
            params![event_id_hex],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn store_raw(
        &self,
        id: &str,
        pubkey: &str,
        created_at: i64,
        kind: i64,
        tags: &str,
        content: &str,
        sig: &str,
        group_name: Option<&str>,
    ) -> Result<()> {
        let conn = Connection::open(&self.db_path)?;
        let stored_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
        conn.execute(
            r#"INSERT OR REPLACE INTO events 
            (id, pubkey, created_at, kind, tags, content, sig, group_name, stored_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
            params![id, pubkey, created_at, kind, tags, content, sig, group_name, stored_at],
        )?;
        Ok(())
    }

    pub async fn query(
        &self,
        group: Option<&str>,
        author: Option<&nostr_sdk::PublicKey>,
        since: Option<i64>,
        limit: Option<i64>,
    ) -> Result<Vec<CachedEvent>> {
        let conn = Connection::open(&self.db_path)?;
        let mut sql = String::from(
            "SELECT id, pubkey, created_at, kind, tags, content, sig, group_name, stored_at FROM events WHERE 1=1"
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(g) = group {
            sql.push_str(&format!(" AND group_name = ?{}", param_values.len() + 1));
            param_values.push(Box::new(g.to_string()));
        }
        if let Some(a) = author {
            sql.push_str(&format!(" AND pubkey = ?{}", param_values.len() + 1));
            param_values.push(Box::new(a.to_hex()));
        }
        if let Some(s) = since {
            sql.push_str(&format!(" AND created_at >= ?{}", param_values.len() + 1));
            param_values.push(Box::new(s));
        }
        sql.push_str(" ORDER BY created_at DESC");
        if let Some(l) = limit {
            sql.push_str(&format!(" LIMIT ?{}", param_values.len() + 1));
            param_values.push(Box::new(l));
        }

        let mut stmt = conn.prepare(&sql)?;
        let params_ref: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(params_ref.as_slice(), |row| {
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
        })?;

        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    pub async fn get(&self, event_id: &EventId) -> Result<Option<CachedEvent>> {
        self.get_event(event_id).await
    }

    pub async fn stats(&self) -> Result<CacheStats> {
        self.get_stats().await
    }

    pub async fn cleanup(&self, retention_days: u32) -> Result<usize> {
        let conn = Connection::open(&self.db_path)?;
        let cutoff = chrono::Utc::now().timestamp() - (retention_days as i64 * 86400);
        let deleted = conn.execute(
            "DELETE FROM events WHERE created_at < ?1",
            params![cutoff],
        )?;
        Ok(deleted)
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
            by_kind: std::collections::HashMap::new(),
            by_group: std::collections::HashMap::new(),
            recent_24h: 0,
        })
    }
}
