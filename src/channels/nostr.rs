// TODO: Refactor this file to use nostr-core instead of inline implementations
// The nostr-core crate now contains extracted versions of:
// - KeyFilter, RespondMode, HistoryMessage, GroupConfig, DynamicConfig
// - NostrMemory, ProfileMetadata, NpubMemory, GroupMemory 
// - RelayClient wrapper, mention detection, context formatting
// - Action protocol parsing, task status handling

use anyhow::{Context, Result};
use async_trait::async_trait;
use lru::LruCache;
use nostr_sdk::prelude::*;
use std::collections::{HashMap, VecDeque};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info, warn};

use super::nostr_memory::NostrMemory;
use super::traits::{Channel, ChannelMessage, SendMessage};
use crate::security::key_filter::{self, KeyFilter};

/// Default capacity for the LRU event cache.
const EVENT_CACHE_CAPACITY: usize = 1000;

/// Agent attribution tag added to all published events (1.20).
fn agent_tag() -> Tag {
    Tag::custom(TagKind::custom("agent"), vec!["snowclaw".to_string()])
}

/// Respond mode for group messages
#[derive(Debug, Clone, PartialEq)]
pub enum RespondMode {
    /// Reply to all messages
    All,
    /// Only reply when mentioned by name/npub or replied to
    Mention,
    /// Respond only to owner's messages
    Owner,
    /// Listen only, never auto-reply
    None,
}

impl RespondMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "all" => Self::All,
            "owner" => Self::Owner,
            "none" | "silent" | "listen" => Self::None,
            _ => Self::Mention,
        }
    }
}

/// A message stored in the per-group history ring buffer.
#[derive(Debug, Clone)]
pub struct HistoryMessage {
    pub sender: String,
    pub npub: String,
    pub content: String,
    pub timestamp: u64,
    pub event_id: String,
    pub is_owner: bool,
}

/// Per-group dynamic configuration loaded from NIP-78 kind 30078 events.
#[derive(Debug, Clone, Default)]
pub struct GroupConfig {
    pub respond_mode: Option<RespondMode>,
    pub context_history: Option<usize>,
}

/// Dynamic configuration loaded from NIP-78 events, keyed by scope.
#[derive(Debug, Clone, Default)]
pub struct DynamicConfig {
    pub global: Option<GroupConfig>,
    pub groups: HashMap<String, GroupConfig>,
    pub npubs: HashMap<String, GroupConfig>,
}

/// Configuration for the Nostr channel
#[derive(Debug, Clone)]
pub struct NostrChannelConfig {
    /// Relay URLs to connect to
    pub relays: Vec<String>,
    /// Secret key for signing events
    pub keys: Keys,
    /// NIP-29 group IDs to subscribe to
    pub groups: Vec<String>,
    /// Whether to listen for DMs (NIP-17)
    pub listen_dms: bool,
    /// Allowed pubkeys (empty = allow all)
    pub allowed_pubkeys: Vec<PublicKey>,
    /// Default respond mode for groups
    pub respond_mode: RespondMode,
    /// Per-group respond mode overrides
    pub group_respond_mode: HashMap<String, RespondMode>,
    /// Names to match for mention detection (lowercased)
    pub mention_names: Vec<String>,
    /// Owner pubkey (for owner mode + dynamic config)
    pub owner: Option<PublicKey>,
    /// Number of recent messages to include as context (default: 20)
    pub context_history: usize,
    /// Directory for persisting nostr memory (defaults to ~/.snowclaw)
    pub persist_dir: std::path::PathBuf,
}

/// Profile cache entry
#[derive(Debug, Clone)]
struct CachedProfile {
    name: String,
    #[allow(dead_code)]
    fetched_at: u64,
}

/// Nostr channel implementation
///
/// Supports:
/// - NIP-29 relay-based groups (kind 9, 11, 12)
/// - NIP-17 gift-wrapped DMs (kind 1059)
/// - NIP-42 relay AUTH (automatic via nostr-sdk)
/// - Profile resolution (pubkey → display name)
/// - LRU event cache for raw event retrieval
/// - Per-group message ring buffer for conversation context
/// - Dynamic config via NIP-78 kind 30078 events from owner
pub struct NostrChannel {
    config: NostrChannelConfig,
    client: Client,
    profile_cache: Arc<RwLock<HashMap<PublicKey, CachedProfile>>>,
    event_cache: Arc<Mutex<LruCache<String, Event>>>,
    group_history: Arc<RwLock<HashMap<String, VecDeque<HistoryMessage>>>>,
    dynamic_config: Arc<RwLock<DynamicConfig>>,
    key_filter: KeyFilter,
    memory: NostrMemory,
    /// NIP-AE: whether bidirectional owner verification is confirmed (kind 14199)
    owner_verified: Arc<AtomicBool>,
}

impl NostrChannel {
    /// Create a new Nostr channel and connect to relays
    pub async fn new(config: NostrChannelConfig) -> Result<Self> {
        let client = Client::new(config.keys.clone());

        // Add relays
        for relay_url in &config.relays {
            client
                .add_relay(relay_url.as_str())
                .await
                .with_context(|| format!("Failed to add relay: {}", relay_url))?;
        }

        // Connect to all relays
        client.connect().await;
        info!(
            "Nostr channel connected to {} relay(s)",
            config.relays.len()
        );

        // Initialize per-npub/group memory
        let memory = NostrMemory::new(&config.persist_dir);

        // Seed key filter with known pubkeys
        let key_filter = KeyFilter::new();
        // Our own pubkey
        key_filter.add_known_pubkey(&config.keys.public_key().to_hex());
        // Owner pubkey
        if let Some(ref g) = config.owner {
            key_filter.add_known_pubkey(&g.to_hex());
        }
        // Allowed pubkeys
        key_filter.add_known_pubkeys(config.allowed_pubkeys.iter().map(|pk| pk.to_hex()));

        let channel = Self {
            config,
            client,
            profile_cache: Arc::new(RwLock::new(HashMap::new())),
            event_cache: Arc::new(Mutex::new(LruCache::new(
                NonZeroUsize::new(EVENT_CACHE_CAPACITY).unwrap(),
            ))),
            group_history: Arc::new(RwLock::new(HashMap::new())),
            dynamic_config: Arc::new(RwLock::new(DynamicConfig::default())),
            key_filter,
            memory,
            owner_verified: Arc::new(AtomicBool::new(false)),
        };

        // Load existing dynamic config from owner's NIP-78 events
        channel.load_dynamic_config().await;

        // Backfill ring buffer with recent group messages from relay
        channel.backfill_history().await;

        // Publish agent state (kind 31121) — announce we're online
        channel.publish_agent_state().await;

        // Publish profile with NIP-AE bot tag
        channel.publish_profile_with_bot_tag().await;

        Ok(channel)
    }

