//! Per-npub and per-group memory for the Nostr channel.
//!
//! Phase 4: SQLite (`memory/social.rs`) is the sole source of truth.
//! The in-memory HashMap cache and JSON file persistence have been removed.
//! When SQLite is unavailable, methods return empty/default (graceful degradation).
//!
//! On first construction with SQLite (`with_sqlite`), if the legacy
//! `nostr_memory.json` exists and SQLite is empty, data is migrated once.
//!
//! Phase 5: NIP-78 relay persistence. Social data is also published to the
//! Nostr relay as kind 30078 events, making the relay the source of truth.
//! On startup, existing social events are synced from relay into SQLite.

use nostr_sdk::prelude::*;
use parking_lot::Mutex as ParkingMutex;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::memory::doc_index::{self, DocHit};
use crate::memory::message_index::{self, IndexDecision, IndexableMessage, MessageHit};
use crate::memory::social::{self, SocialGroup, SocialNpub};
use crate::memory::unified_search::{self, UnifiedHit};

// ── Data structures ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NpubMemory {
    pub npub_hex: String,
    pub display_name: String,
    pub first_seen: u64,
    pub first_seen_group: Option<String>,
    /// Agent's observations and learned context about this person.
    pub notes: Vec<String>,
    /// Owner-provided notes (e.g. "prefers Finnish", "core team member").
    pub owner_notes: Vec<String>,
    pub last_interaction: u64,
    /// Name history (display names change on Nostr).
    #[serde(default)]
    pub name_history: Vec<(u64, String)>,
    /// Full kind 0 profile metadata (about, picture, nip05, etc.).
    #[serde(default)]
    pub profile_metadata: Option<ProfileMetadata>,
}

/// Stored profile metadata from kind 0 events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileMetadata {
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub about: Option<String>,
    pub picture: Option<String>,
    pub nip05: Option<String>,
    pub lud16: Option<String>,
    pub fetched_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMemory {
    pub group_id: String,
    pub purpose: Option<String>,
    /// Hex pubkeys of members we've seen in this group.
    pub members_seen: Vec<String>,
    /// Agent's observations about the group.
    pub notes: Vec<String>,
    pub last_activity: u64,
}

/// Legacy in-memory store — kept only for JSON migration deserialization.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct LegacyMemoryStore {
    pub npubs: HashMap<String, NpubMemory>,
    pub groups: HashMap<String, GroupMemory>,
}

// ── NostrMemory (shared handle) ──────────────────────────────────

/// Thread-safe handle to the Nostr social memory store.
///
/// SQLite is the sole backend. When SQLite is unavailable (`None`),
/// all reads return empty/default and writes are silently dropped.
///
/// When a relay client is set, social data is also published to the
/// Nostr relay as NIP-78 kind 30078 events (best-effort, non-blocking).
#[derive(Clone)]
pub struct NostrMemory {
    /// SQLite connection for social memory.
    sqlite: Option<Arc<ParkingMutex<Connection>>>,
    /// Nostr relay client for NIP-78 social data persistence.
    relay_client: Option<Client>,
    /// Our public key (for relay queries).
    relay_pubkey: Option<PublicKey>,
}

impl NostrMemory {
    /// Create a NostrMemory without SQLite (degraded mode).
    ///
    /// All reads return empty, all writes are no-ops. Used as fallback
    /// when SQLite is unavailable.
    pub fn new(_persist_dir: &Path) -> Self {
        warn!("NostrMemory created without SQLite — operating in degraded mode");
        Self {
            sqlite: None,
            relay_client: None,
            relay_pubkey: None,
        }
    }

    /// Create a NostrMemory backed by SQLite.
    ///
    /// Social tables must already be created (`social::create_social_tables`).
    /// If SQLite is empty and a legacy `nostr_memory.json` exists, migrates
    /// data from JSON into SQLite (one-time migration).
    pub fn with_sqlite(persist_dir: &Path, conn: Arc<ParkingMutex<Connection>>) -> Self {
        let persist_path = persist_dir.join("nostr_memory.json");

        // Check if SQLite already has data
        let has_data = {
            let db = conn.lock();
            match social::list_npubs(&db) {
                Ok(npubs) if !npubs.is_empty() => true,
                _ => {
                    // Also check groups
                    let groups_count: i64 = db
                        .query_row("SELECT COUNT(*) FROM social_groups", [], |r| r.get(0))
                        .unwrap_or(0);
                    groups_count > 0
                }
            }
        };

        // If SQLite is empty, try migrating from legacy JSON
        if !has_data {
            Self::migrate_from_json(&persist_path, &conn);
        }

        // Log what we have
        {
            let db = conn.lock();
            let npub_count = social::list_npubs(&db).map(|v| v.len()).unwrap_or(0);
            let group_count: i64 = db
                .query_row("SELECT COUNT(*) FROM social_groups", [], |r| r.get(0))
                .unwrap_or(0);
            if npub_count > 0 || group_count > 0 {
                info!("Loaded nostr memory (SQLite): {npub_count} contacts, {group_count} groups");
            }
        }

        Self {
            sqlite: Some(conn),
            relay_client: None,
            relay_pubkey: None,
        }
    }

