//! Social memory: per-npub and per-group context stored in SQLite.
//!
//! Provides structured storage for Nostr contact profiles and group metadata,
//! replacing the in-memory HashMap + JSON file approach in `nostr_memory.rs`.
//! All operations are synchronous and take `&Connection` — async wrappers
//! live in `nostr_sqlite.rs`.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use tracing::debug;

// ── Data structures ──────────────────────────────────────────────

/// A social contact's profile and context, stored in `social_npubs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialNpub {
    pub hex_pubkey: String,
    pub display_name: String,
    pub first_seen: i64,
    pub first_seen_group: Option<String>,
    pub last_interaction: i64,
    /// Full kind 0 profile metadata as JSON.
    pub profile_json: Option<String>,
    /// Name history as JSON: `[(timestamp, name), ...]`.
    pub name_history_json: Option<String>,
    /// Agent's observations about this contact.
    pub notes_json: Option<String>,
    /// Owner-provided notes about this contact.
    pub owner_notes_json: Option<String>,
    /// Per-contact preferences as JSON `HashMap<String, String>` (language, verbosity, etc.).
    pub preferences_json: Option<String>,
    /// Whether this contact is an owner of the agent.
    pub is_owner: bool,
}

/// A social group's metadata, stored in `social_groups`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialGroup {
    pub group_id: String,
    pub purpose: Option<String>,
    /// Member hex pubkeys as JSON array.
    pub members_json: Option<String>,
    /// Agent's observations about this group.
    pub notes_json: Option<String>,
    pub last_activity: i64,
}

// ── Schema ───────────────────────────────────────────────────────

/// Create social memory tables and FTS5 index in the given connection.
pub fn create_social_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "-- Social profiles
        CREATE TABLE IF NOT EXISTS social_npubs (
            hex_pubkey TEXT PRIMARY KEY,
            display_name TEXT NOT NULL,
            first_seen INTEGER NOT NULL,
            first_seen_group TEXT,
            last_interaction INTEGER NOT NULL,
            profile_json TEXT,
            name_history_json TEXT,
            notes_json TEXT,
            owner_notes_json TEXT,
            preferences_json TEXT,
            is_owner INTEGER DEFAULT 0
        );

        -- Social groups
        CREATE TABLE IF NOT EXISTS social_groups (
            group_id TEXT PRIMARY KEY,
            purpose TEXT,
            members_json TEXT,
            notes_json TEXT,
            last_activity INTEGER NOT NULL
        );

        -- FTS5 index over social data
        CREATE VIRTUAL TABLE IF NOT EXISTS social_fts USING fts5(
            hex_pubkey,
            display_name,
            notes,
            owner_notes,
            content='social_npubs',
            content_rowid='rowid'
        );

        -- FTS5 sync triggers
        CREATE TRIGGER IF NOT EXISTS social_npubs_ai AFTER INSERT ON social_npubs BEGIN
            INSERT INTO social_fts(rowid, hex_pubkey, display_name, notes, owner_notes)
            VALUES (new.rowid, new.hex_pubkey, new.display_name,
                    COALESCE(new.notes_json, ''), COALESCE(new.owner_notes_json, ''));
        END;
        CREATE TRIGGER IF NOT EXISTS social_npubs_ad AFTER DELETE ON social_npubs BEGIN
            INSERT INTO social_fts(social_fts, rowid, hex_pubkey, display_name, notes, owner_notes)
            VALUES ('delete', old.rowid, old.hex_pubkey, old.display_name,
                    COALESCE(old.notes_json, ''), COALESCE(old.owner_notes_json, ''));
        END;
        CREATE TRIGGER IF NOT EXISTS social_npubs_au AFTER UPDATE ON social_npubs BEGIN
            INSERT INTO social_fts(social_fts, rowid, hex_pubkey, display_name, notes, owner_notes)
            VALUES ('delete', old.rowid, old.hex_pubkey, old.display_name,
                    COALESCE(old.notes_json, ''), COALESCE(old.owner_notes_json, ''));
            INSERT INTO social_fts(rowid, hex_pubkey, display_name, notes, owner_notes)
            VALUES (new.rowid, new.hex_pubkey, new.display_name,
                    COALESCE(new.notes_json, ''), COALESCE(new.owner_notes_json, ''));
        END;",
    )
    .context("failed to create social tables")?;

    Ok(())
}