    /// Fetch recent group messages from relay to populate the ring buffer on startup.
    async fn backfill_history(&self) {
        if self.config.groups.is_empty() {
            return;
        }

        let filter = Filter::new()
            .kinds(vec![Kind::Custom(9), Kind::Custom(11), Kind::Custom(12)])
            .limit(self.config.context_history);

        let fetch_result = self.client.fetch_events(
            filter,
            Duration::from_secs(5),
        ).await;

        match fetch_result {
            Ok(events) => {
                let mut count = 0usize;
                // Sort by timestamp ascending (oldest first) so ring buffer order is correct
                let mut sorted: Vec<Event> = events.into_iter().collect();
                sorted.sort_by_key(|e| e.created_at);

                for event in sorted {
                    // Skip our own events
                    if self.is_own_event(&event) {
                        continue;
                    }

                    let group = match Self::extract_group(&event) {
                        Some(g) if self.config.groups.contains(&g) => g,
                        _ => continue,
                    };

                    let sender_name = self.resolve_name(&event.pubkey).await;
                    let sender_npub = event.pubkey.to_bech32().unwrap_or_else(|_| event.pubkey.to_hex());
                    let is_owner = self.is_from_owner(&event);

                    self.push_history(
                        &group,
                        HistoryMessage {
                            sender: sender_name,
                            npub: sender_npub,
                            content: event.content.clone(),
                            timestamp: event.created_at.as_secs(),
                            event_id: event.id.to_hex(),
                            is_owner,
                        },
                    ).await;
                    count += 1;
                }
                if count > 0 {
                    info!("Backfilled {} messages from relay into ring buffer", count);
                }
            }
            Err(e) => warn!("Failed to backfill history: {e}"),
        }
    }

    /// Load existing NIP-78 config events from owner on startup
    async fn load_dynamic_config(&self) {
        let owner = match &self.config.owner {
            Some(g) => *g,
            None => return,
        };

        let filter = Filter::new()
            .kind(Kind::Custom(30078))
            .author(owner);

        match tokio::time::timeout(
            Duration::from_secs(5),
            self.client.fetch_events(filter, Duration::from_secs(5)),
        )
        .await
        {
            Ok(Ok(events)) => {
                let mut config = self.dynamic_config.write().await;
                for event in events {
                    if let Some(parsed) = Self::parse_config_event(&event) {
                        Self::apply_config_entry(&mut config, parsed);
                    }
                }
                info!("Loaded dynamic config from owner");
            }
            Ok(Err(e)) => warn!("Failed to fetch dynamic config: {e}"),
            Err(_) => warn!("Timeout fetching dynamic config"),
        }
    }

    /// Parse a NIP-78 kind 30078 config event into a (scope, GroupConfig) pair.
    fn parse_config_event(event: &Event) -> Option<(String, GroupConfig)> {
        let d_tag = event.tags.iter().find_map(|tag| {
            let s = tag.as_slice();
            if s.first().map(|v| v.as_str()) == Some("d") {
                s.get(1).map(|v| v.to_string())
            } else {
                None
            }
        })?;

        if !d_tag.starts_with("snowclaw:config:") {
            return None;
        }

        let mut gc = GroupConfig::default();

        for tag in event.tags.iter() {
            let s = tag.as_slice();
            match s.first().map(|v| v.as_str()) {
                Some("respond_mode") => {
                    if let Some(val) = s.get(1) {
                        gc.respond_mode = Some(RespondMode::from_str(val));
                    }
                }
                Some("context_history") => {
                    if let Some(val) = s.get(1) {
                        if let Ok(n) = val.parse::<usize>() {
                            gc.context_history = Some(n);
                        }
                    }
                }
                _ => {}
            }
        }

        Some((d_tag, gc))
    }

    /// Apply a parsed config entry to the dynamic config.
    fn apply_config_entry(config: &mut DynamicConfig, (d_tag, gc): (String, GroupConfig)) {
        let scope = d_tag.trim_start_matches("snowclaw:config:");
        if scope == "global" {
            config.global = Some(gc);
        } else if let Some(group) = scope.strip_prefix("group:") {
            config.groups.insert(group.to_string(), gc);
        } else if let Some(npub) = scope.strip_prefix("npub:") {
            config.npubs.insert(npub.to_string(), gc);
        }
    }

    /// Retrieve a cached raw Nostr event by its hex ID.
    pub async fn get_raw_event(&self, event_id: &str) -> Option<Event> {
        self.event_cache.lock().await.get(event_id).cloned()
    }

    /// Retrieve a cached raw Nostr event as a JSON string.
    pub async fn get_raw_event_json(&self, event_id: &str) -> Option<String> {
        self.event_cache
            .lock()
            .await
            .get(event_id)
            .and_then(|e| serde_json::to_string(e).ok())
    }

    /// Store an event in the LRU cache.
    async fn cache_event(&self, event: &Event) {
        self.event_cache
            .lock()
            .await
            .put(event.id.to_hex(), event.clone());
    }

    /// Add a message to the group's ring buffer.
    async fn push_history(&self, group: &str, msg: HistoryMessage) {
        let max = self.effective_context_history(group).await;
        let mut history = self.group_history.write().await;
        let buf = history.entry(group.to_string()).or_insert_with(VecDeque::new);
        buf.push_back(msg);
        while buf.len() > max {
            buf.pop_front();
        }
    }

    /// Get the effective context_history for a group (dynamic > file > default).
    async fn effective_context_history(&self, group: &str) -> usize {
        let dc = self.dynamic_config.read().await;
        // Check group-specific dynamic config
        if let Some(gc) = dc.groups.get(group) {
            if let Some(n) = gc.context_history {
                return n;
            }
        }
        // Check global dynamic config
        if let Some(ref gc) = dc.global {
            if let Some(n) = gc.context_history {
                return n;
            }
        }
        // Fall back to file config
        self.config.context_history
    }

    /// Format the ring buffer history as conversation context to prepend to a message.
    /// Excludes the current event (by event_id) to avoid duplication.
    async fn format_history_context(&self, group: &str, exclude_event_id: &str) -> String {
        let max = self.effective_context_history(group).await;
        let history = self.group_history.read().await;
        let buf = match history.get(group) {
            Some(b) if !b.is_empty() => b,
            _ => return String::new(),
        };

        let mut ctx = String::from("[Recent conversation context]\n");
        let start = if buf.len() > max { buf.len() - max } else { 0 };
        for msg in buf.iter().skip(start) {
            // Skip the current message to avoid duplication
            if msg.event_id == exclude_event_id {
                continue;
            }
            let short_npub = Self::truncate_npub(&msg.npub);
            let role = if msg.is_owner { " role=owner" } else { "" };
            ctx.push_str(&format!("<{} npub={}{}>  {}\n", msg.sender, short_npub, role, msg.content));
        }
        if ctx == "[Recent conversation context]\n" {
            return String::new(); // no history besides current message
        }
        ctx.push('\n');
        ctx
    }

    /// Build a owner identity line for LLM context.
    pub async fn owner_context_line(&self) -> String {
        match &self.config.owner {
            Some(owner) => {
                let name = self.resolve_name(owner).await;
                let npub = owner.to_bech32().unwrap_or_else(|_| owner.to_hex());
                format!("[Owner: {} ({})]\n", name, npub)
            }
            None => String::new(),
        }
    }

    /// Truncate an npub to first 20 chars for context efficiency.
    fn truncate_npub(npub: &str) -> &str {
        &npub[..20.min(npub.len())]
    }

    /// Build compact header for group messages.
    fn compact_group_header(group: &str, sender: &str, npub: &str, kind: u16, event_id: &str, is_owner: bool) -> String {
        let short_id = &event_id[..8.min(event_id.len())];
        let short_npub = Self::truncate_npub(npub);
        if is_owner {
            format!("[nostr:group=#{group} from={sender} npub={short_npub} role=owner kind={kind} id={short_id}]")
        } else {
            format!("[nostr:group=#{group} from={sender} npub={short_npub} kind={kind} id={short_id}]")
        }
    }

