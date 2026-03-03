//! Collective memory backend — wraps `snow-memory` `SqliteMemoryIndex`
//! to implement the agent runtime `Memory` trait.
//!
//! This backend stores memories as `snow_memory::Memory` entries in a
//! SQLite FTS5 index and ranks recall results using source trust and
//! model-tier weighting from the configured `CollectiveMemoryConfig`.
//!
//! ## Relay sync (Phase 2+3)
//!
//! When relay URLs and an nsec are configured, the backend will:
//! - Publish kind 30078 events to relay after each `store()` (fire-and-forget)
//! - Sync events from relay on startup via `sync_from_relay()`
//! - Track `last_sync_timestamp` in the DB for incremental syncs

use super::traits::{Memory, MemoryCategory, MemoryEntry, RecallContext};
use crate::config::snowclaw_schema::CollectiveMemoryConfig;
use async_trait::async_trait;
use nostr_sdk::nips::nip44;
use parking_lot::Mutex;
use snow_memory::SqliteMemoryIndex;
use snow_memory::types::{Memory as SnowMemory, MemoryTier};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

// ── Conflict detection types ────────────────────────────────────

/// A conflict between memories on the same topic from different sources.
#[derive(Debug, Clone)]
pub struct MemoryConflict {
    pub topic: String,
    pub entries: Vec<ConflictEntry>,
}

/// One side of a memory conflict.
#[derive(Debug, Clone)]
pub struct ConflictEntry {
    pub memory_id: String,
    pub source: String,
    pub summary: String,
    pub confidence: f64,
    pub created_at: u64,
    pub version: u32,
}

/// Agent profile for Nostr kind 0 (metadata) events.
///
/// Published as a replaceable event so relays always serve the latest version.
/// Standard NIP-01 fields (`name`, `about`) are augmented with `snow:*`
/// extension fields for agent-specific metadata.
pub struct AgentProfile {
    pub name: String,
    pub about: String,
    pub model: String,
    pub version: String,
    pub capabilities: Vec<String>,
    pub operator_npub: Option<String>,
}

/// Collective memory backend backed by `snow-memory` `SqliteMemoryIndex`.
pub struct CollectiveMemory {
    index: Mutex<SqliteMemoryIndex>,
    config: CollectiveMemoryConfig,
    #[allow(dead_code)]
    db_path: PathBuf,
    /// Nostr relay client for publish/sync. None = local-only mode.
    relay: Option<RelayState>,
}

/// Holds the nostr_sdk Client + Keys for relay operations.
struct RelayState {
    client: nostr_sdk::Client,
    keys: nostr_sdk::Keys,
}

impl CollectiveMemory {
    /// Create a new collective memory backend.
    ///
    /// `workspace_dir` is used to resolve relative `db_path` values from config.
    /// If `relay_urls` is non-empty and `nsec` is provided, relay sync is enabled.
    pub fn new(
        workspace_dir: &Path,
        config: &CollectiveMemoryConfig,
    ) -> anyhow::Result<Self> {
        Self::new_with_relay(workspace_dir, config, None)
    }

    /// Create with optional relay configuration.
    ///
    /// `nsec` is the Nostr secret key (nsec1... or hex) for signing events.
    /// Relay URLs come from `config.relay_urls`.
    ///
    /// If relay is configured, spawns a background task to connect and
    /// perform an incremental sync from relay.
    pub fn new_with_relay(
        workspace_dir: &Path,
        config: &CollectiveMemoryConfig,
        nsec: Option<&str>,
    ) -> anyhow::Result<Self> {
        let db_path = if Path::new(&config.db_path).is_absolute() {
            PathBuf::from(&config.db_path)
        } else {
            workspace_dir.join(&config.db_path)
        };

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let index = SqliteMemoryIndex::open(&db_path)
            .map_err(|e| anyhow::anyhow!("failed to open collective memory DB: {e}"))?;

        // Initialize metadata table for sync tracking
        init_metadata_table(&index)?;

        let relay = Self::init_relay(config, nsec);

        let mem = Self {
            index: Mutex::new(index),
            config: config.clone(),
            db_path,
            relay,
        };

        // Spawn background relay connect + sync if relay is configured.
        // This is fire-and-forget — errors are logged, not propagated.
        if mem.relay_enabled() {
            // We need to share `mem` with the spawned task. Since we return
            // mem as Box<dyn Memory> (owned), we can't share it easily.
            // Instead, clone the relay state and config for the background task.
            let relay_urls = mem.config.relay_urls.clone();
            let relay_client = mem.relay.as_ref().unwrap().client.clone();
            // For sync, we need index access — use a separate connection to the same DB.
            let sync_db_path = mem.db_path.clone();
            let relay_keys = mem.relay.as_ref().unwrap().keys.clone();
            tokio::spawn(async move {
                // Connect to relays
                for url in &relay_urls {
                    if let Err(e) = relay_client.add_relay(url.as_str()).await {
                        tracing::warn!("collective memory: failed to add relay {url}: {e}");
                    }
                }
                relay_client.connect().await;
                tracing::info!(
                    "collective memory: connected to {} relay(s)",
                    relay_urls.len()
                );

                // Incremental sync from relay
                if let Err(e) = background_sync(&relay_client, &relay_keys, &sync_db_path).await {
                    tracing::warn!("collective memory: startup sync failed: {e}");
                }
            });
        }

        Ok(mem)
    }

    /// Create with an in-memory database (for testing).
    pub fn new_in_memory(config: &CollectiveMemoryConfig) -> anyhow::Result<Self> {
        let index = SqliteMemoryIndex::open_in_memory()
            .map_err(|e| anyhow::anyhow!("failed to open in-memory collective DB: {e}"))?;

        init_metadata_table(&index)?;

        Ok(Self {
            index: Mutex::new(index),
            config: config.clone(),
            db_path: PathBuf::from(":memory:"),
            relay: None,
        })
    }

    /// Create with an in-memory database and relay state (for testing relay publish).
    pub fn new_in_memory_with_relay(
        config: &CollectiveMemoryConfig,
        nsec: &str,
    ) -> anyhow::Result<Self> {
        let index = SqliteMemoryIndex::open_in_memory()
            .map_err(|e| anyhow::anyhow!("failed to open in-memory collective DB: {e}"))?;

        init_metadata_table(&index)?;

        let relay = Self::init_relay(config, Some(nsec));

        Ok(Self {
            index: Mutex::new(index),
            config: config.clone(),
            db_path: PathBuf::from(":memory:"),
            relay,
        })
    }

    /// Try to create a relay state from config + nsec.
    fn init_relay(config: &CollectiveMemoryConfig, nsec: Option<&str>) -> Option<RelayState> {
        let nsec = nsec?;
        if config.relay_urls.is_empty() {
            return None;
        }

        let keys = match nostr_sdk::Keys::parse(nsec) {
            Ok(k) => k,
            Err(e) => {
                tracing::warn!("collective memory: invalid nsec, relay sync disabled: {e}");
                return None;
            }
        };

        let client = nostr_sdk::Client::new(keys.clone());
        Some(RelayState { client, keys })
    }

