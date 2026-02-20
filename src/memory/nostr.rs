//! Nostr-backed memory backend using NIP-78 (kind 30078) app-specific data.
//!
//! Architecture: relay as primary storage, local JSON file as cache/fallback.
//! Writes go to both relay and local cache. Reads try relay first, fall back to cache.

use anyhow::{Context, Result};
use async_trait::async_trait;
use nostr_sdk::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OnceCell, RwLock};
use tracing::{debug, info, warn};

use super::traits::{Memory, MemoryCategory, MemoryEntry};

/// Local JSON cache for offline fallback.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct LocalCache {
    entries: HashMap<String, MemoryEntry>,
}

/// Nostr-backed memory using NIP-78 (kind 30078) app-specific data.
///
/// Each memory entry is a replaceable event keyed by `d` tag.
/// Key format: `snowclaw:<category>:<key>`
///
/// Relay connection is lazy — established on first relay operation.
pub struct NostrMemory {
    relay_url: Option<String>,
    local_relay_url: Option<String>,
    nsec: Option<String>,
    relay_client: OnceCell<(Client, PublicKey)>,
    app_tag: String,
    cache: Arc<RwLock<LocalCache>>,
    cache_path: PathBuf,
}

impl NostrMemory {
    /// Create a Nostr memory backend. Relay connection is lazy.
    pub fn new(
        relay_url: Option<&str>,
        local_relay_url: Option<&str>,
        nsec: Option<&str>,
        workspace_dir: &Path,
    ) -> Self {
        let cache_path = workspace_dir.join("nostr_agent_memory.json");
        let cache = Self::load_cache(&cache_path);

        if !cache.entries.is_empty() {
            debug!("Nostr memory: loaded {} cached entries", cache.entries.len());
        }

        Self {
            relay_url: relay_url.map(String::from),
            local_relay_url: local_relay_url.map(String::from),
            nsec: nsec.map(String::from),
            relay_client: OnceCell::new(),
            app_tag: "snowclaw".to_string(),
            cache: Arc::new(RwLock::new(cache)),
            cache_path,
        }
    }

    /// Create a local-only fallback (no relay). Used when no nsec/relay is configured.
    pub fn local_only(workspace_dir: &Path) -> Self {
        Self::new(None, None, None, workspace_dir)
    }