    /// Attach a Nostr relay client for NIP-78 social data persistence.
    ///
    /// When set, social data writes (ensure_npub, add_npub_note, etc.)
    /// will also publish kind 30078 events to the relay (best-effort).
    /// Call `sync_social_from_relay()` after setting to pull existing data.
    pub fn set_relay_client(&mut self, client: Client, pubkey: PublicKey) {
        self.relay_client = Some(client);
        self.relay_pubkey = Some(pubkey);
        info!("NostrMemory relay persistence enabled (pubkey: {})",
            pubkey.to_bech32().unwrap_or_default());
    }

    /// One-time migration: load legacy `nostr_memory.json` and insert into SQLite.
    fn migrate_from_json(persist_path: &Path, conn: &Arc<ParkingMutex<Connection>>) {
        if !persist_path.exists() {
            return;
        }

        let json_store: LegacyMemoryStore = match std::fs::read_to_string(persist_path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
                warn!("Failed to parse nostr_memory.json for migration: {e}");
                LegacyMemoryStore::default()
            }),
            Err(e) => {
                warn!("Failed to read nostr_memory.json for migration: {e}");
                return;
            }
        };

        if json_store.npubs.is_empty() && json_store.groups.is_empty() {
            return;
        }

        info!(
            "Migrating {} contacts and {} groups from JSON to SQLite",
            json_store.npubs.len(),
            json_store.groups.len()
        );

        let db = conn.lock();
        for npub in json_store.npubs.values() {
            if let Err(e) = social::upsert_npub(&db, &cache_to_social_npub(npub)) {
                warn!("Failed to migrate npub {} to SQLite: {e}", npub.npub_hex);
            }
        }
        for group in json_store.groups.values() {
            if let Err(e) = social::upsert_group(&db, &cache_to_social_group(group)) {
                warn!("Failed to migrate group {} to SQLite: {e}", group.group_id);
            }
        }
    }

    /// Ensure an npub memory entry exists. Returns true if this is a new contact.
    pub async fn ensure_npub(
        &self,
        hex_pubkey: &str,
        display_name: &str,
        timestamp: u64,
        group: Option<&str>,
        is_owner: bool,
    ) -> bool {
        let Some(ref conn) = self.sqlite else {
            return false;
        };

        #[allow(clippy::cast_possible_wrap)]
        let ts = timestamp as i64;

        let is_new = {
            let db = conn.lock();
            match social::touch_npub(&db, hex_pubkey, display_name, ts) {
                Ok(true) => {
                    // Doesn't exist yet — insert
                    let npub = SocialNpub {
                        hex_pubkey: hex_pubkey.to_string(),
                        display_name: display_name.to_string(),
                        first_seen: ts,
                        first_seen_group: group.map(|s| s.to_string()),
                        last_interaction: ts,
                        profile_json: None,
                        name_history_json: None,
                        notes_json: None,
                        owner_notes_json: None,
                        preferences_json: None,
                        is_owner,
                    };
                    if let Err(e) = social::upsert_npub(&db, &npub) {
                        warn!("SQLite upsert_npub failed: {e}");
                    }
                    true
                }
                Ok(false) => false,
                Err(e) => {
                    warn!("SQLite touch_npub failed: {e}");
                    false
                }
            }
        };

        // Best-effort relay publish (don't block on result)
        self.publish_npub_to_relay(hex_pubkey).await;

        is_new
    }

    /// Ensure a group memory entry exists. Returns true if this is a new group.
    pub async fn ensure_group(&self, group_id: &str, timestamp: u64) -> bool {
        let Some(ref conn) = self.sqlite else {
            return false;
        };

        #[allow(clippy::cast_possible_wrap)]
        let ts = timestamp as i64;

        let is_new = {
            let db = conn.lock();
            let exists = social::get_group(&db, group_id)
                .ok()
                .flatten()
                .is_some();

            if exists {
                let _ = db.execute(
                    "UPDATE social_groups SET last_activity = ?1 WHERE group_id = ?2",
                    rusqlite::params![ts, group_id],
                );
                false
            } else {
                let group = SocialGroup {
                    group_id: group_id.to_string(),
                    purpose: None,
                    members_json: None,
                    notes_json: None,
                    last_activity: ts,
                };
                if let Err(e) = social::upsert_group(&db, &group) {
                    warn!("SQLite upsert_group failed: {e}");
                }
                true
            }
        };

        // Best-effort relay publish
        self.publish_group_to_relay(group_id).await;

        is_new
    }

    /// Record that an npub was seen in a group.
    pub async fn record_group_member(&self, group_id: &str, hex_pubkey: &str) {
        let Some(ref conn) = self.sqlite else {
            return;
        };

        {
            let db = conn.lock();
            if let Err(e) = social::record_group_member(&db, group_id, hex_pubkey) {
                warn!("SQLite record_group_member failed: {e}");
                return;
            }
        }

        self.publish_group_to_relay(group_id).await;
    }

    /// Add a note to an npub's memory.
    pub async fn add_npub_note(&self, hex_pubkey: &str, note: &str) {
        let Some(ref conn) = self.sqlite else {
            return;
        };

        {
            let db = conn.lock();
            if let Err(e) = social::add_npub_note(&db, hex_pubkey, note, false) {
                warn!("SQLite add_npub_note failed: {e}");
                return;
            }
        }

        self.publish_npub_to_relay(hex_pubkey).await;
    }

    /// Add an owner note to an npub's memory.
    pub async fn add_npub_owner_note(&self, hex_pubkey: &str, note: &str) {
        let Some(ref conn) = self.sqlite else {
            return;
        };

        {
            let db = conn.lock();
            if let Err(e) = social::add_npub_note(&db, hex_pubkey, note, true) {
                warn!("SQLite add_npub_owner_note failed: {e}");
                return;
            }
        }

        self.publish_npub_to_relay(hex_pubkey).await;
    }

    /// Add a note to a group's memory.
    pub async fn add_group_note(&self, group_id: &str, note: &str) {
        let Some(ref conn) = self.sqlite else {
            return;
        };

        {
            let db = conn.lock();
            if let Err(e) = social::add_group_note(&db, group_id, note) {
                warn!("SQLite add_group_note failed: {e}");
                return;
            }
        }

        self.publish_group_to_relay(group_id).await;
    }

    /// Set a group's purpose.
    pub async fn set_group_purpose(&self, group_id: &str, purpose: &str) {
        let Some(ref conn) = self.sqlite else {
            return;
        };

        {
            let db = conn.lock();
            if let Err(e) = social::set_group_purpose(&db, group_id, purpose) {
                warn!("SQLite set_group_purpose failed: {e}");
                return;
            }
        }

        self.publish_group_to_relay(group_id).await;
    }

    /// Update profile metadata for an npub. Tracks name changes in name_history.
    pub async fn update_profile(
        &self,
        hex_pubkey: &str,
        metadata: ProfileMetadata,
    ) {
        let Some(ref conn) = self.sqlite else {
            return;
        };

        let updated = {
            let profile_json = serde_json::to_string(&metadata).ok();
            let db = conn.lock();
            if let Ok(Some(mut existing)) = social::get_npub(&db, hex_pubkey) {
                // Track name change in name_history
                let new_name = metadata.display_name.as_deref()
                    .or(metadata.name.as_deref());
                if let Some(new_name) = new_name {
                    if new_name != existing.display_name {
                        let mut history: Vec<(i64, String)> = existing
                            .name_history_json
                            .as_deref()
                            .and_then(|json| serde_json::from_str(json).ok())
                            .unwrap_or_default();
                        #[allow(clippy::cast_possible_wrap)]
                        history.push((metadata.fetched_at as i64, existing.display_name.clone()));
                        existing.name_history_json = serde_json::to_string(&history).ok();
                        existing.display_name = new_name.to_string();
                    }
                }
                existing.profile_json = profile_json;
                if let Err(e) = social::upsert_npub(&db, &existing) {
                    warn!("SQLite update_profile failed: {e}");
                    false
                } else {
                    true
                }
            } else {
                false
            }
        };

        if updated {
            self.publish_npub_to_relay(hex_pubkey).await;
        }
    }

    /// Get npub memory (read from SQLite).
    pub async fn get_npub(&self, hex_pubkey: &str) -> Option<NpubMemory> {
        let conn = self.sqlite.as_ref()?;
        let db = conn.lock();
        social::get_npub(&db, hex_pubkey)
            .ok()
            .flatten()
            .map(|s| social_npub_to_npub_memory(&s))
    }

    /// Get group memory (read from SQLite).
    pub async fn get_group(&self, group_id: &str) -> Option<GroupMemory> {
        let conn = self.sqlite.as_ref()?;
        let db = conn.lock();
        social::get_group(&db, group_id)
            .ok()
            .flatten()
            .map(|s| social_group_to_group_memory(&s))
    }

    /// List all known npubs.
    pub async fn list_npubs(&self) -> Vec<NpubMemory> {
        let Some(ref conn) = self.sqlite else {
            return Vec::new();
        };

        let db = conn.lock();
        social::list_npubs(&db)
            .unwrap_or_default()
            .iter()
            .map(social_npub_to_npub_memory)
            .collect()
    }

    /// Build concise LLM context for a sender in a group.
    pub async fn build_context(&self, sender_hex: &str, group_id: &str) -> String {
        let Some(ref conn) = self.sqlite else {
            return String::new();
        };

        let db = conn.lock();
        social::build_social_context(&db, sender_hex, group_id)
    }

    /// Record another agent's state update (kind 31121).
    pub async fn record_agent_state(
        &self,
        agent_hex: &str,
        agent_name: &str,
        d_tag: &str,
        status: &str,
        _content: &str,
        timestamp: u64,
    ) {
        let Some(ref conn) = self.sqlite else {
            return;
        };

        let note = format!("[{}] state: {} d={}", agent_name, status, d_tag);

        #[allow(clippy::cast_possible_wrap)]
        let ts = timestamp as i64;

        {
            let db = conn.lock();

            // Ensure npub exists in SQLite
            if social::get_npub(&db, agent_hex).ok().flatten().is_none() {
                let npub = SocialNpub {
                    hex_pubkey: agent_hex.to_string(),
                    display_name: agent_name.to_string(),
                    first_seen: ts,
                    first_seen_group: None,
                    last_interaction: ts,
                    profile_json: None,
                    name_history_json: None,
                    notes_json: None,
                    owner_notes_json: None,
                    preferences_json: None,
                    is_owner: false,
                };
                if let Err(e) = social::upsert_npub(&db, &npub) {
                    warn!("SQLite upsert agent npub failed: {e}");
                }
            }

            // Read current notes, filter out old state notes, add new
            if let Ok(Some(existing)) = social::get_npub(&db, agent_hex) {
                let mut notes: Vec<String> = existing
                    .notes_json
                    .as_deref()
                    .and_then(|json| serde_json::from_str(json).ok())
                    .unwrap_or_default();
                let state_prefix = format!("[{}] state:", agent_name);
                notes.retain(|n| !n.starts_with(&state_prefix));
                notes.push(note);
                let updated = serde_json::to_string(&notes).unwrap_or_default();

                let _ = db.execute(
                    "UPDATE social_npubs SET notes_json = ?1, last_interaction = ?2 WHERE hex_pubkey = ?3",
                    rusqlite::params![updated, ts, agent_hex],
                );
            }
        }

        self.publish_npub_to_relay(agent_hex).await;
    }

    // ── NIP-78 relay persistence ──────────────────────────────────

    /// Publish an npub's social data to the relay as a NIP-78 kind 30078 event.
    ///
    /// D-tag format: `snowclaw:memory:npub:<npub1bech32...>`
    /// Best-effort — logs warning on failure, never errors.
    async fn publish_npub_to_relay(&self, hex_pubkey: &str) {
        let Some(ref client) = self.relay_client else {
            return;
        };
        let Some(ref conn) = self.sqlite else {
            return;
        };

        // Read current state from SQLite
        let npub = {
            let db = conn.lock();
            match social::get_npub(&db, hex_pubkey) {
                Ok(Some(n)) => n,
                _ => return,
            }
        };

        // Convert hex pubkey to bech32 npub for d-tag
        let bech32_npub = match PublicKey::from_hex(hex_pubkey) {
            Ok(pk) => match pk.to_bech32() {
                Ok(b) => b,
                Err(e) => {
                    warn!("Failed to convert pubkey to bech32: {e}");
                    return;
                }
            },
            Err(e) => {
                warn!("Invalid hex pubkey for relay publish: {e}");
                return;
            }
        };

        let d_tag = format!("snowclaw:memory:npub:{bech32_npub}");
        let content = match serde_json::to_string(&npub) {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to serialize npub for relay: {e}");
                return;
            }
        };

        let mut tags = vec![
            Tag::custom(TagKind::custom("d"), vec![d_tag.clone()]),
            Tag::custom(TagKind::custom("app"), vec!["snowclaw".to_string()]),
            Tag::custom(TagKind::custom("agent"), vec!["snowclaw".to_string()]),
        ];

        // Add group scoping tag if available
        if let Some(ref group) = npub.first_seen_group {
            tags.push(Tag::custom(TagKind::custom("h"), vec![group.clone()]));
        }

        let builder = EventBuilder::new(Kind::Custom(30078), &content).tags(tags);

        match client.send_event_builder(builder).await {
            Ok(_) => debug!("Published social npub to relay: {d_tag}"),
            Err(e) => warn!("Failed to publish social npub to relay: {e}"),
        }
    }

    /// Publish a group's social data to the relay as a NIP-78 kind 30078 event.
    ///
    /// D-tag format: `snowclaw:memory:group:<group_id>`
    /// Best-effort — logs warning on failure, never errors.
    async fn publish_group_to_relay(&self, group_id: &str) {
        let Some(ref client) = self.relay_client else {
            return;
        };
        let Some(ref conn) = self.sqlite else {
            return;
        };

        // Read current state from SQLite
        let group = {
            let db = conn.lock();
            match social::get_group(&db, group_id) {
                Ok(Some(g)) => g,
                _ => return,
            }
        };

        let d_tag = format!("snowclaw:memory:group:{group_id}");
        let content = match serde_json::to_string(&group) {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to serialize group for relay: {e}");
                return;
            }
        };

        let tags = vec![
            Tag::custom(TagKind::custom("d"), vec![d_tag.clone()]),
            Tag::custom(TagKind::custom("h"), vec![group_id.to_string()]),
            Tag::custom(TagKind::custom("app"), vec!["snowclaw".to_string()]),
            Tag::custom(TagKind::custom("agent"), vec!["snowclaw".to_string()]),
        ];

        let builder = EventBuilder::new(Kind::Custom(30078), &content).tags(tags);

        match client.send_event_builder(builder).await {
            Ok(_) => debug!("Published social group to relay: {d_tag}"),
            Err(e) => warn!("Failed to publish social group to relay: {e}"),
        }
    }

    /// Sync social data from relay into local SQLite on startup.
    ///
    /// Fetches all kind 30078 events with `snowclaw:memory:npub:` and
    /// `snowclaw:memory:group:` d-tag prefixes authored by our pubkey,
    /// then upserts into SQLite.
    pub async fn sync_social_from_relay(&self) -> usize {
        let Some(ref client) = self.relay_client else {
            return 0;
        };
        let Some(ref pubkey) = self.relay_pubkey else {
            return 0;
        };
        let Some(ref conn) = self.sqlite else {
            return 0;
        };

        let filter = Filter::new()
            .author(*pubkey)
            .kind(Kind::Custom(30078))
            .custom_tag(
                SingleLetterTag::lowercase(Alphabet::D),
                "snowclaw:memory:",
            );

        let events = match client
            .fetch_events(filter, Duration::from_secs(15))
            .await
        {
            Ok(evts) => evts,
            Err(e) => {
                warn!("Failed to fetch social events from relay: {e}");
                return 0;
            }
        };

        let events: Vec<Event> = events.into_iter().collect();
        let total = events.len();
        let mut synced = 0;

        let db = conn.lock();

        for event in &events {
            let d_tag = event
                .tags
                .iter()
                .find(|t| t.as_slice().first().map(|s| s.as_str()) == Some("d"))
                .and_then(|t| t.as_slice().get(1).map(|s| s.to_string()));

            let d_tag = match d_tag {
                Some(d) => d,
                None => continue,
            };

            if let Some(rest) = d_tag.strip_prefix("snowclaw:memory:npub:") {
                // Parse npub bech32 → hex for SQLite key
                let hex = match PublicKey::from_bech32(rest) {
                    Ok(pk) => pk.to_hex(),
                    Err(e) => {
                        warn!("Invalid npub in relay d-tag {rest}: {e}");
                        continue;
                    }
                };

                match serde_json::from_str::<SocialNpub>(&event.content) {
                    Ok(mut npub) => {
                        // Ensure hex_pubkey matches the d-tag
                        npub.hex_pubkey = hex;
                        if let Err(e) = social::upsert_npub(&db, &npub) {
                            warn!("Failed to sync npub from relay: {e}");
                            continue;
                        }
                        synced += 1;
                    }
                    Err(e) => {
                        warn!("Failed to parse npub event content: {e}");
                    }
                }
            } else if let Some(group_id) = d_tag.strip_prefix("snowclaw:memory:group:") {
                match serde_json::from_str::<SocialGroup>(&event.content) {
                    Ok(mut group) => {
                        group.group_id = group_id.to_string();
                        if let Err(e) = social::upsert_group(&db, &group) {
                            warn!("Failed to sync group from relay: {e}");
                            continue;
                        }
                        synced += 1;
                    }
                    Err(e) => {
                        warn!("Failed to parse group event content: {e}");
                    }
                }
            }
        }

        if synced > 0 {
            info!("Social relay→SQLite sync complete: {synced}/{total} events");
        } else {
            debug!("Social relay→SQLite sync: no new events ({total} total on relay)");
        }

        synced
    }

    /// No-op — JSON persistence has been removed (SQLite commits are immediate).
    /// Kept for caller compatibility.
    pub async fn flush(&self) -> Result<(), std::io::Error> {
        Ok(())
    }

    /// No-op — JSON persistence has been removed.
    /// Kept for caller compatibility.
    pub async fn force_flush(&self) {
        // no-op
    }

    // ── Message indexing (Phase 2) ──────────────────────────────────

    /// Evaluate and index a message if it meets the indexing criteria.
    ///
    /// This is a synchronous, non-async helper because it only touches SQLite.
    /// Safe to call from the listen loop.
    pub fn try_index_message(
        &self,
        event_id: &str,
        sender_hex: &str,
        group_id: Option<&str>,
        content: &str,
        timestamp: u64,
        kind: u32,
        is_bot_mention: bool,
        is_dm: bool,
    ) {
        let decision = message_index::should_index_message(content, kind, is_bot_mention, is_dm);
        if decision == IndexDecision::Skip {
            return;
        }

        let Some(ref conn) = self.sqlite else {
            return;
        };

        let msg = IndexableMessage {
            event_id: event_id.to_string(),
            sender_hex: sender_hex.to_string(),
            group_id: group_id.map(|s| s.to_string()),
            content: content.to_string(),
            #[allow(clippy::cast_possible_wrap)]
            created_at: timestamp as i64,
            kind,
        };

        let db = conn.lock();
        if let Err(e) = message_index::index_message(&db, &msg) {
            warn!("Failed to index message {}: {e}", event_id);
        }
    }

    /// Search indexed messages using FTS5.
    pub fn search_messages(&self, query: &str, limit: usize) -> Vec<MessageHit> {
        let Some(ref conn) = self.sqlite else {
            return Vec::new();
        };

        let db = conn.lock();
        match message_index::search_messages(&db, query, limit) {
            Ok(results) => results,
            Err(e) => {
                warn!("Message search failed: {e}");
                Vec::new()
            }
        }
    }

    // ── Document search (Phase 3) ────────────────────────────────

    /// Search indexed documents using FTS5.
    pub fn search_docs(&self, query: &str, limit: usize) -> Vec<DocHit> {
        let Some(ref conn) = self.sqlite else {
            return Vec::new();
        };

        let db = conn.lock();
        match doc_index::search_docs(&db, query, limit) {
            Ok(results) => results,
            Err(e) => {
                warn!("Document search failed: {e}");
                Vec::new()
            }
        }
    }

    /// Unified search across social, messages, and documents.
    pub fn unified_search(&self, query: &str, limit: usize) -> Vec<UnifiedHit> {
        let Some(ref conn) = self.sqlite else {
            return Vec::new();
        };

        let db = conn.lock();
        match unified_search::unified_recall(&db, query, limit) {
            Ok(results) => results,
            Err(e) => {
                warn!("Unified search failed: {e}");
                Vec::new()
            }
        }
    }
}