    /// Connect the relay client to configured relay URLs.
    /// Must be called after construction for relay operations to work.
    pub async fn connect_relays(&self) {
        let relay = match &self.relay {
            Some(r) => r,
            None => return,
        };

        for url in &self.config.relay_urls {
            if let Err(e) = relay.client.add_relay(url.as_str()).await {
                tracing::warn!("collective memory: failed to add relay {url}: {e}");
            }
        }

        relay.client.connect().await;
        tracing::info!(
            "collective memory: connected to {} relay(s)",
            self.config.relay_urls.len()
        );
    }

    /// Whether relay sync is enabled (keys + relays configured).
    pub fn relay_enabled(&self) -> bool {
        self.relay.is_some()
    }

    /// Publish a kind 0 (metadata) profile event for this agent.
    ///
    /// The event is a replaceable event per NIP-01, so each publish
    /// replaces the previous profile for this pubkey on relays.
    pub async fn publish_agent_profile(&self, profile: AgentProfile) -> anyhow::Result<()> {
        let relay = self
            .relay
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("relay not configured, cannot publish profile"))?;

        let mut content = serde_json::json!({
            "name": profile.name,
            "about": profile.about,
            "snow:model": profile.model,
            "snow:version": profile.version,
            "snow:capabilities": profile.capabilities,
        });

        if let Some(ref npub) = profile.operator_npub {
            content["snow:operator"] = serde_json::json!(npub);
        }

        let builder = nostr_sdk::EventBuilder::new(
            nostr_sdk::Kind::Metadata,
            content.to_string(),
        );

        relay
            .client
            .send_event_builder(builder)
            .await
            .map_err(|e| anyhow::anyhow!("failed to publish agent profile: {e}"))?;