    /// Build compact header for task status events.
    fn compact_task_content(event_id: &str, task_ref: &str, status: &str, detail: &str) -> String {
        let short_id = &event_id[..8.min(event_id.len())];
        let short_task = &task_ref[..8.min(task_ref.len())];
        if detail.is_empty() {
            format!("[nostr:event={short_id}] Task {short_task} → {status}")
        } else {
            format!("[nostr:event={short_id}] Task {short_task} → {status}: {detail}")
        }
    }

    /// Build metadata HashMap for a Nostr event.
    fn build_metadata(event: &Event, group: Option<&str>, is_owner: bool) -> Option<HashMap<String, String>> {
        let mut meta = HashMap::new();
        meta.insert("nostr_event_id".into(), event.id.to_hex());
        meta.insert("nostr_pubkey".into(), event.pubkey.to_hex());
        meta.insert("nostr_kind".into(), event.kind.as_u16().to_string());
        if let Some(g) = group {
            meta.insert("nostr_group".into(), g.to_string());
        }
        if is_owner {
            meta.insert("nostr_is_owner".into(), "true".into());
        }
        Some(meta)
    }

    /// Resolve a pubkey to a display name, with caching.
    /// Stores full profile metadata in nostr_memory on first fetch.
    async fn resolve_name(&self, pubkey: &PublicKey) -> String {
        // Check cache first
        {
            let cache = self.profile_cache.read().await;
            if let Some(cached) = cache.get(pubkey) {
                return cached.name.clone();
            }
        }

        // Fetch kind 0 metadata from relay
        let filter = Filter::new().author(*pubkey).kind(Kind::Metadata).limit(1);
        let now_ts = chrono::Utc::now().timestamp() as u64;

        let (name, profile_meta) = match tokio::time::timeout(
            Duration::from_secs(5),
            self.client.fetch_events(filter, Duration::from_secs(5)),
        )
        .await
        {
            Ok(Ok(events)) => {
                match events.into_iter().next().and_then(|event| {
                    serde_json::from_str::<serde_json::Value>(&event.content).ok()
                }) {
                    Some(meta) => {
                        let display_name = meta.get("display_name").and_then(|v| v.as_str()).map(|s| s.to_string());
                        let name_field = meta.get("name").and_then(|v| v.as_str()).map(|s| s.to_string());
                        let about = meta.get("about").and_then(|v| v.as_str()).map(|s| s.to_string());
                        let picture = meta.get("picture").and_then(|v| v.as_str()).map(|s| s.to_string());
                        let nip05 = meta.get("nip05").and_then(|v| v.as_str()).map(|s| s.to_string());
                        let lud16 = meta.get("lud16").and_then(|v| v.as_str()).map(|s| s.to_string());

                        let resolved = display_name.clone()
                            .or(name_field.clone())
                            .unwrap_or_else(|| {
                                let npub = pubkey.to_bech32().unwrap_or_default();
                                format!("{}...{}", &npub[..10], &npub[npub.len() - 4..])
                            });

                        let profile = super::nostr_memory::ProfileMetadata {
                            name: name_field,
                            display_name,
                            about: about.clone(),
                            picture,
                            nip05,
                            lud16,
                            fetched_at: now_ts,
                        };

                        // Log profile lookup
                        let npub_short = &pubkey.to_bech32().unwrap_or_default()[..20.min(pubkey.to_bech32().unwrap_or_default().len())];
                        let about_short = about.as_deref().unwrap_or("").chars().take(50).collect::<String>();
                        info!("Profile: {} = {} (about: {})", npub_short, resolved, about_short);

                        (resolved, Some(profile))
                    }
                    None => {
                        let npub = pubkey.to_bech32().unwrap_or_default();
                        (format!("{}...{}", &npub[..10], &npub[npub.len() - 4..]), None)
                    }
                }
            }
            _ => {
                let npub = pubkey.to_bech32().unwrap_or_default();
                (format!("{}...{}", &npub[..10], &npub[npub.len() - 4..]), None)
            }
        };

        // Cache it
        {
            let mut cache = self.profile_cache.write().await;
            cache.insert(
                *pubkey,
                CachedProfile {
                    name: name.clone(),
                    fetched_at: now_ts,
                },
            );
        }

        // Store full profile in nostr_memory
        if let Some(profile) = profile_meta {
            self.memory.update_profile(&pubkey.to_hex(), profile).await;
        }

        name
    }

    /// Build subscription filters for groups and DMs
    fn build_filters(&self) -> Vec<Filter> {
        let mut filters = Vec::new();

        // NIP-29 group messages (kind 9 = chat, 11 = thread, 12 = thread reply)
        if !self.config.groups.is_empty() {
            let group_filter = Filter::new()
                .kinds(vec![Kind::Custom(9), Kind::Custom(11), Kind::Custom(12)])
                .since(Timestamp::now());
            filters.push(group_filter);
        }

        // NIP-17 DMs (kind 1059 gift-wrapped)
        if self.config.listen_dms {
            let dm_filter = Filter::new()
                .kind(Kind::GiftWrap)
                .pubkey(self.config.keys.public_key())
                .since(Timestamp::now());
            filters.push(dm_filter);
        }

        // Task status events (kind 1630-1637) for groups we're in
        let task_status_filter = Filter::new()
            .kinds(vec![
                Kind::Custom(1630),
                Kind::Custom(1631),
                Kind::Custom(1632),
                Kind::Custom(1633),
                Kind::Custom(1634),
                Kind::Custom(1635),
                Kind::Custom(1636),
                Kind::Custom(1637),
            ])
            .since(Timestamp::now());
        filters.push(task_status_filter);

        // NIP-78 dynamic config events from owner (kind 30078)
        // NIP-AE owner claim events (kind 14199)
        if let Some(owner) = &self.config.owner {
            let config_filter = Filter::new()
                .kind(Kind::Custom(30078))
                .author(*owner)
                .since(Timestamp::now());
            filters.push(config_filter);

            let owner_claims_filter = Filter::new()
                .kind(Kind::Custom(14199))
                .author(*owner);
            filters.push(owner_claims_filter);
        }

        // Action protocol: kind 1121 (action requests targeting this agent)
        let action_filter = Filter::new()
            .kind(Kind::Custom(1121))
            .pubkey(self.config.keys.public_key())
            .since(Timestamp::now());
        filters.push(action_filter);

        // Agent state: kind 31121 (other agents' status/state updates)
        let agent_state_filter = Filter::new()
            .kind(Kind::Custom(31121))
            .since(Timestamp::now());
        filters.push(agent_state_filter);

        filters
    }

    /// Extract group ID from event tags
    fn extract_group(event: &Event) -> Option<String> {
        event
            .tags
            .iter()
            .find(|tag| tag.as_slice().first().map(|s| s.as_str()) == Some("h"))
            .and_then(|tag| tag.as_slice().get(1).map(|s| s.to_string()))
    }

    /// Check if event is from our own pubkey
    fn is_own_event(&self, event: &Event) -> bool {
        event.pubkey == self.config.keys.public_key()
    }

    /// Check if pubkey is allowed
    fn is_allowed(&self, pubkey: &PublicKey) -> bool {
        self.config.allowed_pubkeys.is_empty() || self.config.allowed_pubkeys.contains(pubkey)
    }