    fn load_cache(path: &Path) -> LocalCache {
        if path.exists() {
            match std::fs::read_to_string(path) {
                Ok(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
                    warn!("Failed to parse nostr memory cache: {e}; starting fresh");
                    LocalCache::default()
                }),
                Err(e) => {
                    warn!("Failed to read nostr memory cache: {e}; starting fresh");
                    LocalCache::default()
                }
            }
        } else {
            LocalCache::default()
        }
    }

    /// Lazily initialize the relay client on first use.
    async fn get_relay(&self) -> Option<&(Client, PublicKey)> {
        let relay_url = self.relay_url.as_deref()?;
        let nsec = self.nsec.as_deref()?;

        self.relay_client
            .get_or_try_init(|| async {
                let keys = Keys::parse(nsec).context("Invalid nsec for Nostr memory")?;
                let public_key = keys.public_key();
                let client = Client::new(keys);

                client
                    .add_relay(relay_url)
                    .await
                    .context("Failed to add memory relay")?;

                if let Some(local_url) = &self.local_relay_url {
                    if let Err(e) = client.add_relay(local_url.as_str()).await {
                        warn!("Failed to add local relay {local_url}: {e}");
                    }
                }

                client.connect().await;

                info!(
                    "Nostr memory relay connected (relay: {}, pubkey: {})",
                    relay_url,
                    public_key.to_bech32().unwrap_or_default()
                );

                Ok::<_, anyhow::Error>((client, public_key))
            })
            .await
            .ok()
    }

    /// Build the `d` tag value for a memory key.
    fn d_tag(&self, key: &str, category: &MemoryCategory) -> String {
        format!("{}:{}:{}", self.app_tag, category, key)
    }

    /// Parse a `d` tag back into (category, key).
    fn parse_d_tag(&self, d_tag: &str) -> Option<(MemoryCategory, String)> {
        let prefix = format!("{}:", self.app_tag);
        let rest = d_tag.strip_prefix(&prefix)?;
        let (cat_str, key) = rest.split_once(':')?;
        let category = match cat_str {
            "core" => MemoryCategory::Core,
            "daily" => MemoryCategory::Daily,
            "conversation" => MemoryCategory::Conversation,
            other => MemoryCategory::Custom(other.to_string()),
        };
        Some((category, key.to_string()))
    }

    /// Fetch events matching filters with timeout.
    async fn fetch(&self, filter: Filter) -> Result<Vec<Event>> {
        let (client, _) = match self.get_relay().await {
            Some(r) => r,
            None => return Ok(Vec::new()),
        };
        let events = client
            .fetch_events(filter, Duration::from_secs(10))
            .await
            .context("Failed to fetch events from relay")?;
        Ok(events.into_iter().collect())
    }

    /// Convert a Nostr event to a MemoryEntry.
    fn event_to_entry(&self, event: &Event) -> Option<MemoryEntry> {
        let d_tag = event
            .tags
            .iter()
            .find(|t| t.as_slice().first().map(|s| s.as_str()) == Some("d"))
            .and_then(|t| t.as_slice().get(1).map(|s| s.to_string()))?;

        let (category, key) = self.parse_d_tag(&d_tag)?;

        let session_id = event
            .tags
            .iter()
            .find(|t| t.as_slice().first().map(|s| s.as_str()) == Some("session"))
            .and_then(|t| t.as_slice().get(1).map(|s| s.to_string()));

        Some(MemoryEntry {
            id: event.id.to_hex(),
            key,
            content: event.content.clone(),
            category,
            timestamp: chrono::DateTime::from_timestamp(event.created_at.as_secs() as i64, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default(),
            session_id,
            score: None,
        })
    }

    /// Persist local cache to disk.
    async fn flush_cache(&self) -> Result<()> {
        let cache = self.cache.read().await;
        let json = serde_json::to_string_pretty(&*cache)?;
        drop(cache);

        if let Some(parent) = self.cache_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.cache_path, json)?;
        Ok(())
    }

    fn has_relay_config(&self) -> bool {
        self.relay_url.is_some() && self.nsec.is_some()
    }
}