        tracing::info!("collective memory: published agent profile (kind 0)");
        Ok(())
    }

    /// Fetch a remote agent's profile (kind 0) from relay.
    ///
    /// Returns `None` if no profile event is found for the given pubkey.
    pub async fn fetch_agent_profile(&self, pubkey: &str) -> anyhow::Result<Option<AgentProfile>> {
        let relay = self
            .relay
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("relay not configured, cannot fetch profile"))?;

        let pk = nostr_sdk::PublicKey::parse(pubkey)
            .map_err(|e| anyhow::anyhow!("invalid pubkey: {e}"))?;

        let filter = nostr_sdk::Filter::new()
            .author(pk)
            .kind(nostr_sdk::Kind::Metadata)
            .limit(1);

        let events = relay
            .client
            .fetch_events(filter, Duration::from_secs(10))
            .await
            .map_err(|e| anyhow::anyhow!("failed to fetch agent profile: {e}"))?;

        let event = match events.into_iter().next() {
            Some(e) => e,
            None => return Ok(None),
        };

        let v: serde_json::Value = serde_json::from_str(&event.content)
            .map_err(|e| anyhow::anyhow!("invalid profile JSON: {e}"))?;

        Ok(Some(AgentProfile {
            name: v["name"].as_str().unwrap_or_default().to_string(),
            about: v["about"].as_str().unwrap_or_default().to_string(),
            model: v["snow:model"].as_str().unwrap_or_default().to_string(),
            version: v["snow:version"].as_str().unwrap_or_default().to_string(),
            capabilities: v["snow:capabilities"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            operator_npub: v["snow:operator"].as_str().map(String::from),
        }))
    }

    /// Store a memory with an explicit tier override.
    ///
    /// Used by the `memory_store` tool when the agent or user specifies a tier.
    pub async fn store_with_tier(
        &self,
        key: &str,
        content: &str,
        _category: MemoryCategory,
        tier: MemoryTier,
    ) -> anyhow::Result<()> {
        let id = Uuid::new_v4().to_string();
        let (summary, detail) = match content.split_once("\n\n") {
            Some((s, d)) => (s.to_string(), d.to_string()),
            None => (content.to_string(), String::new()),
        };

        let source = self
            .relay
            .as_ref()
            .map(|r| r.keys.public_key().to_hex())
            .unwrap_or_else(|| "self".to_string());

        let memory = SnowMemory {
            id,
            tier,
            topic: key.to_string(),
            summary,
            detail,
            context: None,
            source,
            model: String::new(),
            confidence: 0.8,
            supersedes: None,
            version: 1,
            tags: vec![],
            created_at: now_unix(),
        };

        {
            let idx = self.index.lock();
            idx.upsert(&memory, None)
                .map_err(|e| anyhow::anyhow!("collective store failed: {e}"))?;
        }

        self.publish_to_relay(&memory);
        Ok(())
    }

    /// Publish a memory to relay as a kind 30078 NIP-78 event.
    ///
    /// If the memory tier is `Private`, content is encrypted with NIP-44
    /// (self-to-self) before publishing, and an `["encrypted", "nip44"]` tag
    /// is added so sync can detect and decrypt it.
    ///
    /// Fire-and-forget: errors are logged, not propagated.
    fn publish_to_relay(&self, memory: &SnowMemory) {
        let relay = match &self.relay {
            Some(r) => r,
            None => return,
        };

        let mem_event = snow_memory::event::memory_to_event(memory);

        // Encrypt content for Private tier memories
        let (content, extra_tags) = match &memory.tier {
            MemoryTier::Private(_) => {
                match nip44::encrypt(
                    relay.keys.secret_key(),
                    &relay.keys.public_key(),
                    mem_event.content.as_bytes(),
                    nip44::Version::V2,
                ) {
                    Ok(ciphertext) => {
                        tracing::debug!(
                            "collective memory: encrypted Private tier event for '{}'",
                            memory.topic
                        );
                        let tag = nostr_sdk::Tag::custom(
                            nostr_sdk::TagKind::custom("encrypted"),
                            vec!["nip44".to_string()],
                        );
                        (ciphertext, vec![tag])
                    }
                    Err(e) => {
                        tracing::warn!(
                            "collective memory: NIP-44 encrypt failed for '{}', publishing plaintext: {e}",
                            memory.topic
                        );
                        (mem_event.content.clone(), vec![])
                    }
                }
            }
            _ => (mem_event.content.clone(), vec![]),
        };

        // Build tags from mem_event + any encryption tag
        let mut tags: Vec<nostr_sdk::Tag> = mem_event
            .tags
            .iter()
            .map(|(k, v)| {
                nostr_sdk::Tag::custom(
                    nostr_sdk::TagKind::custom(k.clone()),
                    vec![v.clone()],
                )
            })
            .collect();
        tags.extend(extra_tags);

        let builder = nostr_sdk::EventBuilder::new(
            nostr_sdk::Kind::Custom(30078),
            &content,
        )
        .tags(tags);

        let client = relay.client.clone();

        tokio::spawn(async move {
            match client.send_event_builder(builder).await {
                Ok(_output) => {
                    tracing::debug!("collective memory: published to relay");
                }
                Err(e) => {
                    tracing::warn!("collective memory: relay publish failed: {e}");
                }
            }
        });
    }

    /// Sync memories from relay into local DB.
    ///
    /// Uses `last_sync_timestamp` for incremental sync. Returns the number
    /// of new/updated entries synced.
    pub async fn sync_from_relay(&self) -> anyhow::Result<usize> {
        let relay = match &self.relay {
            Some(r) => r,
            None => anyhow::bail!("relay sync not configured (no keys or relay URLs)"),
        };

        let pubkey = relay.keys.public_key();
        let last_sync = {
            let idx = self.index.lock();
            get_last_sync_timestamp(&idx)
        };

        let mut filter = nostr_sdk::Filter::new()
            .author(pubkey)
            .kind(nostr_sdk::Kind::Custom(30078));

        if let Some(ts) = last_sync {
            filter = filter.since(nostr_sdk::Timestamp::from(ts));
        }

        let events = relay
            .client
            .fetch_events(filter, Duration::from_secs(30))
            .await
            .map_err(|e| anyhow::anyhow!("collective memory: relay fetch failed: {e}"))?;

        let events: Vec<nostr_sdk::Event> = events.into_iter().collect();
        let total = events.len();
        let mut synced = 0usize;
        let mut max_ts = last_sync.unwrap_or(0);

        for event in &events {
            let mut mem_event = nostr_event_to_memory_event(event);

            // Decrypt NIP-44 encrypted content if tagged
            if is_nip44_encrypted(event) {
                match nip44::decrypt(
                    relay.keys.secret_key(),
                    &event.pubkey,
                    &mem_event.content,
                ) {
                    Ok(plaintext) => {
                        mem_event.content = plaintext;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "collective memory: NIP-44 decrypt failed for {}, skipping: {e}",
                            event.id.to_hex()
                        );
                        continue;
                    }
                }
            }

            match snow_memory::event::memory_from_event(&mem_event) {
                Ok(memory) => {
                    let event_json = serde_json::to_string(
                        &serde_json::json!({
                            "id": event.id.to_hex(),
                            "kind": event.kind.as_u16(),
                            "pubkey": event.pubkey.to_hex(),
                            "created_at": event.created_at.as_secs(),
                        })
                    ).ok();

                    let idx = self.index.lock();
                    if let Err(e) = idx.upsert(&memory, event_json.as_deref()) {
                        tracing::warn!("collective memory: failed to upsert synced event: {e}");
                        continue;
                    }
                    synced += 1;
                    max_ts = max_ts.max(event.created_at.as_secs());
                }
                Err(e) => {
                    tracing::debug!(
                        "collective memory: skipping non-memory event {}: {e}",
                        event.id.to_hex()
                    );
                }
            }
        }

        // Update last sync timestamp
        if synced > 0 {
            let idx = self.index.lock();
            if let Err(e) = set_last_sync_timestamp(&idx, max_ts) {
                tracing::warn!("collective memory: failed to update sync timestamp: {e}");
            }
        }

        tracing::info!(
            "collective memory: synced {synced}/{total} events from relay"
        );
        Ok(synced)
    }

    /// Promote a memory to a higher visibility tier.
    ///
    /// Validates promotion direction: Private → Group or Public, Group → Public.
    /// Demotions and same-tier changes are rejected.
    pub async fn promote(&self, key: &str, new_tier: MemoryTier) -> anyhow::Result<()> {
        // Look up the existing memory by exact topic match.
        let existing = {
            let idx = self.index.lock();
            idx.get_by_topic(key)
                .map_err(|e| anyhow::anyhow!("promote lookup failed: {e}"))?
        };

        let existing = match existing {
            Some(m) => m,
            None => anyhow::bail!("memory not found: {key}"),
        };

        // Validate promotion direction
        let current_rank = tier_rank(&existing.tier);
        let new_rank = tier_rank(&new_tier);

        if new_rank <= current_rank {
            anyhow::bail!(
                "cannot promote from {} to {}: tier must increase in visibility",
                tier_label(&existing.tier),
                tier_label(&new_tier),
            );
        }

        // Create a new version with the promoted tier
        let promoted = SnowMemory {
            id: existing.id.clone(),
            tier: new_tier,
            topic: existing.topic.clone(),
            summary: existing.summary.clone(),
            detail: existing.detail.clone(),
            context: existing.context.clone(),
            source: existing.source.clone(),
            model: existing.model.clone(),
            confidence: existing.confidence,
            supersedes: Some(existing.id.clone()),
            version: existing.version + 1,
            tags: existing.tags.clone(),
            created_at: now_unix(),
        };

        // Upsert locally
        {
            let idx = self.index.lock();
            idx.upsert(&promoted, None)
                .map_err(|e| anyhow::anyhow!("promote upsert failed: {e}"))?;
        }

        // Publish to relay with new tier
        self.publish_to_relay(&promoted);
        Ok(())
    }

    /// Full resync: ignore last_sync_timestamp and fetch all events.
    pub async fn full_resync(&self) -> anyhow::Result<usize> {
        // Reset the timestamp so sync_from_relay fetches everything
        {
            let idx = self.index.lock();
            let _ = set_last_sync_timestamp(&idx, 0);
        }
        self.sync_from_relay().await
    }

    /// Detect conflicting memories for a given topic.
    ///
    /// Searches the index for memories matching `topic`, then identifies
    /// entries that share the same topic but come from different sources.
    /// Memories from the same source with different versions are NOT conflicts
    /// (they form a supersedes chain).
    ///
    /// Returns an empty vec when no conflicts exist.
    pub fn detect_conflicts(&self, topic: &str) -> anyhow::Result<Vec<MemoryConflict>> {
        let idx = self.index.lock();

        // Search for memories matching the topic. Use FTS to find candidates,
        // then filter to exact topic matches.
        let candidates = idx
            .search(topic, None, 100)
            .map_err(|e| anyhow::anyhow!("conflict detection search failed: {e}"))?;

        // Group by exact topic
        let mut by_topic: std::collections::HashMap<String, Vec<SnowMemory>> =
            std::collections::HashMap::new();
        for (mem, _score) in candidates {
            by_topic.entry(mem.topic.clone()).or_default().push(mem);
        }

        let mut conflicts = Vec::new();
        for (topic_key, mems) in by_topic {
            if mems.len() < 2 {
                continue;
            }

            // Collect distinct sources
            let mut sources: Vec<&str> = mems.iter().map(|m| m.source.as_str()).collect();
            sources.sort();
            sources.dedup();

            // Only flag as conflict if multiple distinct sources exist
            if sources.len() > 1 {
                let entries = mems
                    .iter()
                    .map(|m| ConflictEntry {
                        memory_id: m.id.clone(),
                        source: m.source.clone(),
                        summary: m.summary.clone(),
                        confidence: m.confidence,
                        created_at: m.created_at,
                        version: m.version,
                    })
                    .collect();

                conflicts.push(MemoryConflict {
                    topic: topic_key,
                    entries,
                });
            }
        }

        Ok(conflicts)
    }
}

