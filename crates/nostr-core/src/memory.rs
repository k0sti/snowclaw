//! Per-npub and per-group memory for the Nostr channel.
//!
//! Stores contextual memories about contacts and groups the agent interacts with.
//! In-memory cache with JSON file persistence (NIP-78 relay persistence is a future follow-up).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

// ── Data structures ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NpubMemory {
    pub npub_hex: String,
    pub display_name: String,
    pub first_seen: u64,
    pub first_seen_group: Option<String>,
    /// Agent's observations and learned context about this person.
    pub notes: Vec<String>,
    /// Guardian-provided notes (e.g. "prefers Finnish", "core team member").
    pub guardian_notes: Vec<String>,
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

/// Combined in-memory store for all Nostr social memory.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NostrMemoryStore {
    pub npubs: HashMap<String, NpubMemory>,
    pub groups: HashMap<String, GroupMemory>,
}

// ── NostrMemory (shared handle) ──────────────────────────────────

/// Thread-safe handle to the Nostr memory store.
#[derive(Clone)]
pub struct NostrMemory {
    store: Arc<RwLock<NostrMemoryStore>>,
    /// Path to the JSON persistence file.
    persist_path: PathBuf,
    /// Track whether there are unsaved changes.
    dirty: Arc<RwLock<bool>>,
}

impl NostrMemory {
    /// Create a new NostrMemory, loading from disk if the file exists.
    pub fn new(persist_dir: &Path) -> Self {
        let persist_path = persist_dir.join("nostr_memory.json");
        let store = if persist_path.exists() {
            match std::fs::read_to_string(&persist_path) {
                Ok(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
                    warn!("Failed to parse nostr_memory.json: {e}; starting fresh");
                    NostrMemoryStore::default()
                }),
                Err(e) => {
                    warn!("Failed to read nostr_memory.json: {e}; starting fresh");
                    NostrMemoryStore::default()
                }
            }
        } else {
            NostrMemoryStore::default()
        };

        let npub_count = store.npubs.len();
        let group_count = store.groups.len();
        if npub_count > 0 || group_count > 0 {
            info!("Loaded nostr memory: {npub_count} contacts, {group_count} groups");
        }

