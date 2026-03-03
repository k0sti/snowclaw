//! Agent lesson tool — stores behavioral learnings for later Nostr publication.
//!
//! Lessons are stored in `social.db` in the `agent_lessons` table. The Nostr
//! channel periodically publishes unpublished lessons as kind 4129 events
//! referencing the agent definition.

use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use parking_lot::Mutex;
use rusqlite::Connection;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use tracing::warn;

/// Create the `agent_lessons` table if it doesn't exist.
pub fn create_lesson_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_lessons (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            published INTEGER NOT NULL DEFAULT 0,
            nostr_event_id TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_lessons_unpublished
            ON agent_lessons(published) WHERE published = 0;",
    )
}

/// Insert a lesson into the database. Returns the row id.
pub fn store_lesson(conn: &Connection, content: &str) -> rusqlite::Result<i64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    conn.execute(
        "INSERT INTO agent_lessons (content, created_at) VALUES (?1, ?2)",
        rusqlite::params![content, now as i64],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Fetch all unpublished lessons (id, content, created_at).
pub fn fetch_unpublished(conn: &Connection) -> rusqlite::Result<Vec<(i64, String, i64)>> {
    let mut stmt =
        conn.prepare("SELECT id, content, created_at FROM agent_lessons WHERE published = 0")?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Mark a lesson as published with the given Nostr event id.
pub fn mark_published(conn: &Connection, id: i64, event_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE agent_lessons SET published = 1, nostr_event_id = ?1 WHERE id = ?2",
        rusqlite::params![event_id, id],
    )?;
    Ok(())
}

/// Tool that lets the agent record a behavioral lesson.
///
/// Lessons are persisted locally in `social.db` and later published to the
/// Nostr relay as kind 4129 events by the Nostr channel.
pub struct AgentLessonTool {
    conn: Option<Arc<Mutex<Connection>>>,
}

impl AgentLessonTool {
    /// Create a new `AgentLessonTool` pointing at `social.db` in the given config dir.
    ///
    /// Opens read-write so it can insert lessons. If the file doesn't exist yet
    /// (no Nostr channel configured), falls back gracefully.
    pub fn new(config_dir: &Path) -> Self {
        let db_path = config_dir.join("social.db");
        let conn = Self::open_rw(&db_path);
        Self { conn }
    }

    fn open_rw(db_path: &std::path::PathBuf) -> Option<Arc<Mutex<Connection>>> {
        // Create parent dir if needed
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let conn = match Connection::open(db_path) {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to open social.db for agent lessons: {e}");
                return None;
            }
        };

        if let Err(e) = conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;",
        ) {
            warn!("Failed to set social.db pragmas: {e}");
        }

        if let Err(e) = create_lesson_tables(&conn) {
            warn!("Failed to create agent_lessons table: {e}");
            return None;
        }

        Some(Arc::new(Mutex::new(conn)))
    }
}

#[async_trait]
impl Tool for AgentLessonTool {
    fn name(&self) -> &str {
        "agent_lesson"
    }

    fn description(&self) -> &str {
        "Record a behavioral lesson the agent has learned. Lessons are published \
         to the Nostr relay as kind 4129 events. Use this when you discover a \
         reusable insight about user preferences, domain patterns, or effective \
         interaction strategies."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "lesson": {
                    "type": "string",
                    "description": "The behavioral lesson or insight to record"
                }
            },
            "required": ["lesson"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let lesson = args
            .get("lesson")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'lesson' parameter"))?;

        if lesson.trim().is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Lesson content cannot be empty".into()),
            });
        }

        let Some(ref conn) = self.conn else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Social database not available — lessons cannot be stored.".into()),
            });
        };

        let id = {
            let db = conn.lock();
            match store_lesson(&db, lesson) {
                Ok(id) => id,
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to store lesson: {e}")),
                    });
                }
            }
        };

        Ok(ToolResult {
            success: true,
            output: format!("Lesson recorded (id={id}). It will be published to the Nostr relay as a kind 4129 event."),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")
            .unwrap();
        create_lesson_tables(&conn).unwrap();
        Arc::new(Mutex::new(conn))
    }

    #[tokio::test]
    async fn store_and_retrieve_lesson() {
        let conn = test_conn();
        {
            let db = conn.lock();
            let id = store_lesson(&db, "Users prefer concise responses").unwrap();
            assert_eq!(id, 1);

            let lessons = fetch_unpublished(&db).unwrap();
            assert_eq!(lessons.len(), 1);
            assert_eq!(lessons[0].1, "Users prefer concise responses");
        }
    }

    #[tokio::test]
    async fn mark_published_removes_from_unpublished() {
        let conn = test_conn();
        {
            let db = conn.lock();
            let id = store_lesson(&db, "Test lesson").unwrap();
            mark_published(&db, id, "abc123").unwrap();

            let lessons = fetch_unpublished(&db).unwrap();
            assert!(lessons.is_empty());
        }
    }

    #[tokio::test]
    async fn tool_stores_lesson() {
        let tool = AgentLessonTool {
            conn: Some(test_conn()),
        };
        let result = tool
            .execute(json!({"lesson": "Relay config usually means NIP-29 group setup"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Lesson recorded"));
    }

    #[tokio::test]
    async fn tool_rejects_empty_lesson() {
        let tool = AgentLessonTool {
            conn: Some(test_conn()),
        };
        let result = tool.execute(json!({"lesson": "  "})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("empty"));
    }

    #[tokio::test]
    async fn tool_no_db_returns_error() {
        let tool = AgentLessonTool { conn: None };
        let result = tool
            .execute(json!({"lesson": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("not available"));
    }

    #[tokio::test]
    async fn tool_missing_param_returns_error() {
        let tool = AgentLessonTool {
            conn: Some(test_conn()),
        };
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }
}