/// Background sync: fetch events from relay and upsert into local DB.
/// Uses a separate DB connection since this runs on a spawned task.
async fn background_sync(
    client: &nostr_sdk::Client,
    keys: &nostr_sdk::Keys,
    db_path: &Path,
) -> anyhow::Result<()> {
    // Open a separate connection for the background sync
    let index = if db_path.to_str() == Some(":memory:") {
        return Ok(()); // Can't sync in-memory DB from background task
    } else {
        SqliteMemoryIndex::open(db_path)
            .map_err(|e| anyhow::anyhow!("background sync: failed to open DB: {e}"))?
    };

    init_metadata_table(&index)?;

    let last_sync = get_last_sync_timestamp(&index);
    let pubkey = keys.public_key();

    let mut filter = nostr_sdk::Filter::new()
        .author(pubkey)
        .kind(nostr_sdk::Kind::Custom(30078));

    if let Some(ts) = last_sync {
        filter = filter.since(nostr_sdk::Timestamp::from(ts));
    }

    let events = client
        .fetch_events(filter, Duration::from_secs(30))
        .await
        .map_err(|e| anyhow::anyhow!("background sync: relay fetch failed: {e}"))?;

    let events: Vec<nostr_sdk::Event> = events.into_iter().collect();
    let total = events.len();
    let mut synced = 0usize;
    let mut max_ts = last_sync.unwrap_or(0);

    for event in &events {
        let mut mem_event = nostr_event_to_memory_event(event);

        // Decrypt NIP-44 encrypted content if tagged
        if is_nip44_encrypted(event) {
            match nip44::decrypt(keys.secret_key(), &event.pubkey, &mem_event.content) {
                Ok(plaintext) => {
                    mem_event.content = plaintext;
                }
                Err(e) => {
                    tracing::warn!(
                        "background sync: NIP-44 decrypt failed for {}, skipping: {e}",
                        event.id.to_hex()
                    );
                    continue;
                }
            }
        }

        match snow_memory::event::memory_from_event(&mem_event) {
            Ok(memory) => {
                if let Err(e) = index.upsert(&memory, None) {
                    tracing::warn!("background sync: failed to upsert event: {e}");
                    continue;
                }
                synced += 1;
                max_ts = max_ts.max(event.created_at.as_secs());
            }
            Err(e) => {
                tracing::debug!(
                    "background sync: skipping non-memory event {}: {e}",
                    event.id.to_hex()
                );
            }
        }
    }

    if synced > 0 {
        if let Err(e) = set_last_sync_timestamp(&index, max_ts) {
            tracing::warn!("background sync: failed to update sync timestamp: {e}");
        }
    }

    tracing::info!("collective memory: synced {synced}/{total} events from relay");
    Ok(())
}

/// Check if a nostr event has the `["encrypted", "nip44"]` tag.
fn is_nip44_encrypted(event: &nostr_sdk::Event) -> bool {
    event.tags.iter().any(|t| {
        let s = t.as_slice();
        s.first().map(|v| v.as_str()) == Some("encrypted")
            && s.get(1).map(|v| v.as_str()) == Some("nip44")
    })
}

// ── Conversion helpers ──────────────────────────────────────────

/// Convert a `snow_memory::Memory` to a `nostr_sdk::EventBuilder` (kind 30078).
fn memory_to_event_builder(memory: &SnowMemory) -> nostr_sdk::EventBuilder {
    let mem_event = snow_memory::event::memory_to_event(memory);

    let tags: Vec<nostr_sdk::Tag> = mem_event
        .tags
        .iter()
        .map(|(k, v)| {
            nostr_sdk::Tag::custom(
                nostr_sdk::TagKind::custom(k.clone()),
                vec![v.clone()],
            )
        })
        .collect();

    nostr_sdk::EventBuilder::new(
        nostr_sdk::Kind::Custom(30078),
        &mem_event.content,
    )
    .tags(tags)
}

/// Convert a `nostr_sdk::Event` to a `snow_memory::event::MemoryEvent`.
fn nostr_event_to_memory_event(event: &nostr_sdk::Event) -> snow_memory::event::MemoryEvent {
    let tags = event
        .tags
        .iter()
        .filter_map(|t| {
            let s = t.as_slice();
            if s.len() >= 2 {
                Some((s[0].to_string(), s[1].to_string()))
            } else {
                None
            }
        })
        .collect();

    snow_memory::event::MemoryEvent {
        id: event.id.to_hex(),
        kind: event.kind.as_u16() as u64,
        pubkey: event.pubkey.to_hex(),
        created_at: event.created_at.as_secs(),
        tags,
        content: event.content.clone(),
    }
}

// ── Metadata table for sync tracking ────────────────────────────

fn init_metadata_table(index: &SqliteMemoryIndex) -> anyhow::Result<()> {
    index
        .execute_raw(
            "CREATE TABLE IF NOT EXISTS collective_metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
        )
        .map_err(|e| anyhow::anyhow!("failed to create metadata table: {e}"))?;
    Ok(())
}

fn get_last_sync_timestamp(index: &SqliteMemoryIndex) -> Option<u64> {
    index
        .query_raw(
            "SELECT value FROM collective_metadata WHERE key = 'last_sync_timestamp'",
        )
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
}

fn set_last_sync_timestamp(index: &SqliteMemoryIndex, ts: u64) -> anyhow::Result<()> {
    index
        .execute_raw(&format!(
            "INSERT INTO collective_metadata (key, value) VALUES ('last_sync_timestamp', '{ts}') \
             ON CONFLICT(key) DO UPDATE SET value = '{ts}'"
        ))
        .map_err(|e| anyhow::anyhow!("failed to set sync timestamp: {e}"))?;
    Ok(())
}

// ── Category/tier/entry conversions ─────────────────────────────

/// Convert a `MemoryCategory` to a `MemoryTier`.
fn category_to_tier(category: &MemoryCategory) -> MemoryTier {
    match category {
        MemoryCategory::Core => MemoryTier::Public,
        MemoryCategory::Daily => MemoryTier::Public,
        MemoryCategory::Conversation => MemoryTier::Private("self".to_string()),
        MemoryCategory::Custom(name) if name == "group" => {
            MemoryTier::Group("default".to_string())
        }
        MemoryCategory::Custom(_) => MemoryTier::Public,
    }
}