// ── CRUD operations ──────────────────────────────────────────────

/// Upsert an npub profile. Updates `last_interaction` and display_name on conflict.
pub fn upsert_npub(conn: &Connection, npub: &SocialNpub) -> Result<()> {
    conn.execute(
        "INSERT INTO social_npubs (
            hex_pubkey, display_name, first_seen, first_seen_group,
            last_interaction, profile_json, name_history_json, notes_json, owner_notes_json,
            preferences_json, is_owner
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
        ON CONFLICT(hex_pubkey) DO UPDATE SET
            display_name = excluded.display_name,
            last_interaction = excluded.last_interaction,
            profile_json = COALESCE(excluded.profile_json, social_npubs.profile_json),
            name_history_json = COALESCE(excluded.name_history_json, social_npubs.name_history_json),
            notes_json = COALESCE(excluded.notes_json, social_npubs.notes_json),
            owner_notes_json = COALESCE(excluded.owner_notes_json, social_npubs.owner_notes_json),
            preferences_json = COALESCE(excluded.preferences_json, social_npubs.preferences_json),
            is_owner = MAX(social_npubs.is_owner, excluded.is_owner)",
        params![
            npub.hex_pubkey,
            npub.display_name,
            npub.first_seen,
            npub.first_seen_group,
            npub.last_interaction,
            npub.profile_json,
            npub.name_history_json,
            npub.notes_json,
            npub.owner_notes_json,
            npub.preferences_json,
            npub.is_owner,
        ],
    )?;

    debug!(hex = %npub.hex_pubkey, name = %npub.display_name, "upserted social npub");
    Ok(())
}

