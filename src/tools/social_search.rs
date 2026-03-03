use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use parking_lot::Mutex;
use rusqlite::Connection;
use serde_json::json;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::warn;

use crate::memory::unified_search::{self, UnifiedHit};

/// Search social contacts, messages, and indexed documents from social.db.
///
/// This tool provides agent access to the Nostr channel's social memory
/// (contacts, chat messages, indexed documents) via `unified_recall`.
/// It opens social.db read-only — writes happen only in the Nostr channel.
pub struct SocialSearchTool {
    conn: Option<Arc<Mutex<Connection>>>,
}

impl SocialSearchTool {
    /// Create a new SocialSearchTool pointing at `social.db` in the given directory.
    ///
    /// If the file doesn't exist or can't be opened, the tool degrades gracefully
    /// (returns "no results" for all queries).
    pub fn new(config_dir: &Path) -> Self {
        let db_path = config_dir.join("social.db");
        let conn = Self::open_readonly(&db_path);
        Self { conn }
    }

    fn open_readonly(db_path: &PathBuf) -> Option<Arc<Mutex<Connection>>> {
        if !db_path.exists() {
            return None;
        }

        let conn = match Connection::open_with_flags(
            db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to open social.db for search: {e}");
                return None;
            }
        };

        if let Err(e) = conn.execute_batch("PRAGMA busy_timeout = 3000;") {
            warn!("Failed to set social.db pragmas: {e}");
        }

        Some(Arc::new(Mutex::new(conn)))
    }
}

#[async_trait]
impl Tool for SocialSearchTool {
    fn name(&self) -> &str {
        "social_search"
    }

    fn description(&self) -> &str {
        "Search social contacts, chat messages, and indexed documents. \
         Use this to find people, past conversations, or document content \
         from Nostr channels."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords or phrase to search for"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results to return (default: 10)"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'query' parameter"))?;

        #[allow(clippy::cast_possible_truncation)]
        let limit = args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(10, |v| v as usize);

        let Some(ref conn) = self.conn else {
            return Ok(ToolResult {
                success: true,
                output: "Social database not available (no social.db found).".into(),
                error: None,
            });
        };

        let hits = {
            let db = conn.lock();
            match unified_search::unified_recall(&db, query, limit) {
                Ok(h) => h,
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Social search failed: {e}")),
                    });
                }
            }
        };

        if hits.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No social results found matching that query.".into(),
                error: None,
            });
        }

        let mut output = format!("Found {} results:\n", hits.len());
        for hit in &hits {
            let _ = writeln!(output, "- [{}] {}", hit.source(), format_hit(hit));
        }

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

fn format_hit(hit: &UnifiedHit) -> String {
    match hit {
        UnifiedHit::Social(s) => {
            let mut out = format!("{} ({})", s.display_name, s.hex_pubkey);
            if let Some(ref notes) = s.notes_json {
                if let Ok(notes) = serde_json::from_str::<Vec<String>>(notes) {
                    if !notes.is_empty() {
                        let _ = write!(out, " — notes: {}", notes.join("; "));
                    }
                }
            }
            out
        }
        UnifiedHit::Message(m) => {
            let ts = chrono::DateTime::from_timestamp(m.created_at, 0)
                .map(|dt| dt.format("%Y-%m-%d").to_string())
                .unwrap_or_default();
            format!(
                "{} (from {} {})",
                m.content,
                &m.sender_hex[..8.min(m.sender_hex.len())],
                ts
            )
        }
        UnifiedHit::Document(d) => {
            format!("[{}] {}", d.path, d.content)
        }
        UnifiedHit::Memory(e) => {
            format!("{}: {}", e.key, e.content)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::doc_index;
    use crate::memory::message_index::{self, IndexableMessage};
    use crate::memory::social::{self, SocialNpub};

    fn test_conn() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .unwrap();
        social::create_social_tables(&conn).unwrap();
        message_index::create_message_tables(&conn).unwrap();
        doc_index::create_doc_tables(&conn).unwrap();
        Arc::new(Mutex::new(conn))
    }

    #[tokio::test]
    async fn no_db_returns_not_available() {
        let tool = SocialSearchTool { conn: None };
        let result = tool.execute(json!({"query": "test"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("not available"));
    }

    #[tokio::test]
    async fn empty_db_returns_no_results() {
        let tool = SocialSearchTool {
            conn: Some(test_conn()),
        };
        let result = tool.execute(json!({"query": "anything"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No social results"));
    }

    #[tokio::test]
    async fn finds_social_contact() {
        let conn = test_conn();
        {
            let db = conn.lock();
            social::upsert_npub(
                &db,
                &SocialNpub {
                    hex_pubkey: "aabbccdd".into(),
                    display_name: "RustDeveloper".into(),
                    first_seen: 1000,
                    first_seen_group: None,
                    last_interaction: 2000,
                    profile_json: None,
                    name_history_json: None,
                    notes_json: Some(r#"["likes Rust"]"#.into()),
                    owner_notes_json: None,
                    preferences_json: None,
                    is_owner: false,
                },
            )
            .unwrap();
        }

        let tool = SocialSearchTool { conn: Some(conn) };
        let result = tool
            .execute(json!({"query": "RustDeveloper"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("RustDeveloper"));
        assert!(result.output.contains("social"));
    }

    #[tokio::test]
    async fn finds_indexed_message() {
        let conn = test_conn();
        {
            let db = conn.lock();
            message_index::index_message(
                &db,
                &IndexableMessage {
                    event_id: "evt1".into(),
                    sender_hex: "aabbccdd".into(),
                    group_id: None,
                    content: "Discussion about async Rust patterns".into(),
                    created_at: 1700000000,
                    kind: 1,
                },
            )
            .unwrap();
        }

        let tool = SocialSearchTool { conn: Some(conn) };
        let result = tool
            .execute(json!({"query": "async Rust"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("async"));
        assert!(result.output.contains("message"));
    }

    #[tokio::test]
    async fn finds_indexed_document() {
        let conn = test_conn();
        {
            let db = conn.lock();
            doc_index::index_content(
                &db,
                "v://guide.md",
                "Getting started with systems programming",
                "document",
            )
            .unwrap();
        }

        let tool = SocialSearchTool { conn: Some(conn) };
        let result = tool
            .execute(json!({"query": "systems programming"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("systems programming"));
        assert!(result.output.contains("document"));
    }

    #[tokio::test]
    async fn missing_query_returns_error() {
        let tool = SocialSearchTool {
            conn: Some(test_conn()),
        };
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn respects_limit() {
        let conn = test_conn();
        {
            let db = conn.lock();
            for i in 0..10 {
                message_index::index_message(
                    &db,
                    &IndexableMessage {
                        event_id: format!("evt{i}"),
                        sender_hex: "aabb".into(),
                        group_id: None,
                        content: format!("common keyword discussion item {i}"),
                        created_at: 1700000000 + i,
                        kind: 1,
                    },
                )
                .unwrap();
            }
        }

        let tool = SocialSearchTool { conn: Some(conn) };
        let result = tool
            .execute(json!({"query": "common keyword", "limit": 3}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Found 3"));
    }
}