// ── Conversion helpers ──────────────────────────────────────────

/// Convert a `SocialNpub` (SQLite row) to an `NpubMemory` (public API type).
fn social_npub_to_npub_memory(s: &SocialNpub) -> NpubMemory {
    #[allow(clippy::cast_sign_loss)]
    let first_seen = s.first_seen as u64;
    #[allow(clippy::cast_sign_loss)]
    let last_interaction = s.last_interaction as u64;

    let notes: Vec<String> = s
        .notes_json
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();

    let owner_notes: Vec<String> = s
        .owner_notes_json
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();

    let name_history: Vec<(u64, String)> = s
        .name_history_json
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();

    let profile_metadata: Option<ProfileMetadata> = s
        .profile_json
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok());

    NpubMemory {
        npub_hex: s.hex_pubkey.clone(),
        display_name: s.display_name.clone(),
        first_seen,
        first_seen_group: s.first_seen_group.clone(),
        notes,
        owner_notes,
        last_interaction,
        name_history,
        profile_metadata,
    }
}

/// Convert a `SocialGroup` (SQLite row) to a `GroupMemory` (public API type).
fn social_group_to_group_memory(s: &SocialGroup) -> GroupMemory {
    #[allow(clippy::cast_sign_loss)]
    let last_activity = s.last_activity as u64;

    let members_seen: Vec<String> = s
        .members_json
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();

    let notes: Vec<String> = s
        .notes_json
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();

    GroupMemory {
        group_id: s.group_id.clone(),
        purpose: s.purpose.clone(),
        members_seen,
        notes,
        last_activity,
    }
}