    /// Check if event is from the owner
    fn is_from_owner(&self, event: &Event) -> bool {
        match &self.config.owner {
            Some(owner) => event.pubkey == *owner,
            None => false,
        }
    }

    /// Get the effective respond mode for a group (dynamic > file > default)
    async fn respond_mode_for_group(&self, group: &str) -> RespondMode {
        // Check dynamic config first
        let dc = self.dynamic_config.read().await;
        if let Some(gc) = dc.groups.get(group) {
            if let Some(ref mode) = gc.respond_mode {
                return mode.clone();
            }
        }
        if let Some(ref gc) = dc.global {
            if let Some(ref mode) = gc.respond_mode {
                return mode.clone();
            }
        }
        drop(dc);

        // Then file config
        if let Some(mode) = self.config.group_respond_mode.get(group) {
            return mode.clone();
        }

        // Then default
        self.config.respond_mode.clone()
    }

    /// Check if an event mentions us (by name, npub, or is a reply to our event)
    fn is_mentioned(&self, event: &Event) -> bool {
        let our_pubkey = self.config.keys.public_key();

        // Check p-tags for our pubkey (explicit mention or reply)
        for tag in event.tags.iter() {
            let slice = tag.as_slice();
            if slice.first().map(|s| s.as_str()) == Some("p") {
                if let Some(hex) = slice.get(1) {
                    if hex == &our_pubkey.to_hex() {
                        return true;
                    }
                }
            }
        }

        // Check content for name mentions (case-insensitive)
        let content_lower = event.content.to_lowercase();
        for name in &self.config.mention_names {
            if content_lower.contains(name) {
                return true;
            }
        }

        // Check for npub/hex mention in content
        let our_npub = our_pubkey.to_bech32().unwrap_or_default();
        let our_hex = our_pubkey.to_hex();
        if content_lower.contains(&our_npub) || content_lower.contains(&our_hex) {
            return true;
        }

        false
    }

    /// Check if a non-owner pubkey is allowed to perform an action
    fn is_action_allowed(&self, _action: &str, pubkey: &PublicKey) -> bool {
        // For now: allowed pubkeys can do anything that's not owner-only
        // TODO: implement per-action permission checks from config
        self.is_allowed(pubkey)
    }

    /// Extract action params from event tags
    fn extract_action_params(event: &Event) -> Vec<(String, String)> {
        event.tags.iter().filter_map(|tag| {
            let s = tag.as_slice();
            if s.first().map(|v| v.as_str()) == Some("param") {
                match (s.get(1), s.get(2)) {
                    (Some(k), Some(v)) => Some((k.to_string(), v.to_string())),
                    (Some(k), None) => Some((k.to_string(), String::new())),
                    _ => None,
                }
            } else {
                None
            }
        }).collect()
    }

    /// Publish a kind 9 group message
    pub async fn send_group_message(&self, group: &str, content: &str) -> Result<EventId> {
        let tags = vec![
            Tag::custom(TagKind::custom("h"), vec![group.to_string()]),
            agent_tag(),
        ];

        let builder = EventBuilder::new(Kind::Custom(9), content).tags(tags);

        let output = self
            .client
            .send_event_builder(builder)
            .await
            .context("Failed to send group message")?;

        let event_id = output.val;
        info!("Sent group message to #{}: {}", group, event_id);
        Ok(event_id)
    }

    /// Publish a NIP-17 gift-wrapped DM
    pub async fn send_dm(&self, recipient: &PublicKey, content: &str) -> Result<()> {
        let extra_tags: Vec<Tag> = vec![agent_tag()];
        self.client
            .send_private_msg(*recipient, content, extra_tags)
            .await
            .context("Failed to send DM")?;

        info!("Sent DM to {}", recipient.to_bech32().unwrap_or_default());
        Ok(())
    }

    /// Publish agent state (kind 31121) — replaceable event announcing online status.
    async fn publish_agent_state(&self) {
        let uptime_start = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let content = serde_json::json!({
            "groups": self.config.groups,
            "model": "configured",
            "uptime_start": uptime_start,
        });

        let tags = vec![
            Tag::custom(TagKind::custom("d"), vec!["snowclaw:status".to_string()]),
            Tag::custom(TagKind::custom("status"), vec!["online".to_string()]),
            Tag::custom(TagKind::custom("version"), vec!["0.1.0".to_string()]),
            agent_tag(),
        ];

        let builder = EventBuilder::new(Kind::Custom(31121), content.to_string()).tags(tags);
        match self.client.send_event_builder(builder).await {
            Ok(output) => info!("Published agent state (online): {}", output.val),
            Err(e) => warn!("Failed to publish agent state: {e}"),
        }
    }

    /// Publish kind:0 profile with NIP-AE bot tag and owner p-tag.
    /// Fetches existing metadata from relay to preserve name/about/picture, then republishes with tags.
    async fn publish_profile_with_bot_tag(&self) {
        // Fetch existing kind:0 content from relay
        let filter = Filter::new()
            .author(self.config.keys.public_key())
            .kind(Kind::Metadata)
            .limit(1);

        let existing_content = match tokio::time::timeout(
            Duration::from_secs(5),
            self.client.fetch_events(filter, Duration::from_secs(5)),
        )
        .await
        {
            Ok(Ok(events)) => events
                .into_iter()
                .next()
                .map(|e| e.content.clone())
                .unwrap_or_else(|| "{}".to_string()),
            _ => "{}".to_string(),
        };

        // Build tags: bot tag + optional owner p-tag
        let mut tags: Vec<Tag> = vec![
            Tag::custom(TagKind::Custom("bot".into()), Vec::<String>::new()),
            agent_tag(),
        ];
        if let Some(owner) = &self.config.owner {
            tags.push(Tag::public_key(*owner));
        }

        let builder = EventBuilder::new(Kind::Metadata, &existing_content).tags(tags);
        match self.client.send_event_builder(builder).await {
            Ok(output) => info!("Published profile with NIP-AE bot tag: {}", output.val),
            Err(e) => warn!("Failed to publish profile with bot tag: {e}"),
        }
    }

    /// Publish an action response (kind 1121) referencing the original request.
    async fn publish_action_response(
        &self,
        request_event: &Event,
        action: &str,
        status: &str,
        content: &str,
    ) -> Result<()> {
        let tags = vec![
            Tag::custom(TagKind::custom("p"), vec![request_event.pubkey.to_hex()]),
            Tag::custom(
                TagKind::custom("e"),
                vec![request_event.id.to_hex(), String::new(), "reply".to_string()],
            ),
            Tag::custom(
                TagKind::custom("action"),
                vec![format!("{}.result", action)],
            ),
            Tag::custom(TagKind::custom("status"), vec![status.to_string()]),
            agent_tag(),
        ];

        let builder = EventBuilder::new(Kind::Custom(1121), content).tags(tags);
        let output = self
            .client
            .send_event_builder(builder)
            .await
            .context("Failed to publish action response")?;

        info!("Published action response {}.result status={}: {}", action, status, output.val);
        Ok(())
    }