/// Get an npub by hex pubkey.
pub fn get_npub(conn: &Connection, hex_pubkey: &str) -> Result<Option<SocialNpub>> {
    let mut stmt = conn.prepare(
        "SELECT hex_pubkey, display_name, first_seen, first_seen_group,
                last_interaction, profile_json, name_history_json, notes_json, owner_notes_json,
                preferences_json, is_owner
         FROM social_npubs WHERE hex_pubkey = ?1",
    )?;

    let mut rows = stmt.query_map(params![hex_pubkey], |row| {
        let is_owner_int: i64 = row.get(10)?;
        Ok(SocialNpub {
            hex_pubkey: row.get(0)?,
            display_name: row.get(1)?,
            first_seen: row.get(2)?,
            first_seen_group: row.get(3)?,
            last_interaction: row.get(4)?,
            profile_json: row.get(5)?,
            name_history_json: row.get(6)?,
            notes_json: row.get(7)?,
            owner_notes_json: row.get(8)?,
            preferences_json: row.get(9)?,
            is_owner: is_owner_int != 0,
        })
    })?;

    match rows.next() {
        Some(Ok(npub)) => Ok(Some(npub)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// Add a note to an npub's notes or owner_notes JSON array.
pub fn add_npub_note(
    conn: &Connection,
    hex_pubkey: &str,
    note: &str,
    is_owner: bool,
) -> Result<()> {
    let column = if is_owner {
        "owner_notes_json"
    } else {
        "notes_json"
    };

    // Read current notes
    let sql = format!(
        "SELECT {column} FROM social_npubs WHERE hex_pubkey = ?1"
    );
    let current: Option<String> = conn
        .query_row(&sql, params![hex_pubkey], |row| row.get(0))
        .ok()
        .flatten();

    let mut notes: Vec<String> = current
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();

    notes.push(note.to_string());
    let updated = serde_json::to_string(&notes)?;

    let update_sql = format!(
        "UPDATE social_npubs SET {column} = ?1 WHERE hex_pubkey = ?2"
    );
    conn.execute(&update_sql, params![updated, hex_pubkey])?;

    debug!(hex = %hex_pubkey, is_owner, "added social note");
    Ok(())
}

/// Add a note to a group's notes JSON array.
pub fn add_group_note(conn: &Connection, group_id: &str, note: &str) -> Result<()> {
    let current: Option<String> = conn
        .query_row(
            "SELECT notes_json FROM social_groups WHERE group_id = ?1",
            params![group_id],
            |row| row.get(0),
        )
        .ok()
        .flatten();

    let mut notes: Vec<String> = current
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();

    notes.push(note.to_string());
    let updated = serde_json::to_string(&notes)?;

    conn.execute(
        "UPDATE social_groups SET notes_json = ?1 WHERE group_id = ?2",
        params![updated, group_id],
    )?;

    debug!(group_id = %group_id, "added group note");
    Ok(())
}

/// Set a group's purpose.
pub fn set_group_purpose(conn: &Connection, group_id: &str, purpose: &str) -> Result<()> {
    conn.execute(
        "UPDATE social_groups SET purpose = ?1 WHERE group_id = ?2",
        params![purpose, group_id],
    )?;

    debug!(group_id = %group_id, "set group purpose");
    Ok(())
}

/// Record a member in a group's members JSON array (deduplicates).
pub fn record_group_member(
    conn: &Connection,
    group_id: &str,
    hex_pubkey: &str,
) -> Result<()> {
    let current: Option<String> = conn
        .query_row(
            "SELECT members_json FROM social_groups WHERE group_id = ?1",
            params![group_id],
            |row| row.get(0),
        )
        .ok()
        .flatten();

    let mut members: Vec<String> = current
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();

    if !members.contains(&hex_pubkey.to_string()) {
        members.push(hex_pubkey.to_string());
        let updated = serde_json::to_string(&members)?;

        conn.execute(
            "UPDATE social_groups SET members_json = ?1 WHERE group_id = ?2",
            params![updated, group_id],
        )?;

        debug!(group_id = %group_id, hex = %hex_pubkey, "recorded group member");
    }

    Ok(())
}

/// Update last_interaction and optionally track a name change for an npub.
pub fn touch_npub(
    conn: &Connection,
    hex_pubkey: &str,
    display_name: &str,
    timestamp: i64,
) -> Result<bool> {
    // Check if name changed
    let current_name: Option<String> = conn
        .query_row(
            "SELECT display_name FROM social_npubs WHERE hex_pubkey = ?1",
            params![hex_pubkey],
            |row| row.get(0),
        )
        .ok();

    if let Some(ref old_name) = current_name {
        if old_name != display_name {
            // Append to name history
            let history_json: Option<String> = conn
                .query_row(
                    "SELECT name_history_json FROM social_npubs WHERE hex_pubkey = ?1",
                    params![hex_pubkey],
                    |row| row.get(0),
                )
                .ok()
                .flatten();

            let mut history: Vec<(i64, String)> = history_json
                .as_deref()
                .and_then(|json| serde_json::from_str(json).ok())
                .unwrap_or_default();

            history.push((timestamp, old_name.clone()));
            let updated = serde_json::to_string(&history)?;

            conn.execute(
                "UPDATE social_npubs SET display_name = ?1, last_interaction = ?2, name_history_json = ?3 WHERE hex_pubkey = ?4",
                params![display_name, timestamp, updated, hex_pubkey],
            )?;
        } else {
            conn.execute(
                "UPDATE social_npubs SET last_interaction = ?1 WHERE hex_pubkey = ?2",
                params![timestamp, hex_pubkey],
            )?;
        }
        Ok(false) // not new
    } else {
        Ok(true) // doesn't exist yet — caller should upsert
    }
}

/// List all known npubs.
pub fn list_npubs(conn: &Connection) -> Result<Vec<SocialNpub>> {
    let mut stmt = conn.prepare(
        "SELECT hex_pubkey, display_name, first_seen, first_seen_group,
                last_interaction, profile_json, name_history_json, notes_json, owner_notes_json,
                preferences_json, is_owner
         FROM social_npubs ORDER BY last_interaction DESC",
    )?;

    let rows = stmt.query_map([], |row| {
        let is_owner_int: i64 = row.get(10)?;
        Ok(SocialNpub {
            hex_pubkey: row.get(0)?,
            display_name: row.get(1)?,
            first_seen: row.get(2)?,
            first_seen_group: row.get(3)?,
            last_interaction: row.get(4)?,
            profile_json: row.get(5)?,
            name_history_json: row.get(6)?,
            notes_json: row.get(7)?,
            owner_notes_json: row.get(8)?,
            preferences_json: row.get(9)?,
            is_owner: is_owner_int != 0,
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Upsert a social group.
pub fn upsert_group(conn: &Connection, group: &SocialGroup) -> Result<()> {
    conn.execute(
        "INSERT INTO social_groups (group_id, purpose, members_json, notes_json, last_activity)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(group_id) DO UPDATE SET
            purpose = COALESCE(excluded.purpose, social_groups.purpose),
            members_json = COALESCE(excluded.members_json, social_groups.members_json),
            notes_json = COALESCE(excluded.notes_json, social_groups.notes_json),
            last_activity = excluded.last_activity",
        params![
            group.group_id,
            group.purpose,
            group.members_json,
            group.notes_json,
            group.last_activity,
        ],
    )?;

    debug!(group_id = %group.group_id, "upserted social group");
    Ok(())
}

/// Get a social group by ID.
pub fn get_group(conn: &Connection, group_id: &str) -> Result<Option<SocialGroup>> {
    let mut stmt = conn.prepare(
        "SELECT group_id, purpose, members_json, notes_json, last_activity
         FROM social_groups WHERE group_id = ?1",
    )?;

    let mut rows = stmt.query_map(params![group_id], |row| {
        Ok(SocialGroup {
            group_id: row.get(0)?,
            purpose: row.get(1)?,
            members_json: row.get(2)?,
            notes_json: row.get(3)?,
            last_activity: row.get(4)?,
        })
    })?;

    match rows.next() {
        Some(Ok(group)) => Ok(Some(group)),
        Some(Err(e)) => Err(e.into()),
        None => Ok(None),
    }
}

/// Build concise LLM context string for a sender in a group.
///
/// Returns a formatted string suitable for prompt injection with group and
/// sender context from social memory.
pub fn build_social_context(
    conn: &Connection,
    sender_hex: &str,
    group_id: &str,
) -> String {
    let mut ctx = String::new();

    // Group context
    if let Ok(Some(group)) = get_group(conn, group_id) {
        if let Some(ref purpose) = group.purpose {
            let _ = writeln!(ctx, "[Group #{} purpose: {}]", group_id, purpose);
        }
        if let Some(ref notes_json) = group.notes_json {
            if let Ok(notes) = serde_json::from_str::<Vec<String>>(notes_json) {
                if !notes.is_empty() {
                    let _ = writeln!(
                        ctx,
                        "[Group #{} notes: {}]",
                        group_id,
                        notes.join("; ")
                    );
                }
            }
        }
    }

    // Sender context
    if let Ok(Some(npub)) = get_npub(conn, sender_hex) {
        let mut parts = Vec::new();

        if let Some(ref owner_notes_json) = npub.owner_notes_json {
            if let Ok(notes) = serde_json::from_str::<Vec<String>>(owner_notes_json) {
                if !notes.is_empty() {
                    parts.push(format!("owner says: {}", notes.join("; ")));
                }
            }
        }

        if let Some(ref notes_json) = npub.notes_json {
            if let Ok(notes) = serde_json::from_str::<Vec<String>>(notes_json) {
                if !notes.is_empty() {
                    // Show last 3 notes max to keep context concise
                    let recent: Vec<&str> = notes.iter().rev().take(3).map(|s| s.as_str()).collect();
                    let ordered: Vec<&str> = recent.into_iter().rev().collect();
                    parts.push(format!("notes: {}", ordered.join("; ")));
                }
            }
        }

        if !parts.is_empty() {
            let _ = writeln!(
                ctx,
                "[Known about {}: {}]",
                npub.display_name,
                parts.join(" | ")
            );
        }
    }

    ctx
}

/// Search social data using FTS5.
pub fn search_social(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<SocialNpub>> {
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
        "SELECT n.hex_pubkey, n.display_name, n.first_seen, n.first_seen_group,
                n.last_interaction, n.profile_json, n.name_history_json, n.notes_json, n.owner_notes_json,
                n.preferences_json, n.is_owner
         FROM social_fts f
         JOIN social_npubs n ON n.rowid = f.rowid
         WHERE social_fts MATCH ?1
         ORDER BY bm25(social_fts)
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(params![fts_query, limit_i64], |row| {
        let is_owner_int: i64 = row.get(10)?;
        Ok(SocialNpub {
            hex_pubkey: row.get(0)?,
            display_name: row.get(1)?,
            first_seen: row.get(2)?,
            first_seen_group: row.get(3)?,
            last_interaction: row.get(4)?,
            profile_json: row.get(5)?,
            name_history_json: row.get(6)?,
            notes_json: row.get(7)?,
            owner_notes_json: row.get(8)?,
            preferences_json: row.get(9)?,
            is_owner: is_owner_int != 0,
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

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .unwrap();
        create_social_tables(&conn).unwrap();
        conn
    }

    fn sample_npub(hex: &str, name: &str) -> SocialNpub {
        SocialNpub {
            hex_pubkey: hex.to_string(),
            display_name: name.to_string(),
            first_seen: 1000,
            first_seen_group: Some("test_group".to_string()),
            last_interaction: 2000,
            profile_json: None,
            name_history_json: None,
            notes_json: None,
            owner_notes_json: None,
            preferences_json: None,
            is_owner: false,
        }
    }

    fn sample_group(id: &str) -> SocialGroup {
        SocialGroup {
            group_id: id.to_string(),
            purpose: Some("testing".to_string()),
            members_json: Some(serde_json::to_string(&vec!["aabb", "ccdd"]).unwrap()),
            notes_json: None,
            last_activity: 3000,
        }
    }

    // ── Schema tests ─────────────────────────────────────────────

    #[test]
    fn create_tables_idempotent() {
        let conn = test_conn();
        // Second call should not error
        create_social_tables(&conn).unwrap();
    }

    #[test]
    fn tables_exist_after_creation() {
        let conn = test_conn();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='social_npubs'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='social_groups'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    // ── Npub CRUD tests ─────────────────────────────────────────

    #[test]
    fn upsert_and_get_npub() {
        let conn = test_conn();
        let npub = sample_npub("aabb", "Alice");
        upsert_npub(&conn, &npub).unwrap();

        let fetched = get_npub(&conn, "aabb").unwrap().unwrap();
        assert_eq!(fetched.hex_pubkey, "aabb");
        assert_eq!(fetched.display_name, "Alice");
        assert_eq!(fetched.first_seen, 1000);
        assert_eq!(fetched.last_interaction, 2000);
    }

    #[test]
    fn get_nonexistent_npub_returns_none() {
        let conn = test_conn();
        assert!(get_npub(&conn, "nonexistent").unwrap().is_none());
    }

    #[test]
    fn upsert_npub_updates_on_conflict() {
        let conn = test_conn();
        let npub = sample_npub("aabb", "Alice");
        upsert_npub(&conn, &npub).unwrap();

        let updated = SocialNpub {
            display_name: "Alice V2".to_string(),
            last_interaction: 5000,
            ..npub
        };
        upsert_npub(&conn, &updated).unwrap();

        let fetched = get_npub(&conn, "aabb").unwrap().unwrap();
        assert_eq!(fetched.display_name, "Alice V2");
        assert_eq!(fetched.last_interaction, 5000);
        // first_seen should be preserved (not updated on conflict)
        assert_eq!(fetched.first_seen, 1000);
    }

    #[test]
    fn upsert_npub_preserves_existing_notes_when_new_is_null() {
        let conn = test_conn();
        let npub = SocialNpub {
            notes_json: Some(serde_json::to_string(&vec!["existing note"]).unwrap()),
            ..sample_npub("aabb", "Alice")
        };
        upsert_npub(&conn, &npub).unwrap();

        // Upsert with None notes should preserve existing
        let updated = SocialNpub {
            notes_json: None,
            last_interaction: 5000,
            ..sample_npub("aabb", "Alice")
        };
        upsert_npub(&conn, &updated).unwrap();

        let fetched = get_npub(&conn, "aabb").unwrap().unwrap();
        assert!(fetched.notes_json.is_some());
        let notes: Vec<String> = serde_json::from_str(fetched.notes_json.as_ref().unwrap()).unwrap();
        assert_eq!(notes, vec!["existing note"]);
    }

    // ── Note operations ─────────────────────────────────────────

    #[test]
    fn add_agent_note() {
        let conn = test_conn();
        upsert_npub(&conn, &sample_npub("aabb", "Alice")).unwrap();

        add_npub_note(&conn, "aabb", "likes Rust", false).unwrap();
        add_npub_note(&conn, "aabb", "prefers CLI tools", false).unwrap();

        let fetched = get_npub(&conn, "aabb").unwrap().unwrap();
        let notes: Vec<String> =
            serde_json::from_str(fetched.notes_json.as_ref().unwrap()).unwrap();
        assert_eq!(notes, vec!["likes Rust", "prefers CLI tools"]);
    }

    #[test]
    fn add_owner_note() {
        let conn = test_conn();
        upsert_npub(&conn, &sample_npub("aabb", "Alice")).unwrap();

        add_npub_note(&conn, "aabb", "core team member", true).unwrap();

        let fetched = get_npub(&conn, "aabb").unwrap().unwrap();
        let notes: Vec<String> =
            serde_json::from_str(fetched.owner_notes_json.as_ref().unwrap()).unwrap();
        assert_eq!(notes, vec!["core team member"]);
        // Agent notes should remain empty/None
        assert!(fetched.notes_json.is_none());
    }

    #[test]
    fn add_note_to_nonexistent_npub_is_noop() {
        let conn = test_conn();
        // Should not error, just no-op
        add_npub_note(&conn, "nonexistent", "test note", false).unwrap();
    }

    // ── Group CRUD tests ────────────────────────────────────────

    #[test]
    fn upsert_and_get_group() {
        let conn = test_conn();
        let group = sample_group("dev_team");
        upsert_group(&conn, &group).unwrap();

        let fetched = get_group(&conn, "dev_team").unwrap().unwrap();
        assert_eq!(fetched.group_id, "dev_team");
        assert_eq!(fetched.purpose.as_deref(), Some("testing"));
        assert_eq!(fetched.last_activity, 3000);
    }

    #[test]
    fn get_nonexistent_group_returns_none() {
        let conn = test_conn();
        assert!(get_group(&conn, "nonexistent").unwrap().is_none());
    }

    #[test]
    fn upsert_group_updates_on_conflict() {
        let conn = test_conn();
        upsert_group(&conn, &sample_group("team")).unwrap();

        let updated = SocialGroup {
            purpose: Some("production".to_string()),
            last_activity: 9000,
            ..sample_group("team")
        };
        upsert_group(&conn, &updated).unwrap();

        let fetched = get_group(&conn, "team").unwrap().unwrap();
        assert_eq!(fetched.purpose.as_deref(), Some("production"));
        assert_eq!(fetched.last_activity, 9000);
    }

    // ── build_social_context tests ──────────────────────────────

    #[test]
    fn build_context_empty_db() {
        let conn = test_conn();
        let ctx = build_social_context(&conn, "aabb", "test");
        assert!(ctx.is_empty());
    }

    #[test]
    fn build_context_includes_group_purpose() {
        let conn = test_conn();
        upsert_group(&conn, &sample_group("dev")).unwrap();

        let ctx = build_social_context(&conn, "aabb", "dev");
        assert!(ctx.contains("testing"));
    }

    #[test]
    fn build_context_includes_group_notes() {
        let conn = test_conn();
        let group = SocialGroup {
            notes_json: Some(serde_json::to_string(&vec!["active channel"]).unwrap()),
            ..sample_group("dev")
        };
        upsert_group(&conn, &group).unwrap();

        let ctx = build_social_context(&conn, "aabb", "dev");
        assert!(ctx.contains("active channel"));
    }

    #[test]
    fn build_context_includes_sender_notes() {
        let conn = test_conn();
        upsert_npub(&conn, &sample_npub("aabb", "Alice")).unwrap();
        add_npub_note(&conn, "aabb", "likes Rust", false).unwrap();
        add_npub_note(&conn, "aabb", "core team", true).unwrap();

        let ctx = build_social_context(&conn, "aabb", "test");
        assert!(ctx.contains("likes Rust"));
        assert!(ctx.contains("core team"));
        assert!(ctx.contains("Alice"));
    }

    #[test]
    fn build_context_limits_notes_to_3() {
        let conn = test_conn();
        upsert_npub(&conn, &sample_npub("aabb", "Alice")).unwrap();
        for i in 0..10 {
            add_npub_note(&conn, "aabb", &format!("note_{i}"), false).unwrap();
        }

        let ctx = build_social_context(&conn, "aabb", "test");
        // Should contain the last 3 notes
        assert!(ctx.contains("note_7"));
        assert!(ctx.contains("note_8"));
        assert!(ctx.contains("note_9"));
        // Should not contain the first note
        assert!(!ctx.contains("note_0"));
    }

    // ── FTS5 search tests ───────────────────────────────────────

    #[test]
    fn search_by_display_name() {
        let conn = test_conn();
        upsert_npub(&conn, &sample_npub("aabb", "Alice")).unwrap();
        upsert_npub(&conn, &sample_npub("ccdd", "Bob")).unwrap();

        let results = search_social(&conn, "Alice", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].hex_pubkey, "aabb");
    }

    #[test]
    fn search_by_notes_content() {
        let conn = test_conn();
        upsert_npub(&conn, &sample_npub("aabb", "Alice")).unwrap();
        add_npub_note(&conn, "aabb", "expert in cryptography", false).unwrap();

        let results = search_social(&conn, "cryptography", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].hex_pubkey, "aabb");
    }

    #[test]
    fn search_by_owner_notes() {
        let conn = test_conn();
        upsert_npub(&conn, &sample_npub("aabb", "Alice")).unwrap();
        add_npub_note(&conn, "aabb", "VIP contact", true).unwrap();

        let results = search_social(&conn, "VIP", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_empty_query_returns_empty() {
        let conn = test_conn();
        upsert_npub(&conn, &sample_npub("aabb", "Alice")).unwrap();
        assert!(search_social(&conn, "", 10).unwrap().is_empty());
        assert!(search_social(&conn, "   ", 10).unwrap().is_empty());
    }

    #[test]
    fn search_no_match_returns_empty() {
        let conn = test_conn();
        upsert_npub(&conn, &sample_npub("aabb", "Alice")).unwrap();
        let results = search_social(&conn, "zzzznonexistent", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_respects_limit() {
        let conn = test_conn();
        for i in 0..20 {
            upsert_npub(&conn, &sample_npub(&format!("hex_{i:02}"), &format!("User_{i}"))).unwrap();
        }
        let results = search_social(&conn, "User", 5).unwrap();
        assert!(results.len() <= 5);
    }

    #[test]
    fn fts_syncs_on_update() {
        let conn = test_conn();
        upsert_npub(&conn, &sample_npub("aabb", "OldName")).unwrap();

        // Should find by old name
        assert_eq!(search_social(&conn, "OldName", 10).unwrap().len(), 1);

        // Update name
        let updated = SocialNpub {
            display_name: "NewName".to_string(),
            last_interaction: 5000,
            ..sample_npub("aabb", "NewName")
        };
        upsert_npub(&conn, &updated).unwrap();

        // Old name should not match
        assert!(search_social(&conn, "OldName", 10).unwrap().is_empty());
        // New name should match
        assert_eq!(search_social(&conn, "NewName", 10).unwrap().len(), 1);
    }

    #[test]
    fn search_with_double_quotes_does_not_error() {
        let conn = test_conn();
        upsert_npub(&conn, &sample_npub("aabb", "Alice")).unwrap();

        // Should not crash on embedded double quotes
        let results = search_social(&conn, r#"say "hello" world"#, 10).unwrap();
        assert!(results.len() <= 10);
    }

    // ── Unicode / edge cases ────────────────────────────────────

    #[test]
    fn unicode_display_name() {
        let conn = test_conn();
        upsert_npub(&conn, &sample_npub("uni", "日本太郎")).unwrap();
        let fetched = get_npub(&conn, "uni").unwrap().unwrap();
        assert_eq!(fetched.display_name, "日本太郎");
    }

    #[test]
    fn profile_json_roundtrip() {
        let conn = test_conn();
        let profile = r#"{"name":"Alice","about":"Rust dev","nip05":"alice@example.com"}"#;
        let npub = SocialNpub {
            profile_json: Some(profile.to_string()),
            ..sample_npub("aabb", "Alice")
        };

        upsert_npub(&conn, &npub).unwrap();

        let fetched = get_npub(&conn, "aabb").unwrap().unwrap();
        assert_eq!(fetched.profile_json.as_deref(), Some(profile));
    }
}