        Self {
            store: Arc::new(RwLock::new(store)),
            persist_path,
            dirty: Arc::new(RwLock::new(false)),
        }
    }

    /// Ensure an npub memory entry exists. Returns true if this is a new contact.
    pub async fn ensure_npub(
        &self,
        hex_pubkey: &str,
        display_name: &str,
        timestamp: u64,
        group: Option<&str>,
    ) -> bool {
        let mut store = self.store.write().await;
        if let Some(existing) = store.npubs.get_mut(hex_pubkey) {
            // Update last interaction
            existing.last_interaction = timestamp;
            // Track name changes
            if existing.display_name != display_name {
                existing
                    .name_history
                    .push((timestamp, existing.display_name.clone()));
                existing.display_name = display_name.to_string();
            }
            false
        } else {
            store.npubs.insert(
                hex_pubkey.to_string(),
                NpubMemory {
                    npub_hex: hex_pubkey.to_string(),
                    display_name: display_name.to_string(),
                    first_seen: timestamp,
                    first_seen_group: group.map(|s| s.to_string()),
                    notes: Vec::new(),
                    guardian_notes: Vec::new(),
                    last_interaction: timestamp,
                    name_history: Vec::new(),
                    profile_metadata: None,
                },
            );
            *self.dirty.write().await = true;
            true
        }
    }

    /// Ensure a group memory entry exists. Returns true if this is a new group.
    pub async fn ensure_group(&self, group_id: &str, timestamp: u64) -> bool {
        let mut store = self.store.write().await;
        if let Some(existing) = store.groups.get_mut(group_id) {
            existing.last_activity = timestamp;
            false
        } else {
            store.groups.insert(
                group_id.to_string(),
                GroupMemory {
                    group_id: group_id.to_string(),
                    purpose: None,
                    members_seen: Vec::new(),
                    notes: Vec::new(),
                    last_activity: timestamp,
                },
            );
            *self.dirty.write().await = true;
            true
        }
    }

    /// Record that an npub was seen in a group.
    pub async fn record_group_member(&self, group_id: &str, hex_pubkey: &str) {
        let mut store = self.store.write().await;
        if let Some(group) = store.groups.get_mut(group_id) {
            if !group.members_seen.contains(&hex_pubkey.to_string()) {
                group.members_seen.push(hex_pubkey.to_string());
                drop(store);
                *self.dirty.write().await = true;
            }
        }
    }

    /// Add a note to an npub's memory.
    pub async fn add_npub_note(&self, hex_pubkey: &str, note: &str) {
        let mut store = self.store.write().await;
        if let Some(npub) = store.npubs.get_mut(hex_pubkey) {
            npub.notes.push(note.to_string());
            drop(store);
            *self.dirty.write().await = true;
        }
    }

    /// Add a guardian note to an npub's memory.
    pub async fn add_npub_guardian_note(&self, hex_pubkey: &str, note: &str) {
        let mut store = self.store.write().await;
        if let Some(npub) = store.npubs.get_mut(hex_pubkey) {
            npub.guardian_notes.push(note.to_string());
            drop(store);
            *self.dirty.write().await = true;
        }
    }

    /// Add a note to a group's memory.
    pub async fn add_group_note(&self, group_id: &str, note: &str) {
        let mut store = self.store.write().await;
        if let Some(group) = store.groups.get_mut(group_id) {
            group.notes.push(note.to_string());
            drop(store);
            *self.dirty.write().await = true;
        }
    }

    /// Set a group's purpose.
    pub async fn set_group_purpose(&self, group_id: &str, purpose: &str) {
        let mut store = self.store.write().await;
        if let Some(group) = store.groups.get_mut(group_id) {
            group.purpose = Some(purpose.to_string());
            drop(store);
            *self.dirty.write().await = true;
        }
    }

    /// Update profile metadata for an npub. Tracks name changes in name_history.
    pub async fn update_profile(
        &self,
        hex_pubkey: &str,
        metadata: ProfileMetadata,
    ) {
        let mut store = self.store.write().await;
        if let Some(npub) = store.npubs.get_mut(hex_pubkey) {
            // Track display_name changes
            let new_name = metadata.display_name.as_deref()
                .or(metadata.name.as_deref())
                .unwrap_or(&npub.display_name);
            if new_name != npub.display_name {
                npub.name_history.push((metadata.fetched_at, npub.display_name.clone()));
                npub.display_name = new_name.to_string();
            }
            npub.profile_metadata = Some(metadata);
            drop(store);
            *self.dirty.write().await = true;
        }
    }

    /// Get npub memory (read-only clone).
    pub async fn get_npub(&self, hex_pubkey: &str) -> Option<NpubMemory> {
        self.store.read().await.npubs.get(hex_pubkey).cloned()
    }

    /// Get group memory (read-only clone).
    pub async fn get_group(&self, group_id: &str) -> Option<GroupMemory> {
        self.store.read().await.groups.get(group_id).cloned()
    }

    /// List all known npubs.
    pub async fn list_npubs(&self) -> Vec<NpubMemory> {
        self.store.read().await.npubs.values().cloned().collect()
    }

    /// Build concise LLM context for a sender in a group.
    pub async fn build_context(&self, sender_hex: &str, group_id: &str) -> String {
        let store = self.store.read().await;
        let mut ctx = String::new();

        // Group context
        if let Some(group) = store.groups.get(group_id) {
            if let Some(ref purpose) = group.purpose {
                ctx.push_str(&format!("[Group #{} purpose: {}]\n", group_id, purpose));
            }
            if !group.notes.is_empty() {
                ctx.push_str(&format!(
                    "[Group #{} notes: {}]\n",
                    group_id,
                    group.notes.join("; ")
                ));
            }
        }

        // Sender context
        if let Some(npub) = store.npubs.get(sender_hex) {
            let mut parts = Vec::new();
            if !npub.guardian_notes.is_empty() {
                parts.push(format!("guardian says: {}", npub.guardian_notes.join("; ")));
            }
            if !npub.notes.is_empty() {
                // Show last 3 notes max to keep context concise
                let recent: Vec<&str> = npub.notes.iter().rev().take(3).map(|s| s.as_str()).collect();
                parts.push(format!("notes: {}", recent.into_iter().rev().collect::<Vec<_>>().join("; ")));
            }
            if !parts.is_empty() {
                ctx.push_str(&format!(
                    "[Known about {}: {}]\n",
                    npub.display_name,
                    parts.join(" | ")
                ));
            }
        }

        ctx
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
        // Store as a note on the agent's npub memory
        let note = format!("[{}] state: {} d={}", agent_name, status, d_tag);
        let mut store = self.store.write().await;
        let npub = store.npubs.entry(agent_hex.to_string()).or_insert_with(|| {
            NpubMemory {
                npub_hex: agent_hex.to_string(),
                display_name: agent_name.to_string(),
                first_seen: timestamp,
                first_seen_group: None,
                notes: Vec::new(),
                guardian_notes: Vec::new(),
                last_interaction: timestamp,
                name_history: Vec::new(),
                profile_metadata: None,
            }
        });
        npub.last_interaction = timestamp;
        // Keep only last 5 state notes to avoid bloat
        npub.notes.retain(|n| !n.starts_with(&format!("[{}] state:", agent_name)));
        npub.notes.push(note);
        drop(store);
        *self.dirty.write().await = true;
    }

    /// Persist to disk if dirty.
    pub async fn flush(&self) -> Result<(), std::io::Error> {
        let is_dirty = *self.dirty.read().await;
        if !is_dirty {
            return Ok(());
        }

        let store = self.store.read().await;
        let json = serde_json::to_string_pretty(&*store)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        drop(store);

        // Ensure parent directory exists
        if let Some(parent) = self.persist_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(&self.persist_path, json)?;
        *self.dirty.write().await = false;
        debug!("Flushed nostr memory to {}", self.persist_path.display());
        Ok(())
    }

    /// Force a flush (for shutdown / periodic save).
    pub async fn force_flush(&self) {
        *self.dirty.write().await = true;
        if let Err(e) = self.flush().await {
            warn!("Failed to flush nostr memory: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn new_contact_auto_created() {
        let dir = TempDir::new().unwrap();
        let mem = NostrMemory::new(dir.path());

        let is_new = mem.ensure_npub("aabb", "Alice", 1000, Some("techteam")).await;
        assert!(is_new);

        let is_new2 = mem.ensure_npub("aabb", "Alice", 1001, Some("techteam")).await;
        assert!(!is_new2);

        let npub = mem.get_npub("aabb").await.unwrap();
        assert_eq!(npub.display_name, "Alice");
        assert_eq!(npub.first_seen, 1000);
        assert_eq!(npub.last_interaction, 1001);
    }

    #[tokio::test]
    async fn name_change_tracked() {
        let dir = TempDir::new().unwrap();
        let mem = NostrMemory::new(dir.path());

        mem.ensure_npub("cc", "Bob", 100, None).await;
        mem.ensure_npub("cc", "Bobby", 200, None).await;

        let npub = mem.get_npub("cc").await.unwrap();
        assert_eq!(npub.display_name, "Bobby");
        assert_eq!(npub.name_history.len(), 1);
        assert_eq!(npub.name_history[0].1, "Bob");
    }

    #[tokio::test]
    async fn group_memory_and_members() {
        let dir = TempDir::new().unwrap();
        let mem = NostrMemory::new(dir.path());

        let is_new = mem.ensure_group("techteam", 1000).await;
        assert!(is_new);

        mem.record_group_member("techteam", "aabb").await;
        mem.record_group_member("techteam", "ccdd").await;
        mem.record_group_member("techteam", "aabb").await; // duplicate

        let group = mem.get_group("techteam").await.unwrap();
        assert_eq!(group.members_seen.len(), 2);
    }

    #[tokio::test]
    async fn build_context_includes_notes() {
        let dir = TempDir::new().unwrap();
        let mem = NostrMemory::new(dir.path());

        mem.ensure_npub("aa", "Alice", 100, Some("test")).await;
        mem.add_npub_guardian_note("aa", "prefers Finnish").await;
        mem.add_npub_note("aa", "asked about Rust").await;

        mem.ensure_group("test", 100).await;
        mem.set_group_purpose("test", "Rust development").await;

        let ctx = mem.build_context("aa", "test").await;
        assert!(ctx.contains("Rust development"));
        assert!(ctx.contains("prefers Finnish"));
        assert!(ctx.contains("asked about Rust"));
    }

    #[tokio::test]
    async fn persistence_roundtrip() {
        let dir = TempDir::new().unwrap();

        {
            let mem = NostrMemory::new(dir.path());
            mem.ensure_npub("ff", "Frank", 500, None).await;
            mem.add_npub_note("ff", "likes coffee").await;
            mem.force_flush().await;
        }

        // Reload from disk
        let mem2 = NostrMemory::new(dir.path());
        let npub = mem2.get_npub("ff").await.unwrap();
        assert_eq!(npub.display_name, "Frank");
        assert_eq!(npub.notes, vec!["likes coffee"]);
    }
}