    /// Dispatch an action request to the appropriate handler.
    async fn dispatch_action(
        &self,
        action: &str,
        params: &[(String, String)],
        group: Option<&str>,
        event: &Event,
    ) -> Result<()> {
        match action {
            "control.stop" => {
                let mut dc = self.dynamic_config.write().await;
                if let Some(g) = group {
                    // Group-specific stop
                    warn!("🛑 Action control.stop for #{}", g);
                    let gc = dc.groups.entry(g.to_string()).or_insert_with(GroupConfig::default);
                    gc.respond_mode = Some(RespondMode::None);
                } else {
                    // Global HALT
                    warn!("🛑 Action control.stop (global) — all groups silenced");
                    for g in &self.config.groups {
                        let gc = dc.groups.entry(g.clone()).or_insert_with(GroupConfig::default);
                        gc.respond_mode = Some(RespondMode::None);
                    }
                    let global = dc.global.get_or_insert_with(GroupConfig::default);
                    global.respond_mode = Some(RespondMode::None);
                }
                drop(dc);
                self.publish_action_response(event, action, "ok", "").await
            }

            "control.resume" => {
                let mode_str = params
                    .iter()
                    .find(|(k, _)| k == "mode")
                    .map(|(_, v)| v.as_str())
                    .unwrap_or("mention");
                let new_mode = RespondMode::from_str(mode_str);

                let mut dc = self.dynamic_config.write().await;
                if let Some(g) = group {
                    warn!("▶️ Action control.resume #{} to {:?}", g, new_mode);
                    let gc = dc.groups.entry(g.to_string()).or_insert_with(GroupConfig::default);
                    gc.respond_mode = Some(new_mode.clone());
                } else {
                    warn!("▶️ Action control.resume (global) to {:?}", new_mode);
                    for g in &self.config.groups {
                        let gc = dc.groups.entry(g.clone()).or_insert_with(GroupConfig::default);
                        gc.respond_mode = Some(new_mode.clone());
                    }
                    let global = dc.global.get_or_insert_with(GroupConfig::default);
                    global.respond_mode = Some(new_mode);
                }
                drop(dc);
                self.publish_action_response(event, action, "ok", "").await
            }

            "control.ping" => {
                let uptime_start = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let content = serde_json::json!({
                    "uptime_start": uptime_start,
                    "groups": self.config.groups,
                    "model": "configured",
                });
                self.publish_action_response(event, action, "ok", &content.to_string()).await
            }

            "config.set" => {
                let respond_mode = params.iter().find(|(k, _)| k == "respond_mode").map(|(_, v)| v.as_str());
                let context_history = params
                    .iter()
                    .find(|(k, _)| k == "context_history")
                    .and_then(|(_, v)| v.parse::<usize>().ok());

                let mut dc = self.dynamic_config.write().await;
                if let Some(g) = group {
                    let gc = dc.groups.entry(g.to_string()).or_insert_with(GroupConfig::default);
                    if let Some(mode) = respond_mode {
                        gc.respond_mode = Some(RespondMode::from_str(mode));
                    }
                    if let Some(n) = context_history {
                        gc.context_history = Some(n);
                    }
                    info!("Config updated for #{}: mode={:?} history={:?}", g, gc.respond_mode, gc.context_history);
                } else {
                    let gc = dc.global.get_or_insert_with(GroupConfig::default);
                    if let Some(mode) = respond_mode {
                        gc.respond_mode = Some(RespondMode::from_str(mode));
                    }
                    if let Some(n) = context_history {
                        gc.context_history = Some(n);
                    }
                    info!("Global config updated: mode={:?} history={:?}", gc.respond_mode, gc.context_history);
                }
                drop(dc);

                let content = serde_json::json!({
                    "respond_mode": respond_mode,
                    "context_history": context_history,
                    "applied_to": group.unwrap_or("global"),
                });
                self.publish_action_response(event, action, "ok", &content.to_string()).await
            }

            "config.get" => {
                let dc = self.dynamic_config.read().await;
                let (mode, history) = if let Some(g) = group {
                    let gc = dc.groups.get(g);
                    (
                        gc.and_then(|c| c.respond_mode.as_ref()).map(|m| format!("{:?}", m)),
                        gc.and_then(|c| c.context_history),
                    )
                } else {
                    let gc = dc.global.as_ref();
                    (
                        gc.and_then(|c| c.respond_mode.as_ref()).map(|m| format!("{:?}", m)),
                        gc.and_then(|c| c.context_history),
                    )
                };
                drop(dc);

                let content = serde_json::json!({
                    "scope": group.unwrap_or("global"),
                    "respond_mode": mode,
                    "context_history": history,
                    "file_respond_mode": format!("{:?}", self.config.respond_mode),
                    "file_context_history": self.config.context_history,
                });
                self.publish_action_response(event, action, "ok", &content.to_string()).await
            }

            _ => {
                warn!("Unknown action: {}", action);
                let content = serde_json::json!({"error": format!("unknown action: {}", action)});
                self.publish_action_response(event, action, "error", &content.to_string()).await
            }
        }
    }

    /// Publish a NIP-78 kind 30078 config event (used by CLI)
    pub async fn publish_config_event(
        &self,
        d_tag: &str,
        respond_mode: Option<&str>,
        context_history: Option<usize>,
    ) -> Result<EventId> {
        let mut tags = vec![
            Tag::custom(TagKind::custom("d"), vec![d_tag.to_string()]),
            agent_tag(),
        ];

        if let Some(mode) = respond_mode {
            tags.push(Tag::custom(
                TagKind::custom("respond_mode"),
                vec![mode.to_string()],
            ));
        }
        if let Some(n) = context_history {
            tags.push(Tag::custom(
                TagKind::custom("context_history"),
                vec![n.to_string()],
            ));
        }

        let builder = EventBuilder::new(Kind::Custom(30078), "").tags(tags);
        let output = self
            .client
            .send_event_builder(builder)
            .await
            .context("Failed to publish config event")?;

        info!("Published config event {}: {}", d_tag, output.val);
        Ok(output.val)
    }
}