#[async_trait]
impl Memory for NostrMemory {
    fn name(&self) -> &str {
        "nostr"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let entry = MemoryEntry {
            id: format!("nostr-{}", chrono::Utc::now().timestamp_millis()),
            key: key.to_string(),
            content: content.to_string(),
            category: category.clone(),
            timestamp: now,
            session_id: session_id.map(String::from),
            score: None,
        };

        // Write to local cache first (always)
        {
            let mut cache = self.cache.write().await;
            cache.entries.insert(key.to_string(), entry.clone());
        }
        if let Err(e) = self.flush_cache().await {
            warn!("Failed to flush memory cache: {e}");
        }

        // Write to relay if available
        if let Some((client, _)) = self.get_relay().await {
            let d_tag = self.d_tag(key, &category);

            let mut tags = vec![
                Tag::custom(TagKind::custom("d"), vec![d_tag]),
                Tag::custom(TagKind::custom("app"), vec![self.app_tag.clone()]),
                Tag::custom(TagKind::custom("category"), vec![category.to_string()]),
                Tag::custom(TagKind::custom("agent"), vec!["snowclaw".to_string()]),
            ];

            if let Some(sid) = session_id {
                tags.push(Tag::custom(
                    TagKind::custom("session"),
                    vec![sid.to_string()],
                ));
            }

            let builder = EventBuilder::new(Kind::Custom(30078), content).tags(tags);

            match client.send_event_builder(builder).await {
                Ok(_) => debug!("Stored memory to relay: {} ({})", key, category),
                Err(e) => warn!("Failed to publish memory to relay: {e} (cached locally)"),
            }
        }

        debug!("Stored memory: {} ({})", key, category);
        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let query_lower = query.to_lowercase();

        // Try relay first if available
        if self.has_relay_config() {
            if let Some((_, public_key)) = self.get_relay().await {
                let filter = Filter::new()
                    .author(*public_key)
                    .kind(Kind::Custom(30078))
                    .limit(500);

                match self.fetch(filter).await {
                    Ok(events) if !events.is_empty() => {
                        let mut results: Vec<MemoryEntry> = events
                            .iter()
                            .filter_map(|e| self.event_to_entry(e))
                            .filter(|entry| {
                                let matches_query =
                                    entry.content.to_lowercase().contains(&query_lower)
                                        || entry.key.to_lowercase().contains(&query_lower);
                                let matches_session = session_id.map_or(true, |sid| {
                                    entry.session_id.as_deref() == Some(sid)
                                });
                                matches_query && matches_session
                            })
                            .take(limit)
                            .collect();

                        results.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
                        return Ok(results);
                    }
                    Ok(_) => {} // empty, fall through to cache
                    Err(e) => warn!("Relay recall failed, using cache: {e}"),
                }
            }
        }

        // Fallback to local cache
        let cache = self.cache.read().await;
        let mut results: Vec<MemoryEntry> = cache
            .entries
            .values()
            .filter(|e| {
                let matches_query = e.key.to_lowercase().contains(&query_lower)
                    || e.content.to_lowercase().contains(&query_lower);
                let matches_session = session_id.map_or(true, |sid| {
                    e.session_id.as_deref() == Some(sid)
                });
                matches_query && matches_session
            })
            .cloned()
            .collect();

        results.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        results.truncate(limit);
        Ok(results)
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        // Try relay first
        if self.has_relay_config() {
            if let Some((_, public_key)) = self.get_relay().await {
                for cat in &[
                    MemoryCategory::Core,
                    MemoryCategory::Daily,
                    MemoryCategory::Conversation,
                ] {
                    let d_tag = self.d_tag(key, cat);
                    let filter = Filter::new()
                        .author(*public_key)
                        .kind(Kind::Custom(30078))
                        .custom_tag(SingleLetterTag::lowercase(Alphabet::D), d_tag)
                        .limit(1);

                    match self.fetch(filter).await {
                        Ok(events) => {
                            if let Some(event) = events.first() {
                                if let Some(entry) = self.event_to_entry(event) {
                                    return Ok(Some(entry));
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Relay get failed for key {key}: {e}");
                            break; // fall through to cache
                        }
                    }
                }
            }
        }

        // Fallback to local cache
        let cache = self.cache.read().await;
        Ok(cache.entries.get(key).cloned())
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        // Try relay first
        if self.has_relay_config() {
            if let Some((_, public_key)) = self.get_relay().await {
                let filter = Filter::new()
                    .author(*public_key)
                    .kind(Kind::Custom(30078))
                    .limit(1000);

                match self.fetch(filter).await {
                    Ok(events) if !events.is_empty() => {
                        let entries: Vec<MemoryEntry> = events
                            .iter()
                            .filter_map(|e| self.event_to_entry(e))
                            .filter(|entry| {
                                category.map_or(true, |cat| &entry.category == cat)
                                    && session_id.map_or(true, |sid| {
                                        entry.session_id.as_deref() == Some(sid)
                                    })
                            })
                            .collect();
                        return Ok(entries);
                    }
                    Ok(_) => {} // empty, fall through
                    Err(e) => warn!("Relay list failed, using cache: {e}"),
                }
            }
        }

        // Fallback to local cache
        let cache = self.cache.read().await;
        let entries: Vec<MemoryEntry> = cache
            .entries
            .values()
            .filter(|e| {
                category.map_or(true, |cat| &e.category == cat)
                    && session_id.map_or(true, |sid| e.session_id.as_deref() == Some(sid))
            })
            .cloned()
            .collect();
        Ok(entries)
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        let mut found = false;

        // Delete from relay if available
        if let Some((client, _)) = self.get_relay().await {
            if let Ok(Some(entry)) = self.get(key).await {
                if let Ok(event_id) = EventId::from_hex(&entry.id) {
                    let deletion = EventDeletionRequest::new().id(event_id);
                    let builder = EventBuilder::delete(deletion);
                    match client.send_event_builder(builder).await {
                        Ok(_) => {
                            info!("Forgot memory from relay: {}", key);
                            found = true;
                        }
                        Err(e) => warn!("Failed to delete from relay: {e}"),
                    }
                }
            }
        }

        // Remove from local cache
        {
            let mut cache = self.cache.write().await;
            if cache.entries.remove(key).is_some() {
                found = true;
            }
        }
        if found {
            if let Err(e) = self.flush_cache().await {
                warn!("Failed to flush cache after forget: {e}");
            }
        }

        Ok(found)
    }

    async fn count(&self) -> Result<usize> {
        let entries = self.list(None, None).await?;
        Ok(entries.len())
    }

    async fn health_check(&self) -> bool {
        if let Some((client, _)) = self.get_relay().await {
            let relays = client.relays().await;
            relays
                .values()
                .any(|r| r.status() == RelayStatus::Connected)
        } else {
            true // local-only is always healthy
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d_tag_format() {
        let app = "snowclaw";
        let category = MemoryCategory::Core;
        let key = "user:k0:profile";
        let d_tag = format!("{}:{}:{}", app, category, key);
        assert_eq!(d_tag, "snowclaw:core:user:k0:profile");
    }

    #[test]
    fn parse_d_tag_roundtrip() {
        let prefix = "snowclaw:";
        let d_tag = "snowclaw:daily:2026-02-18";
        let rest = d_tag.strip_prefix(prefix).unwrap();
        let (cat_str, key) = rest.split_once(':').unwrap();
        assert_eq!(cat_str, "daily");
        assert_eq!(key, "2026-02-18");
    }

    #[tokio::test]
    async fn local_only_store_and_recall() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mem = NostrMemory::local_only(tmp.path());

        mem.store("lang", "Rust is fast", MemoryCategory::Core, None)
            .await
            .unwrap();

        let results = mem.recall("rust", 10, None).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "lang");
        assert!(results[0].content.contains("Rust"));
    }

    #[tokio::test]
    async fn local_only_get_and_forget() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mem = NostrMemory::local_only(tmp.path());

        mem.store("key1", "value1", MemoryCategory::Core, None)
            .await
            .unwrap();

        let entry = mem.get("key1").await.unwrap();
        assert!(entry.is_some());

        let removed = mem.forget("key1").await.unwrap();
        assert!(removed);

        let entry = mem.get("key1").await.unwrap();
        assert!(entry.is_none());
    }

    #[tokio::test]
    async fn local_only_count_and_list() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mem = NostrMemory::local_only(tmp.path());

        mem.store("a", "aa", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b", "bb", MemoryCategory::Daily, None)
            .await
            .unwrap();

        assert_eq!(mem.count().await.unwrap(), 2);

        let core = mem.list(Some(&MemoryCategory::Core), None).await.unwrap();
        assert_eq!(core.len(), 1);
    }

    #[tokio::test]
    async fn local_only_persistence_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();

        {
            let mem = NostrMemory::local_only(tmp.path());
            mem.store("persist", "data", MemoryCategory::Core, None)
                .await
                .unwrap();
        }

        let mem2 = NostrMemory::local_only(tmp.path());
        let entry = mem2.get("persist").await.unwrap();
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().content, "data");
    }

    #[tokio::test]
    async fn name_is_nostr() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mem = NostrMemory::local_only(tmp.path());
        assert_eq!(mem.name(), "nostr");
    }

    #[tokio::test]
    async fn health_check_local_only_is_true() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mem = NostrMemory::local_only(tmp.path());
        assert!(mem.health_check().await);
    }
}