/// Convert an `NpubMemory` to a `SocialNpub` for SQLite upsert (used in JSON migration).
fn cache_to_social_npub(n: &NpubMemory) -> SocialNpub {
    #[allow(clippy::cast_possible_wrap)]
    let first_seen = n.first_seen as i64;
    #[allow(clippy::cast_possible_wrap)]
    let last_interaction = n.last_interaction as i64;

    let notes_json = if n.notes.is_empty() {
        None
    } else {
        serde_json::to_string(&n.notes).ok()
    };

    let owner_notes_json = if n.owner_notes.is_empty() {
        None
    } else {
        serde_json::to_string(&n.owner_notes).ok()
    };

    let name_history_json = if n.name_history.is_empty() {
        None
    } else {
        serde_json::to_string(&n.name_history).ok()
    };

    let profile_json = n
        .profile_metadata
        .as_ref()
        .and_then(|m| serde_json::to_string(m).ok());

    SocialNpub {
        hex_pubkey: n.npub_hex.clone(),
        display_name: n.display_name.clone(),
        first_seen,
        first_seen_group: n.first_seen_group.clone(),
        last_interaction,
        profile_json,
        name_history_json,
        notes_json,
        owner_notes_json,
        preferences_json: None,
        is_owner: false,
    }
}

/// Convert a `GroupMemory` to a `SocialGroup` for SQLite upsert (used in JSON migration).
fn cache_to_social_group(g: &GroupMemory) -> SocialGroup {
    #[allow(clippy::cast_possible_wrap)]
    let last_activity = g.last_activity as i64;

    let members_json = if g.members_seen.is_empty() {
        None
    } else {
        serde_json::to_string(&g.members_seen).ok()
    };

    let notes_json = if g.notes.is_empty() {
        None
    } else {
        serde_json::to_string(&g.notes).ok()
    };

    SocialGroup {
        group_id: g.group_id.clone(),
        purpose: g.purpose.clone(),
        members_json,
        notes_json,
        last_activity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_sqlite_conn() -> Arc<ParkingMutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .unwrap();
        social::create_social_tables(&conn).unwrap();
        message_index::create_message_tables(&conn).unwrap();
        doc_index::create_doc_tables(&conn).unwrap();
        Arc::new(ParkingMutex::new(conn))
    }

    // ── Degraded mode (no SQLite) tests ─────────────────────────

    #[tokio::test]
    async fn no_sqlite_returns_defaults() {
        let dir = TempDir::new().unwrap();
        let mem = NostrMemory::new(dir.path());

        // ensure_npub returns false (no-op)
        let is_new = mem.ensure_npub("aabb", "Alice", 1000, Some("techteam"), false).await;
        assert!(!is_new);

        // get returns None
        assert!(mem.get_npub("aabb").await.is_none());
        assert!(mem.get_group("any").await.is_none());
        assert!(mem.list_npubs().await.is_empty());
        assert!(mem.build_context("aa", "test").await.is_empty());

        // flush/force_flush are no-ops
        mem.flush().await.unwrap();
        mem.force_flush().await;
    }

    // ── SQLite-backed tests ─────────────────────────────────────

    #[tokio::test]
    async fn sqlite_new_contact_auto_created() {
        let dir = TempDir::new().unwrap();
        let conn = test_sqlite_conn();
        let mem = NostrMemory::with_sqlite(dir.path(), conn.clone());

        let is_new = mem.ensure_npub("aabb", "Alice", 1000, Some("techteam"), false).await;
        assert!(is_new);

        // Verify via get_npub
        let npub = mem.get_npub("aabb").await.unwrap();
        assert_eq!(npub.display_name, "Alice");
        assert_eq!(npub.first_seen, 1000);

        // Verify in SQLite directly
        {
            let db = conn.lock();
            let npub = social::get_npub(&db, "aabb").unwrap().unwrap();
            assert_eq!(npub.display_name, "Alice");
            assert_eq!(npub.first_seen, 1000);
        }

        let is_new2 = mem.ensure_npub("aabb", "Alice", 1001, Some("techteam"), false).await;
        assert!(!is_new2);

        // Verify last_interaction updated in SQLite
        {
            let db = conn.lock();
            let npub = social::get_npub(&db, "aabb").unwrap().unwrap();
            assert_eq!(npub.last_interaction, 1001);
        }
    }

    #[tokio::test]
    async fn sqlite_name_change_tracked() {
        let dir = TempDir::new().unwrap();
        let conn = test_sqlite_conn();
        let mem = NostrMemory::with_sqlite(dir.path(), conn.clone());

        mem.ensure_npub("cc", "Bob", 100, None, false).await;
        mem.ensure_npub("cc", "Bobby", 200, None, false).await;

        // Verify via get_npub
        let npub = mem.get_npub("cc").await.unwrap();
        assert_eq!(npub.display_name, "Bobby");
        assert_eq!(npub.name_history.len(), 1);
        assert_eq!(npub.name_history[0].1, "Bob");

        // Verify in SQLite
        {
            let db = conn.lock();
            let npub = social::get_npub(&db, "cc").unwrap().unwrap();
            assert_eq!(npub.display_name, "Bobby");
            let history: Vec<(i64, String)> =
                serde_json::from_str(npub.name_history_json.as_deref().unwrap()).unwrap();
            assert_eq!(history.len(), 1);
            assert_eq!(history[0].1, "Bob");
        }
    }

    #[tokio::test]
    async fn sqlite_group_memory_and_members() {
        let dir = TempDir::new().unwrap();
        let conn = test_sqlite_conn();
        let mem = NostrMemory::with_sqlite(dir.path(), conn.clone());

        let is_new = mem.ensure_group("techteam", 1000).await;
        assert!(is_new);

        mem.record_group_member("techteam", "aabb").await;
        mem.record_group_member("techteam", "ccdd").await;
        mem.record_group_member("techteam", "aabb").await; // duplicate

        // Verify via get_group
        let group = mem.get_group("techteam").await.unwrap();
        assert_eq!(group.members_seen.len(), 2);

        // Verify in SQLite
        {
            let db = conn.lock();
            let group = social::get_group(&db, "techteam").unwrap().unwrap();
            let members: Vec<String> =
                serde_json::from_str(group.members_json.as_deref().unwrap()).unwrap();
            assert_eq!(members.len(), 2);
        }
    }

    #[tokio::test]
    async fn sqlite_build_context_includes_notes() {
        let dir = TempDir::new().unwrap();
        let conn = test_sqlite_conn();
        let mem = NostrMemory::with_sqlite(dir.path(), conn);

        mem.ensure_npub("aa", "Alice", 100, Some("test"), false).await;
        mem.add_npub_owner_note("aa", "prefers Finnish").await;
        mem.add_npub_note("aa", "asked about Rust").await;

        mem.ensure_group("test", 100).await;
        mem.set_group_purpose("test", "Rust development").await;

        let ctx = mem.build_context("aa", "test").await;
        assert!(ctx.contains("Rust development"));
        assert!(ctx.contains("prefers Finnish"));
        assert!(ctx.contains("asked about Rust"));
    }

    #[tokio::test]
    async fn sqlite_json_migration_on_first_use() {
        let dir = TempDir::new().unwrap();

        // Write legacy JSON file directly
        let json_store = LegacyMemoryStore {
            npubs: {
                let mut m = HashMap::new();
                m.insert(
                    "ff".to_string(),
                    NpubMemory {
                        npub_hex: "ff".to_string(),
                        display_name: "Frank".to_string(),
                        first_seen: 500,
                        first_seen_group: None,
                        notes: vec!["likes coffee".to_string()],
                        owner_notes: Vec::new(),
                        last_interaction: 500,
                        name_history: Vec::new(),
                        profile_metadata: None,
                    },
                );
                m
            },
            groups: {
                let mut m = HashMap::new();
                m.insert(
                    "devteam".to_string(),
                    GroupMemory {
                        group_id: "devteam".to_string(),
                        purpose: Some("Development".to_string()),
                        members_seen: Vec::new(),
                        notes: Vec::new(),
                        last_activity: 600,
                    },
                );
                m
            },
        };
        let json = serde_json::to_string_pretty(&json_store).unwrap();
        std::fs::write(dir.path().join("nostr_memory.json"), json).unwrap();

        // Now open with SQLite — should migrate from JSON
        let conn = test_sqlite_conn();
        let mem = NostrMemory::with_sqlite(dir.path(), conn.clone());

        let npub = mem.get_npub("ff").await.unwrap();
        assert_eq!(npub.display_name, "Frank");
        assert_eq!(npub.notes, vec!["likes coffee"]);

        // Verify it made it into SQLite
        {
            let db = conn.lock();
            let npub = social::get_npub(&db, "ff").unwrap().unwrap();
            assert_eq!(npub.display_name, "Frank");
        }
    }

    // ── Message indexing tests (Phase 2) ─────────────────────────

    #[test]
    fn try_index_message_indexes_substantial_content() {
        let dir = TempDir::new().unwrap();
        let conn = test_sqlite_conn();
        let mem = NostrMemory::with_sqlite(dir.path(), conn.clone());

        mem.try_index_message(
            "evt1", "aabb", Some("techteam"),
            "This is a substantial message about Rust programming",
            1000, 9, false, false,
        );

        let results = mem.search_messages("Rust programming", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].event_id, "evt1");
        assert_eq!(results[0].sender_hex, "aabb");
    }

    #[test]
    fn try_index_message_skips_short_content() {
        let dir = TempDir::new().unwrap();
        let conn = test_sqlite_conn();
        let mem = NostrMemory::with_sqlite(dir.path(), conn.clone());

        mem.try_index_message(
            "evt1", "aabb", Some("techteam"),
            "hi", // too short
            1000, 9, false, false,
        );

        let db = conn.lock();
        assert_eq!(message_index::count_messages(&db).unwrap(), 0);
    }

    #[test]
    fn try_index_message_always_indexes_dm() {
        let dir = TempDir::new().unwrap();
        let conn = test_sqlite_conn();
        let mem = NostrMemory::with_sqlite(dir.path(), conn.clone());

        mem.try_index_message(
            "evt1", "aabb", None,
            "short DM", // short but is_dm=true
            1000, 14, false, true,
        );

        let results = mem.search_messages("short DM", 10);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn try_index_message_always_indexes_bot_mention() {
        let dir = TempDir::new().unwrap();
        let conn = test_sqlite_conn();
        let mem = NostrMemory::with_sqlite(dir.path(), conn.clone());

        mem.try_index_message(
            "evt1", "aabb", Some("devteam"),
            "hey bot!", // short but is_bot_mention=true
            1000, 9, true, false,
        );

        let results = mem.search_messages("hey bot", 10);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_messages_without_sqlite_returns_empty() {
        let dir = TempDir::new().unwrap();
        let mem = NostrMemory::new(dir.path()); // no SQLite

        let results = mem.search_messages("anything", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn try_index_message_without_sqlite_is_noop() {
        let dir = TempDir::new().unwrap();
        let mem = NostrMemory::new(dir.path()); // no SQLite

        // Should not panic
        mem.try_index_message(
            "evt1", "aabb", Some("group"),
            "This is a substantial message for testing",
            1000, 9, false, false,
        );
    }

    // ── Document search tests (Phase 3) ──────────────────────────

    #[test]
    fn search_docs_finds_indexed_content() {
        let dir = TempDir::new().unwrap();
        let conn = test_sqlite_conn();
        let mem = NostrMemory::with_sqlite(dir.path(), conn.clone());

        // Index content directly via doc_index
        {
            let db = conn.lock();
            doc_index::index_content(
                &db,
                "v://test.md",
                "Rust systems programming guide",
                "document",
            )
            .unwrap();
        }

        let results = mem.search_docs("Rust programming", 10);
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("Rust"));
    }

    #[test]
    fn search_docs_without_sqlite_returns_empty() {
        let dir = TempDir::new().unwrap();
        let mem = NostrMemory::new(dir.path());

        let results = mem.search_docs("anything", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn unified_search_finds_across_types() {
        let dir = TempDir::new().unwrap();
        let conn = test_sqlite_conn();
        let mem = NostrMemory::with_sqlite(dir.path(), conn.clone());

        // Index a document
        {
            let db = conn.lock();
            doc_index::index_content(
                &db,
                "v://guide.md",
                "Rust language overview and tutorial",
                "document",
            )
            .unwrap();
        }

        // Index a message
        mem.try_index_message(
            "evt1",
            "aabb",
            Some("devteam"),
            "Discussion about Rust async patterns and performance",
            1000,
            9,
            false,
            false,
        );

        let results = mem.unified_search("Rust", 10);
        assert!(results.len() >= 2, "Expected hits from both docs and messages");

        let sources: Vec<&str> = results.iter().map(|h| h.source()).collect();
        assert!(sources.contains(&"document"), "Should contain document hits");
        assert!(sources.contains(&"message"), "Should contain message hits");
    }

    #[test]
    fn unified_search_without_sqlite_returns_empty() {
        let dir = TempDir::new().unwrap();
        let mem = NostrMemory::new(dir.path());

        let results = mem.unified_search("anything", 10);
        assert!(results.is_empty());
    }
}