#[async_trait]
impl Channel for NostrChannel {
    fn name(&self) -> &str {
        "nostr"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        // Determine if recipient is a group or a pubkey
        if message.recipient.starts_with('#') {
            // Group message: #group-name
            let group = message.recipient.trim_start_matches('#');
            let event_id = self.send_group_message(group, &message.content).await?;

            // Add our own reply to the ring buffer so context history includes both sides
            let our_name = self.resolve_name(&self.config.keys.public_key()).await;
            let our_npub = self.config.keys.public_key().to_bech32().unwrap_or_default();
            self.push_history(
                group,
                HistoryMessage {
                    sender: our_name,
                    npub: our_npub,
                    content: message.content.clone(),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    event_id: event_id.to_hex(),
                    is_owner: false,
                },
            )
            .await;
        } else {
            // DM: npub or hex pubkey
            let pubkey = if message.recipient.starts_with("npub") {
                PublicKey::from_bech32(&message.recipient)
                    .context("Invalid npub for DM recipient")?
            } else {
                PublicKey::from_hex(&message.recipient)
                    .context("Invalid hex pubkey for DM recipient")?
            };
            self.send_dm(&pubkey, &message.content).await?;
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        let filters = self.build_filters();

        info!(
            "Subscribing to {} filter(s) on {} relay(s)",
            filters.len(),
            self.config.relays.len()
        );

        // Subscribe each filter separately
        for filter in filters {
            self.client
                .subscribe(filter, None)
                .await
                .context("Failed to subscribe")?;
        }

        // Handle events
        let mut notifications = self.client.notifications();

        loop {
            match notifications.recv().await {
                Ok(notification) => {
                    if let RelayPoolNotification::Event { event, .. } = notification {
                        // Skip own events
                        if self.is_own_event(&event) {
                            continue;
                        }

                        // Check allowed pubkeys
                        if !self.is_allowed(&event.pubkey) {
                            debug!("Ignoring event from non-allowed pubkey: {}", event.pubkey);
                            continue;
                        }

                        // Cache every processed event
                        self.cache_event(&event).await;

                        let kind = event.kind.as_u16();

                        match kind {
                            // NIP-AE owner claim (kind 14199) — verify bidirectional ownership
                            14199 => {
                                if self.is_from_owner(&event) {
                                    let our_pk = self.config.keys.public_key();
                                    let claimed = event.tags.iter().any(|tag| {
                                        tag.as_slice().first().map(|s| s.as_str()) == Some("p")
                                            && tag.as_slice().get(1).map(|s| s.as_str()) == Some(our_pk.to_hex().as_str())
                                    });
                                    if claimed {
                                        self.owner_verified.store(true, Ordering::Relaxed);
                                        info!("NIP-AE: Bidirectional owner verification confirmed via kind 14199");
                                    }
                                }
                            }

                            // NIP-78 dynamic config events from owner
                            30078 => {
                                if self.is_from_owner(&event) {
                                    if let Some(parsed) = Self::parse_config_event(&event) {
                                        let mut dc = self.dynamic_config.write().await;
                                        Self::apply_config_entry(&mut dc, parsed);
                                        info!("Updated dynamic config from owner event {}", event.id.to_hex());
                                    }
                                }
                            }

                            // NIP-29 group messages
                            9 | 11 | 12 => {
                                let group = Self::extract_group(&event)
                                    .unwrap_or_else(|| "unknown".to_string());

                                // Filter by configured groups
                                if !self.config.groups.is_empty()
                                    && !self.config.groups.contains(&group)
                                {
                                    continue;
                                }

                                let sender_name = self.resolve_name(&event.pubkey).await;
                                let sender_npub = event.pubkey.to_bech32().unwrap_or_else(|_| event.pubkey.to_hex());
                                let event_id_hex = event.id.to_hex();
                                let is_owner = self.is_from_owner(&event);

                                // Register sender pubkey as known (safe hex)
                                self.key_filter.add_known_pubkey(&event.pubkey.to_hex());

                                // Sanitize message content before it enters any LLM context
                                let sanitize_ctx = format!("group #{} from {}", group, sender_npub);
                                let (sanitized_content, flags) = self.key_filter.sanitize(&event.content, &sanitize_ctx);
                                if !flags.is_empty() {
                                    key_filter::log_flags(&flags);
                                    // Alert owner via DM if nsec was detected
                                    if flags.iter().any(|f| f.kind == key_filter::SecurityFlagKind::NsecDetected) {
                                        if let Some(owner) = &self.config.owner {
                                            let alert = format!(
                                                "⚠️ Secret key (nsec) detected in message from {} in #{} — redacted before LLM processing",
                                                sender_npub, group
                                            );
                                            if let Err(e) = self.send_dm(owner, &alert).await {
                                                warn!("Failed to alert owner about nsec detection: {e}");
                                            }
                                        }
                                    }
                                }

                                // Update per-npub and per-group memory
                                let sender_hex = event.pubkey.to_hex();
                                let is_new_contact = self.memory.ensure_npub(
                                    &sender_hex,
                                    &sender_name,
                                    event.created_at.as_secs(),
                                    Some(&group),
                                ).await;
                                if is_new_contact {
                                    let short_npub = &sender_npub[..20.min(sender_npub.len())];
                                    info!("New contact: {} ({}) in #{}", sender_name, short_npub, group);
                                }
                                self.memory.ensure_group(&group, event.created_at.as_secs()).await;
                                self.memory.record_group_member(&group, &sender_hex).await;

                                // Owner killswitch and soft controls
                                if is_owner {
                                    let cmd = event.content.trim().to_lowercase();

                                    // HALT = nuclear killswitch — all groups go silent
                                    if cmd == "halt" {
                                        warn!("🛑 HALT from owner — all processing stopped");
                                        let mut dc = self.dynamic_config.write().await;
                                        // Set all configured groups to none
                                        for g in &self.config.groups {
                                            let gc = dc.groups.entry(g.clone()).or_insert_with(GroupConfig::default);
                                            gc.respond_mode = Some(RespondMode::None);
                                        }
                                        // Also set global
                                        let global = dc.global.get_or_insert_with(GroupConfig::default);
                                        global.respond_mode = Some(RespondMode::None);
                                        continue;
                                    }

                                    // Soft stop: group-specific
                                    if cmd == "stop" {
                                        warn!("🛑 Stop from owner in #{}", group);
                                        let mut dc = self.dynamic_config.write().await;
                                        let gc = dc.groups.entry(group.clone()).or_insert_with(GroupConfig::default);
                                        gc.respond_mode = Some(RespondMode::None);
                                        continue;
                                    }

                                    // Resume: group-specific or global
                                    if cmd == "resume" || cmd.starts_with("resume ") {
                                        let mode_str = cmd.strip_prefix("resume").unwrap_or("mention").trim();
                                        let mode_str = if mode_str.is_empty() { "mention" } else { mode_str };
                                        let new_mode = RespondMode::from_str(mode_str);
                                        warn!("▶️ Owner resumed #{} to {:?}", group, new_mode);
                                        let mut dc = self.dynamic_config.write().await;
                                        let gc = dc.groups.entry(group.clone()).or_insert_with(GroupConfig::default);
                                        gc.respond_mode = Some(new_mode.clone());
                                        // If HALT was active, also clear global
                                        if let Some(ref mut global) = dc.global {
                                            if global.respond_mode == Some(RespondMode::None) {
                                                global.respond_mode = Some(new_mode);
                                            }
                                        }
                                        continue;
                                    }
                                }

                                // Always cache message in ring buffer BEFORE respond mode check
                                self.push_history(
                                    &group,
                                    HistoryMessage {
                                        sender: sender_name.clone(),
                                        npub: sender_npub.clone(),
                                        content: sanitized_content.clone(),
                                        timestamp: event.created_at.as_secs(),
                                        event_id: event_id_hex.clone(),
                                        is_owner,
                                    },
                                )
                                .await;

                                // Check respond mode for this group
                                let mode = self.respond_mode_for_group(&group).await;
                                match mode {
                                    RespondMode::None => {
                                        debug!("Skipping group message (respond_mode=none): #{}", group);
                                        continue;
                                    }
                                    RespondMode::Owner => {
                                        if !is_owner {
                                            debug!("Skipping group message (not from owner): #{}", group);
                                            continue;
                                        }
                                    }
                                    RespondMode::Mention => {
                                        if !self.is_mentioned(&event) {
                                            debug!("Skipping group message (not mentioned): #{}", group);
                                            continue;
                                        }
                                    }
                                    RespondMode::All => {} // process everything
                                }

                                // Compact header format
                                let header = Self::compact_group_header(
                                    &group,
                                    &sender_name,
                                    &sender_npub,
                                    kind,
                                    &event_id_hex,
                                    is_owner,
                                );

                                // Prepend owner identity + memory + conversation context
                                let owner_line = self.owner_context_line().await;
                                let memory_context = self.memory.build_context(&sender_hex, &group).await;
                                let history_context = self.format_history_context(&group, &event_id_hex).await;

                                // Mode-specific guidance
                                let mode_guidance = match mode {
                                    RespondMode::All => "[You are listening to all messages in this group. You do NOT need to respond to every message. Only respond when you can add value — answer a question, provide useful info, contribute to the discussion, or when something is clearly directed at you. Stay silent on casual chatter. Quality over quantity. To stay silent, reply with exactly NO_REPLY and nothing else.]\n",
                                    _ => "",
                                };

                                let content =
                                    format!("{}{}{}{}{}\n{}", owner_line, mode_guidance, memory_context, history_context, header, sanitized_content);

                                let msg = ChannelMessage {
                                    id: event_id_hex.clone(),
                                    sender: sender_name,
                                    reply_target: format!("#{}", group),
                                    content,
                                    channel: format!("nostr:#{}", group),
                                    timestamp: event.created_at.as_secs(),
                                    thread_ts: None,
                                    
                                };

                                if tx.send(msg).await.is_err() {
                                    warn!("Channel receiver dropped, stopping listener");
                                    break;
                                }

                                // Flush memory to disk if dirty (cheap no-op if clean)
                                if let Err(e) = self.memory.flush().await {
                                    warn!("Failed to flush nostr memory: {e}");
                                }
                            }

                            // Task status events (1630-1637)
                            1630..=1637 => {
                                let sender_name = self.resolve_name(&event.pubkey).await;
                                let event_id_hex = event.id.to_hex();
                                let status_name = match kind {
                                    1630 => "Queued",
                                    1631 => "Done",
                                    1632 => "Cancelled",
                                    1633 => "Draft",
                                    1634 => "Executing",
                                    1635 => "Blocked",
                                    1636 => "Review",
                                    1637 => "Failed",
                                    _ => "Unknown",
                                };

                                // Extract task reference from e tag
                                let task_ref = event
                                    .tags
                                    .iter()
                                    .find(|tag| {
                                        tag.as_slice().first().map(|s| s.as_str()) == Some("e")
                                    })
                                    .and_then(|tag| tag.as_slice().get(1).map(|s| s.to_string()))
                                    .unwrap_or_else(|| "unknown".to_string());

                                let content = Self::compact_task_content(
                                    &event_id_hex,
                                    &task_ref,
                                    status_name,
                                    &event.content,
                                );

                                let msg = ChannelMessage {
                                    id: event_id_hex.clone(),
                                    sender: sender_name,
                                    reply_target: "tasks".to_string(),
                                    content,
                                    channel: "nostr:tasks".to_string(),
                                    timestamp: event.created_at.as_secs(),
                                    thread_ts: None,
                                    
                                };

                                if tx.send(msg).await.is_err() {
                                    break;
                                }
                            }

                            // Action protocol: kind 1121 (action requests)
                            1121 => {
                                // Verify it's targeting us (p tag)
                                let targets_us = event.tags.iter().any(|tag| {
                                    let s = tag.as_slice();
                                    s.first().map(|v| v.as_str()) == Some("p")
                                        && s.get(1).map(|v| v.as_str())
                                            == Some(&self.config.keys.public_key().to_hex())
                                });
                                if !targets_us {
                                    continue;
                                }

                                let action = event.tags.iter().find_map(|tag| {
                                    let s = tag.as_slice();
                                    if s.first().map(|v| v.as_str()) == Some("action") {
                                        s.get(1).map(|v| v.to_string())
                                    } else {
                                        None
                                    }
                                });

                                let sender_name = self.resolve_name(&event.pubkey).await;
                                let is_owner = self.is_from_owner(&event);

                                if let Some(action) = action {
                                    info!("📩 Action request from {} (owner={}): {}", sender_name, is_owner, action);

                                    // Check permissions: control.* and config.set are owner-only
                                    let owner_only = action.starts_with("control.stop")
                                        || action.starts_with("control.resume")
                                        || action == "config.set";
                                    let allowed = if owner_only {
                                        is_owner
                                    } else {
                                        is_owner || self.is_action_allowed(&action, &event.pubkey)
                                    };

                                    if !allowed {
                                        warn!("⛔ Denied action {} from {}", action, sender_name);
                                        if let Err(e) = self.publish_action_response(&event, &action, "denied", "").await {
                                            warn!("Failed to publish denied response: {e}");
                                        }
                                        continue;
                                    }

                                    // Dispatch to action handlers
                                    let params = Self::extract_action_params(&event);
                                    let group = Self::extract_group(&event);
                                    if let Err(e) = self.dispatch_action(
                                        &action,
                                        &params,
                                        group.as_deref(),
                                        &event,
                                    ).await {
                                        warn!("Action {} failed: {e}", action);
                                        if let Err(e2) = self.publish_action_response(
                                            &event, &action, "error", &e.to_string(),
                                        ).await {
                                            warn!("Failed to publish error response: {e2}");
                                        }
                                    }
                                }
                            }

                            // Agent state: kind 31121 (other agents' status)
                            31121 => {
                                // Don't process our own state events
                                if self.is_own_event(&event) {
                                    continue;
                                }

                                let sender_name = self.resolve_name(&event.pubkey).await;
                                let d_tag = event.tags.iter().find_map(|tag| {
                                    let s = tag.as_slice();
                                    if s.first().map(|v| v.as_str()) == Some("d") {
                                        s.get(1).map(|v| v.to_string())
                                    } else {
                                        None
                                    }
                                });

                                let status = event.tags.iter().find_map(|tag| {
                                    let s = tag.as_slice();
                                    if s.first().map(|v| v.as_str()) == Some("status") {
                                        s.get(1).map(|v| v.to_string())
                                    } else {
                                        None
                                    }
                                });

                                info!(
                                    "🤖 Agent state from {}: d={} status={} content={}",
                                    sender_name,
                                    d_tag.as_deref().unwrap_or("?"),
                                    status.as_deref().unwrap_or("?"),
                                    &event.content.chars().take(80).collect::<String>()
                                );

                                // Cache in memory for agent awareness
                                self.memory.record_agent_state(
                                    &event.pubkey.to_hex(),
                                    &sender_name,
                                    d_tag.as_deref().unwrap_or("unknown"),
                                    status.as_deref().unwrap_or("unknown"),
                                    &event.content,
                                    event.created_at.as_secs(),
                                ).await;
                            }

                            _ => {
                                debug!("Ignoring event kind {}", kind);
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Notification error: {}", e);
                    // nostr-sdk handles reconnection internally
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }

        Ok(())
    }

    async fn health_check(&self) -> bool {
        // Check if we have at least one connected relay
        let relays = self.client.relays().await;
        relays
            .values()
            .any(|r| r.status() == RelayStatus::Connected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nostr_channel_config_is_constructible() {
        let keys = Keys::generate();
        let config = NostrChannelConfig {
            relays: vec!["wss://relay.example.com".to_string()],
            keys: keys.clone(),
            groups: vec!["test-group".to_string()],
            listen_dms: true,
            allowed_pubkeys: vec![],
            respond_mode: RespondMode::Mention,
            group_respond_mode: HashMap::new(),
            mention_names: vec!["snowclaw".to_string()],
            owner: None,
            context_history: 20,
            persist_dir: std::path::PathBuf::from("/tmp"),
        };

        assert_eq!(config.relays.len(), 1);
        assert_eq!(config.groups.len(), 1);
        assert!(config.listen_dms);
        assert_eq!(config.keys.public_key(), keys.public_key());
    }

    #[test]
    fn respond_mode_from_str_owner() {
        assert_eq!(RespondMode::from_str("owner"), RespondMode::Owner);
        assert_eq!(RespondMode::from_str("Owner"), RespondMode::Owner);
        assert_eq!(RespondMode::from_str("OWNER"), RespondMode::Owner);
    }

    #[test]
    fn extract_group_from_tags() {
        let keys = Keys::generate();
        let tags = vec![Tag::custom(
            TagKind::custom("h"),
            vec!["test-group".to_string()],
        )];
        let event = EventBuilder::new(Kind::Custom(9), "hello")
            .tags(tags)
            .sign_with_keys(&keys)
            .unwrap();

        let group = NostrChannel::extract_group(&event);
        assert_eq!(group, Some("test-group".to_string()));
    }

    #[test]
    fn extract_group_missing_tag() {
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(9), "hello")
            .sign_with_keys(&keys)
            .unwrap();

        let group = NostrChannel::extract_group(&event);
        assert_eq!(group, None);
    }

    #[test]
    fn compact_group_header_format() {
        let header =
            NostrChannel::compact_group_header("techteam", "k0sh", "npub1abcdef1234567890abcdef", 9, "abcdef1234567890", false);
        assert_eq!(header, "[nostr:group=#techteam from=k0sh npub=npub1abcdef123456789 kind=9 id=abcdef12]");
    }

    #[test]
    fn compact_group_header_owner() {
        let header =
            NostrChannel::compact_group_header("techteam", "k0sh", "npub1abcdef1234567890abcdef", 9, "abcdef1234567890", true);
        assert_eq!(header, "[nostr:group=#techteam from=k0sh npub=npub1abcdef123456789 role=owner kind=9 id=abcdef12]");
    }

    #[test]
    fn compact_task_content_with_detail() {
        let content = NostrChannel::compact_task_content(
            "abcdef1234567890",
            "abc1234567890000",
            "Executing",
            "working on it",
        );
        assert_eq!(
            content,
            "[nostr:event=abcdef12] Task abc12345 → Executing: working on it"
        );
    }

    #[test]
    fn compact_task_content_without_detail() {
        let content = NostrChannel::compact_task_content(
            "abcdef1234567890",
            "abc1234567890000",
            "Done",
            "",
        );
        assert_eq!(content, "[nostr:event=abcdef12] Task abc12345 → Done");
    }

    #[test]
    fn build_metadata_includes_fields() {
        let keys = Keys::generate();
        let tags = vec![Tag::custom(
            TagKind::custom("h"),
            vec!["mygroup".to_string()],
        )];
        let event = EventBuilder::new(Kind::Custom(9), "test")
            .tags(tags)
            .sign_with_keys(&keys)
            .unwrap();

        let meta = NostrChannel::build_metadata(&event, Some("mygroup"), true).unwrap();
        assert_eq!(meta.get("nostr_event_id").unwrap(), &event.id.to_hex());
        assert_eq!(meta.get("nostr_pubkey").unwrap(), &event.pubkey.to_hex());
        assert_eq!(meta.get("nostr_kind").unwrap(), "9");
        assert_eq!(meta.get("nostr_group").unwrap(), "mygroup");
        assert_eq!(meta.get("nostr_is_owner").unwrap(), "true");
    }

    #[test]
    fn build_metadata_without_group() {
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(1631), "done")
            .sign_with_keys(&keys)
            .unwrap();

        let meta = NostrChannel::build_metadata(&event, None, false).unwrap();
        assert!(!meta.contains_key("nostr_group"));
        assert!(!meta.contains_key("nostr_is_owner"));
        assert_eq!(meta.get("nostr_kind").unwrap(), "1631");
    }

    #[test]
    fn parse_config_event_valid() {
        let keys = Keys::generate();
        let tags = vec![
            Tag::custom(TagKind::custom("d"), vec!["snowclaw:config:group:techteam".to_string()]),
            Tag::custom(TagKind::custom("respond_mode"), vec!["all".to_string()]),
            Tag::custom(TagKind::custom("context_history"), vec!["30".to_string()]),
        ];
        let event = EventBuilder::new(Kind::Custom(30078), "")
            .tags(tags)
            .sign_with_keys(&keys)
            .unwrap();

        let (d_tag, gc) = NostrChannel::parse_config_event(&event).unwrap();
        assert_eq!(d_tag, "snowclaw:config:group:techteam");
        assert_eq!(gc.respond_mode, Some(RespondMode::All));
        assert_eq!(gc.context_history, Some(30));
    }

    #[test]
    fn parse_config_event_global() {
        let keys = Keys::generate();
        let tags = vec![
            Tag::custom(TagKind::custom("d"), vec!["snowclaw:config:global".to_string()]),
            Tag::custom(TagKind::custom("respond_mode"), vec!["owner".to_string()]),
        ];
        let event = EventBuilder::new(Kind::Custom(30078), "")
            .tags(tags)
            .sign_with_keys(&keys)
            .unwrap();

        let (d_tag, gc) = NostrChannel::parse_config_event(&event).unwrap();
        assert_eq!(d_tag, "snowclaw:config:global");
        assert_eq!(gc.respond_mode, Some(RespondMode::Owner));
        assert_eq!(gc.context_history, None);
    }

    #[test]
    fn apply_config_entry_scopes() {
        let mut dc = DynamicConfig::default();
        
        NostrChannel::apply_config_entry(&mut dc, ("snowclaw:config:global".into(), GroupConfig {
            respond_mode: Some(RespondMode::Owner),
            context_history: Some(10),
        }));
        assert!(dc.global.is_some());
        
        NostrChannel::apply_config_entry(&mut dc, ("snowclaw:config:group:test".into(), GroupConfig {
            respond_mode: Some(RespondMode::All),
            context_history: None,
        }));
        assert!(dc.groups.contains_key("test"));

        NostrChannel::apply_config_entry(&mut dc, ("snowclaw:config:npub:abc123".into(), GroupConfig {
            respond_mode: Some(RespondMode::Mention),
            context_history: Some(5),
        }));
        assert!(dc.npubs.contains_key("abc123"));
    }

    #[tokio::test]
    async fn event_cache_store_and_retrieve() {
        let cache = Arc::new(Mutex::new(LruCache::<String, Event>::new(
            NonZeroUsize::new(10).unwrap(),
        )));

        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(9), "cached msg")
            .sign_with_keys(&keys)
            .unwrap();

        let id = event.id.to_hex();
        cache.lock().await.put(id.clone(), event.clone());

        let retrieved = cache.lock().await.get(&id).cloned();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().content, "cached msg");
    }

    #[tokio::test]
    async fn event_cache_eviction() {
        let cap = 1000;
        let cache = Arc::new(Mutex::new(LruCache::<String, Event>::new(
            NonZeroUsize::new(cap).unwrap(),
        )));

        let keys = Keys::generate();
        let mut first_id = String::new();

        for i in 0..=cap {
            let event = EventBuilder::new(Kind::Custom(9), format!("msg {i}"))
                .sign_with_keys(&keys)
                .unwrap();
            let id = event.id.to_hex();
            if i == 0 {
                first_id = id.clone();
            }
            cache.lock().await.put(id, event);
        }

        // First event should have been evicted (1001 entries, capacity 1000)
        assert!(cache.lock().await.get(&first_id).is_none());
        // Cache should be at capacity
        assert_eq!(cache.lock().await.len(), cap);
    }
}