/// Classify memory tier based on key scope prefix.
///
/// Key format: `<scope>:<subject>:<detail>` — the first segment determines
/// the default privacy tier per the collective memory v2 key convention.
pub fn scope_to_tier(key: &str) -> MemoryTier {
    match key.split(':').next().unwrap_or("") {
        "core" => MemoryTier::Public,
        "lesson" => MemoryTier::Public,
        "pref" => MemoryTier::Private("self".to_string()),
        "contact" => MemoryTier::Private("self".to_string()),
        "conv" => MemoryTier::Private("self".to_string()),
        "group" => {
            let group_id = key.split(':').nth(1).unwrap_or("default");
            MemoryTier::Group(group_id.to_string())
        }
        _ => MemoryTier::Public,
    }
}

/// Check whether a memory with the given tier should be visible in the given context.
fn tier_visible_in_context(tier: &MemoryTier, context: &RecallContext) -> bool {
    match tier {
        MemoryTier::Public => true,
        MemoryTier::Private(_) => context.is_main_session,
        MemoryTier::Group(group_id) => {
            context.is_main_session
                || context.group_id.as_deref() == Some(group_id.as_str())
        }
    }
}

/// Numeric rank for tier visibility ordering: Private(0) < Group(1) < Public(2).
fn tier_rank(tier: &MemoryTier) -> u8 {
    match tier {
        MemoryTier::Private(_) => 0,
        MemoryTier::Group(_) => 1,
        MemoryTier::Public => 2,
    }
}

/// Human-readable label for a tier (used in error messages).
fn tier_label(tier: &MemoryTier) -> &'static str {
    match tier {
        MemoryTier::Private(_) => "private",
        MemoryTier::Group(_) => "group",
        MemoryTier::Public => "public",
    }
}

/// Convert a `MemoryTier` back to a `MemoryCategory`.
fn tier_to_category(tier: &MemoryTier) -> MemoryCategory {
    match tier {
        MemoryTier::Public => MemoryCategory::Core,
        MemoryTier::Group(id) => MemoryCategory::Custom(format!("group:{id}")),
        MemoryTier::Private(_) => MemoryCategory::Conversation,
    }
}

/// Convert a `SnowMemory` to a runtime `MemoryEntry`.
fn snow_to_entry(m: &SnowMemory, score: Option<f64>) -> MemoryEntry {
    MemoryEntry {
        id: m.id.clone(),
        key: m.topic.clone(),
        content: if m.detail.is_empty() {
            m.summary.clone()
        } else {
            format!("{}\n\n{}", m.summary, m.detail)
        },
        category: tier_to_category(&m.tier),
        timestamp: chrono::DateTime::from_timestamp(m.created_at as i64, 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default(),
        session_id: None,
        score,
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[async_trait]
impl Memory for CollectiveMemory {
    fn name(&self) -> &str {
        "collective"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        _session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let id = Uuid::new_v4().to_string();
        let (summary, detail) = match content.split_once("\n\n") {
            Some((s, d)) => (s.to_string(), d.to_string()),
            None => (content.to_string(), String::new()),
        };

        // Use our pubkey as source if relay is configured, otherwise "self"
        let source = self
            .relay
            .as_ref()
            .map(|r| r.keys.public_key().to_hex())
            .unwrap_or_else(|| "self".to_string());

        // Determine tier: if the key has a recognized scope prefix, use that;
        // otherwise fall back to category-based classification.
        let tier = if key.contains(':') {
            scope_to_tier(key)
        } else {
            category_to_tier(&category)
        };

        let memory = SnowMemory {
            id,
            tier,
            topic: key.to_string(),
            summary,
            detail,
            context: None,
            source,
            model: String::new(),
            confidence: 0.8,
            supersedes: None,
            version: 1,
            tags: vec![],
            created_at: now_unix(),
        };

        // Store locally first
        {
            let idx = self.index.lock();
            idx.upsert(&memory, None)
                .map_err(|e| anyhow::anyhow!("collective store failed: {e}"))?;
        }

        // Fire-and-forget relay publish
        self.publish_to_relay(&memory);

        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        _session_id: Option<&str>,
        context: Option<&RecallContext>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let sm_config = self.config.to_snow_memory_config();
        let idx = self.index.lock();

        // Fetch extra results when filtering so we still return up to `limit` after filtering.
        let fetch_limit = if context.is_some() { limit * 3 } else { limit };
        let results = idx
            .ranked_search(query, None, &sm_config, fetch_limit)
            .map_err(|e| anyhow::anyhow!("collective recall failed: {e}"))?;

        let entries: Vec<MemoryEntry> = results
            .iter()
            .filter(|r| {
                match context {
                    Some(ctx) => tier_visible_in_context(&r.memory.tier, ctx),
                    None => true, // no context = no filtering
                }
            })
            .take(limit)
            .map(|r| snow_to_entry(&r.memory, Some(r.effective_score)))
            .collect();

        Ok(entries)
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let idx = self.index.lock();
        let result = idx
            .get_by_topic(key)
            .map_err(|e| anyhow::anyhow!("collective get failed: {e}"))?;
        Ok(result.as_ref().map(|m| snow_to_entry(m, None)))
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        _session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let tier_filter = category.map(|c| {
            match category_to_tier(c) {
                MemoryTier::Public => "public",
                MemoryTier::Group(_) => "group:",
                MemoryTier::Private(_) => "private:",
            }
        });

        let idx = self.index.lock();
        let memories = idx
            .list_all(tier_filter, 1000)
            .map_err(|e| anyhow::anyhow!("collective list failed: {e}"))?;

        Ok(memories.iter().map(|m| snow_to_entry(m, None)).collect())
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        let idx = self.index.lock();
        idx.delete_by_topic(key)
            .map_err(|e| anyhow::anyhow!("collective forget failed: {e}"))
    }

    async fn count(&self) -> anyhow::Result<usize> {
        let idx = self.index.lock();
        idx.count()
            .map_err(|e| anyhow::anyhow!("collective count failed: {e}"))
    }

    async fn promote(&self, key: &str, new_tier: MemoryTier) -> anyhow::Result<()> {
        CollectiveMemory::promote(self, key, new_tier).await
    }

    async fn health_check(&self) -> bool {
        self.index.lock().count().is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::ToBech32;

    fn test_config() -> CollectiveMemoryConfig {
        CollectiveMemoryConfig::default()
    }

    #[tokio::test]
    async fn store_and_count() {
        let mem = CollectiveMemory::new_in_memory(&test_config()).unwrap();
        assert_eq!(mem.count().await.unwrap(), 0);

        mem.store("rust/errors", "How to handle errors in Rust", MemoryCategory::Core, None)
            .await
            .unwrap();
        assert_eq!(mem.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn store_and_recall() {
        let mut cfg = test_config();
        // Add self as a trusted source so ranking doesn't zero out
        cfg.source_preferences.push(
            crate::config::snowclaw_schema::CollectiveSourceEntry {
                npub: Some("self".to_string()),
                group: None,
                trust: 1.0,
            },
        );

        let mem = CollectiveMemory::new_in_memory(&cfg).unwrap();

        mem.store(
            "rust/errors",
            "Error handling with Result and Option types",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();

        mem.store(
            "nostr/nip44",
            "NIP-44 encryption for private messages",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();

        let results = mem.recall("error handling", 10, None, None).await.unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].key, "rust/errors");
        assert!(results[0].score.unwrap() > 0.0);
    }

    #[tokio::test]
    async fn health_check_passes() {
        let mem = CollectiveMemory::new_in_memory(&test_config()).unwrap();
        assert!(mem.health_check().await);
    }

    #[tokio::test]
    async fn name_returns_collective() {
        let mem = CollectiveMemory::new_in_memory(&test_config()).unwrap();
        assert_eq!(mem.name(), "collective");
    }

    // ── Conversion tests ────────────────────────────────────────

    #[test]
    fn memory_to_event_builder_roundtrip() {
        let memory = SnowMemory {
            id: "test123".to_string(),
            tier: MemoryTier::Public,
            topic: "rust/errors".to_string(),
            summary: "Use anyhow for errors".to_string(),
            detail: "Prefer anyhow::Result in app code.".to_string(),
            context: None,
            source: "deadbeef".to_string(),
            model: "test/model".to_string(),
            confidence: 0.9,
            supersedes: None,
            version: 1,
            tags: vec!["rust".to_string()],
            created_at: 1700000000,
        };

        // Convert Memory -> MemoryEvent -> nostr_sdk EventBuilder tags
        let mem_event = snow_memory::event::memory_to_event(&memory);
        let builder = memory_to_event_builder(&memory);

        // Verify the builder produces expected kind
        // We can't easily inspect EventBuilder internals, but we can
        // sign it and check the resulting event
        let keys = nostr_sdk::Keys::generate();
        let event = builder.sign_with_keys(&keys).unwrap();

        assert_eq!(event.kind, nostr_sdk::Kind::Custom(30078));
        assert_eq!(event.content, mem_event.content);

        // Check d-tag is present
        let d_tag = event
            .tags
            .iter()
            .find(|t| t.as_slice().first().map(|s| s.as_str()) == Some("d"))
            .and_then(|t| t.as_slice().get(1).map(|s| s.to_string()));
        assert_eq!(d_tag, Some("snow:memory:rust/errors".to_string()));
    }

    #[test]
    fn nostr_event_to_memory_event_conversion() {
        let keys = nostr_sdk::Keys::generate();

        let tags = vec![
            nostr_sdk::Tag::custom(
                nostr_sdk::TagKind::custom("d"),
                vec!["snow:memory:test/topic".to_string()],
            ),
            nostr_sdk::Tag::custom(
                nostr_sdk::TagKind::custom("snow:tier"),
                vec!["public".to_string()],
            ),
            nostr_sdk::Tag::custom(
                nostr_sdk::TagKind::custom("snow:model"),
                vec!["test/model".to_string()],
            ),
            nostr_sdk::Tag::custom(
                nostr_sdk::TagKind::custom("snow:confidence"),
                vec!["0.85".to_string()],
            ),
            nostr_sdk::Tag::custom(
                nostr_sdk::TagKind::custom("snow:source"),
                vec![keys.public_key().to_hex()],
            ),
            nostr_sdk::Tag::custom(
                nostr_sdk::TagKind::custom("snow:version"),
                vec!["1".to_string()],
            ),
        ];

        let content = serde_json::json!({
            "summary": "Test summary",
            "detail": "Test detail",
        })
        .to_string();

        let event = nostr_sdk::EventBuilder::new(
            nostr_sdk::Kind::Custom(30078),
            &content,
        )
        .tags(tags)
        .sign_with_keys(&keys)
        .unwrap();

        let mem_event = nostr_event_to_memory_event(&event);

        assert_eq!(mem_event.kind, 30078);
        assert_eq!(mem_event.pubkey, keys.public_key().to_hex());
        assert_eq!(mem_event.content, content);

        // Should parse into a valid Memory
        let memory = snow_memory::event::memory_from_event(&mem_event).unwrap();
        assert_eq!(memory.topic, "test/topic");
        assert_eq!(memory.summary, "Test summary");
        assert_eq!(memory.confidence, 0.85);
    }

    #[test]
    fn full_roundtrip_memory_through_nostr_event() {
        let keys = nostr_sdk::Keys::generate();

        let original = SnowMemory {
            id: "rt_test".to_string(),
            tier: MemoryTier::Public,
            topic: "nostr/nip78".to_string(),
            summary: "NIP-78 for app data".to_string(),
            detail: "Use kind 30078 for application-specific data.".to_string(),
            context: None,
            source: keys.public_key().to_hex(),
            model: "anthropic/claude-opus-4-6".to_string(),
            confidence: 0.92,
            supersedes: None,
            version: 1,
            tags: vec!["nostr".to_string(), "nip78".to_string()],
            created_at: 1700000000,
        };

        // Memory -> EventBuilder -> sign -> Event -> MemoryEvent -> Memory
        let builder = memory_to_event_builder(&original);
        let event = builder.sign_with_keys(&keys).unwrap();
        let mem_event = nostr_event_to_memory_event(&event);
        let recovered = snow_memory::event::memory_from_event(&mem_event).unwrap();

        assert_eq!(recovered.topic, original.topic);
        assert_eq!(recovered.summary, original.summary);
        assert_eq!(recovered.detail, original.detail);
        assert_eq!(recovered.model, original.model);
        assert_eq!(recovered.confidence, original.confidence);
        assert_eq!(recovered.version, original.version);
        assert_eq!(recovered.tags, original.tags);
        assert_eq!(recovered.source, original.source);
    }

    #[test]
    fn relay_disabled_when_no_config() {
        let mem = CollectiveMemory::new_in_memory(&test_config()).unwrap();
        assert!(!mem.relay_enabled());
    }

    #[test]
    fn relay_disabled_when_no_relays() {
        let config = CollectiveMemoryConfig {
            relay_urls: vec![],
            ..CollectiveMemoryConfig::default()
        };
        let keys = nostr_sdk::Keys::generate();
        let nsec = keys.secret_key().to_bech32().unwrap();
        let mem = CollectiveMemory::new_in_memory_with_relay(&config, &nsec).unwrap();
        assert!(!mem.relay_enabled());
    }

    #[test]
    fn relay_enabled_with_config() {
        let config = CollectiveMemoryConfig {
            relay_urls: vec!["wss://relay.example.com".to_string()],
            ..CollectiveMemoryConfig::default()
        };
        let keys = nostr_sdk::Keys::generate();
        let nsec = keys.secret_key().to_bech32().unwrap();
        let mem = CollectiveMemory::new_in_memory_with_relay(&config, &nsec).unwrap();
        assert!(mem.relay_enabled());
    }

    #[test]
    fn is_nip44_encrypted_detects_tag() {
        let keys = nostr_sdk::Keys::generate();

        // Event without encryption tag
        let plain_event = nostr_sdk::EventBuilder::new(
            nostr_sdk::Kind::Custom(30078),
            "plaintext",
        )
        .sign_with_keys(&keys)
        .unwrap();
        assert!(!is_nip44_encrypted(&plain_event));

        // Event with encryption tag
        let enc_event = nostr_sdk::EventBuilder::new(
            nostr_sdk::Kind::Custom(30078),
            "ciphertext",
        )
        .tags(vec![nostr_sdk::Tag::custom(
            nostr_sdk::TagKind::custom("encrypted"),
            vec!["nip44".to_string()],
        )])
        .sign_with_keys(&keys)
        .unwrap();
        assert!(is_nip44_encrypted(&enc_event));
    }

    #[test]
    fn nip44_encrypt_decrypt_roundtrip_collective() {
        let keys = nostr_sdk::Keys::generate();
        let plaintext = "secret collective memory content";

        let ciphertext = nip44::encrypt(
            keys.secret_key(),
            &keys.public_key(),
            plaintext,
            nip44::Version::V2,
        )
        .expect("NIP-44 encrypt should succeed");

        assert_ne!(ciphertext, plaintext);

        let decrypted = nip44::decrypt(keys.secret_key(), &keys.public_key(), &ciphertext)
            .expect("NIP-44 decrypt should succeed");

        assert_eq!(decrypted, plaintext);
    }

    #[tokio::test]
    async fn store_with_tier_stores_correct_tier() {
        let mem = CollectiveMemory::new_in_memory(&test_config()).unwrap();

        mem.store_with_tier(
            "contact:abc123",
            "Friend's npub",
            MemoryCategory::Core,
            MemoryTier::Private("self".to_string()),
        )
        .await
        .unwrap();

        let entry = mem.get("contact:abc123").await.unwrap();
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().content, "Friend's npub");
    }

    #[test]
    fn scope_to_tier_classifies_correctly() {
        assert!(matches!(scope_to_tier("core:timezone"), MemoryTier::Public));
        assert!(matches!(scope_to_tier("lesson:rust"), MemoryTier::Public));
        assert!(matches!(scope_to_tier("pref:lang"), MemoryTier::Private(_)));
        assert!(matches!(scope_to_tier("contact:abc"), MemoryTier::Private(_)));
        assert!(matches!(scope_to_tier("conv:session1"), MemoryTier::Private(_)));
        assert!(matches!(scope_to_tier("group:nostr-dev"), MemoryTier::Group(_)));
        assert!(matches!(scope_to_tier("unknown_key"), MemoryTier::Public));
    }

    #[test]
    fn agent_profile_kind0_event_structure() {
        let keys = nostr_sdk::Keys::generate();

        let profile = AgentProfile {
            name: "snow-studio".to_string(),
            about: "Snowclaw instance on studio".to_string(),
            model: "anthropic/claude-opus-4-6".to_string(),
            version: "0.1.7".to_string(),
            capabilities: vec!["memory".to_string(), "code".to_string(), "nostr".to_string()],
            operator_npub: Some("npub1testoperator".to_string()),
        };

        let mut content = serde_json::json!({
            "name": profile.name,
            "about": profile.about,
            "snow:model": profile.model,
            "snow:version": profile.version,
            "snow:capabilities": profile.capabilities,
        });
        content["snow:operator"] = serde_json::json!(profile.operator_npub);

        let builder = nostr_sdk::EventBuilder::new(
            nostr_sdk::Kind::Metadata,
            content.to_string(),
        );

        let event = builder.sign_with_keys(&keys).unwrap();

        assert_eq!(event.kind, nostr_sdk::Kind::Metadata);

        let parsed: serde_json::Value = serde_json::from_str(&event.content).unwrap();
        assert_eq!(parsed["name"], "snow-studio");
        assert_eq!(parsed["about"], "Snowclaw instance on studio");
        assert_eq!(parsed["snow:model"], "anthropic/claude-opus-4-6");
        assert_eq!(parsed["snow:version"], "0.1.7");
        assert_eq!(
            parsed["snow:capabilities"],
            serde_json::json!(["memory", "code", "nostr"])
        );
        assert_eq!(parsed["snow:operator"], "npub1testoperator");
    }

    #[test]
    fn agent_profile_serialization_roundtrip() {
        let profile = AgentProfile {
            name: "test-agent".to_string(),
            about: "A test agent".to_string(),
            model: "test/model".to_string(),
            version: "0.1.0".to_string(),
            capabilities: vec!["memory".to_string()],
            operator_npub: None,
        };

        // Serialize the same way publish_agent_profile does
        let content = serde_json::json!({
            "name": profile.name,
            "about": profile.about,
            "snow:model": profile.model,
            "snow:version": profile.version,
            "snow:capabilities": profile.capabilities,
        });
        // operator_npub is None, so it should not be in the JSON
        assert!(content.get("snow:operator").is_none());

        // Now parse back the same way fetch_agent_profile does
        let v = content;
        let recovered = AgentProfile {
            name: v["name"].as_str().unwrap_or_default().to_string(),
            about: v["about"].as_str().unwrap_or_default().to_string(),
            model: v["snow:model"].as_str().unwrap_or_default().to_string(),
            version: v["snow:version"].as_str().unwrap_or_default().to_string(),
            capabilities: v["snow:capabilities"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            operator_npub: v["snow:operator"].as_str().map(String::from),
        };

        assert_eq!(recovered.name, "test-agent");
        assert_eq!(recovered.about, "A test agent");
        assert_eq!(recovered.model, "test/model");
        assert_eq!(recovered.version, "0.1.0");
        assert_eq!(recovered.capabilities, vec!["memory"]);
        assert!(recovered.operator_npub.is_none());
    }

    #[test]
    fn metadata_table_tracks_sync_timestamp() {
        let index = SqliteMemoryIndex::open_in_memory().unwrap();
        init_metadata_table(&index).unwrap();

        // Initially no timestamp
        assert_eq!(get_last_sync_timestamp(&index), None);

        // Set a timestamp
        set_last_sync_timestamp(&index, 1700000000).unwrap();
        assert_eq!(get_last_sync_timestamp(&index), Some(1700000000));

        // Update the timestamp
        set_last_sync_timestamp(&index, 1700001000).unwrap();
        assert_eq!(get_last_sync_timestamp(&index), Some(1700001000));
    }
}

#[cfg(test)]
mod forget_list_tests {
    use super::*;

    fn test_config() -> CollectiveMemoryConfig {
        CollectiveMemoryConfig::default()
    }

    #[tokio::test]
    async fn forget_deletes_by_topic() {
        let mem = CollectiveMemory::new_in_memory(&test_config()).unwrap();

        mem.store("rust/errors", "How to handle errors", MemoryCategory::Core, None)
            .await
            .unwrap();
        assert_eq!(mem.count().await.unwrap(), 1);

        // Forget existing topic
        let deleted = mem.forget("rust/errors").await.unwrap();
        assert!(deleted);
        assert_eq!(mem.count().await.unwrap(), 0);

        // Forget non-existent topic
        let deleted = mem.forget("nonexistent").await.unwrap();
        assert!(!deleted);
    }

    #[tokio::test]
    async fn list_returns_all_memories() {
        let mem = CollectiveMemory::new_in_memory(&test_config()).unwrap();

        mem.store("rust/errors", "Error handling", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("nostr/nip44", "NIP-44 encryption", MemoryCategory::Core, None)
            .await
            .unwrap();

        let all = mem.list(None, None).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn list_filters_by_category() {
        let mem = CollectiveMemory::new_in_memory(&test_config()).unwrap();

        // Store a public memory (Core -> Public tier)
        mem.store("core:timezone", "UTC preferred", MemoryCategory::Core, None)
            .await
            .unwrap();
        // Store a private memory (Conversation -> Private tier)
        mem.store("conv:session1", "Session context", MemoryCategory::Conversation, None)
            .await
            .unwrap();

        // Filter by Core (Public tier)
        let public = mem.list(Some(&MemoryCategory::Core), None).await.unwrap();
        assert_eq!(public.len(), 1);
        assert_eq!(public[0].key, "core:timezone");

        // Filter by Conversation (Private tier)
        let private = mem.list(Some(&MemoryCategory::Conversation), None).await.unwrap();
        assert_eq!(private.len(), 1);
        assert_eq!(private[0].key, "conv:session1");
    }

    // ── Conflict detection tests ───────────────────────────────

    /// Helper: insert a SnowMemory directly into the index for conflict tests.
    fn insert_test_memory(
        mem: &CollectiveMemory,
        id: &str,
        topic: &str,
        summary: &str,
        source: &str,
        version: u32,
    ) {
        let memory = SnowMemory {
            id: id.to_string(),
            tier: MemoryTier::Public,
            topic: topic.to_string(),
            summary: summary.to_string(),
            detail: String::new(),
            context: None,
            source: source.to_string(),
            model: String::new(),
            confidence: 0.8,
            supersedes: None,
            version,
            tags: vec![],
            created_at: now_unix(),
        };
        let idx = mem.index.lock();
        idx.upsert(&memory, None).unwrap();
    }

    #[test]
    fn detect_conflicts_same_topic_different_sources() {
        let mem = CollectiveMemory::new_in_memory(&test_config()).unwrap();

        insert_test_memory(&mem, "a1", "rust/errors", "Use anyhow for errors", "agent_alpha", 1);
        insert_test_memory(&mem, "b1", "rust/errors", "Use thiserror for errors", "agent_beta", 1);

        let conflicts = mem.detect_conflicts("errors").unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].topic, "rust/errors");
        assert_eq!(conflicts[0].entries.len(), 2);

        // Both sources should be represented
        let sources: Vec<&str> = conflicts[0].entries.iter().map(|e| e.source.as_str()).collect();
        assert!(sources.contains(&"agent_alpha"));
        assert!(sources.contains(&"agent_beta"));
    }

    #[test]
    fn detect_conflicts_same_source_not_conflict() {
        let mem = CollectiveMemory::new_in_memory(&test_config()).unwrap();

        // Same source, different versions — supersedes chain, NOT a conflict
        insert_test_memory(&mem, "a1", "rust/errors", "Use anyhow v1", "agent_alpha", 1);
        insert_test_memory(&mem, "a2", "rust/errors", "Use anyhow v2", "agent_alpha", 2);

        let conflicts = mem.detect_conflicts("errors").unwrap();
        assert!(conflicts.is_empty());
    }

    #[test]
    fn detect_conflicts_no_duplicates_returns_empty() {
        let mem = CollectiveMemory::new_in_memory(&test_config()).unwrap();

        insert_test_memory(&mem, "a1", "rust/errors", "Use anyhow for errors", "agent_alpha", 1);
        insert_test_memory(&mem, "b1", "nostr/nip44", "NIP-44 encryption", "agent_beta", 1);

        // Search for errors should only find one topic
        let conflicts = mem.detect_conflicts("errors").unwrap();
        assert!(conflicts.is_empty());
    }
}

#[cfg(test)]
mod promote_tests {
    use super::*;

    fn test_config() -> CollectiveMemoryConfig {
        CollectiveMemoryConfig::default()
    }

    #[tokio::test]
    async fn promote_private_to_public() {
        let mem = CollectiveMemory::new_in_memory(&test_config()).unwrap();

        mem.store_with_tier(
            "pref:language",
            "Rust is preferred",
            MemoryCategory::Core,
            MemoryTier::Private("self".to_string()),
        )
        .await
        .unwrap();

        mem.promote("pref:language", MemoryTier::Public)
            .await
            .unwrap();

        assert_eq!(mem.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn promote_private_to_group() {
        let mem = CollectiveMemory::new_in_memory(&test_config()).unwrap();

        mem.store_with_tier(
            "pref:editor",
            "Uses neovim",
            MemoryCategory::Core,
            MemoryTier::Private("self".to_string()),
        )
        .await
        .unwrap();

        mem.promote("pref:editor", MemoryTier::Group("dev-team".to_string()))
            .await
            .unwrap();

        assert_eq!(mem.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn reject_demotion_public_to_private() {
        let mem = CollectiveMemory::new_in_memory(&test_config()).unwrap();

        mem.store_with_tier(
            "core:timezone",
            "UTC preferred",
            MemoryCategory::Core,
            MemoryTier::Public,
        )
        .await
        .unwrap();

        let result = mem
            .promote("core:timezone", MemoryTier::Private("self".to_string()))
            .await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("tier must increase"));
    }

    #[tokio::test]
    async fn reject_same_tier_promotion() {
        let mem = CollectiveMemory::new_in_memory(&test_config()).unwrap();

        mem.store_with_tier(
            "core:lang",
            "Rust",
            MemoryCategory::Core,
            MemoryTier::Public,
        )
        .await
        .unwrap();

        let result = mem.promote("core:lang", MemoryTier::Public).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn promote_nonexistent_key_errors() {
        let mem = CollectiveMemory::new_in_memory(&test_config()).unwrap();

        let result = mem.promote("nonexistent:key", MemoryTier::Public).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("memory not found"));
    }

    #[test]
    fn tier_rank_ordering() {
        assert!(tier_rank(&MemoryTier::Private("s".into())) < tier_rank(&MemoryTier::Group("g".into())));
        assert!(tier_rank(&MemoryTier::Group("g".into())) < tier_rank(&MemoryTier::Public));
        assert!(tier_rank(&MemoryTier::Private("s".into())) < tier_rank(&MemoryTier::Public));
    }
}
