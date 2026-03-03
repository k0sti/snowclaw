//! Composite Nostr+SQLite memory backend.
//!
//! Architecture:
//! - **Write path:** store in local SQLite (fast) → publish kind 30078 to relay (async)
//! - **Read/recall path:** query local SQLite using hybrid search (vector + FTS5)
//! - **Startup sync:** fetch missing events from relay → upsert into SQLite
//!
//! The relay provides durable, portable persistence. SQLite provides fast local
//! semantic search with embeddings and FTS5. Best of both worlds.

use anyhow::{Context, Result};
use async_trait::async_trait;
use nostr_sdk::prelude::*;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::OnceCell;
use tracing::{debug, info, warn};

use super::embeddings::EmbeddingProvider;
use super::sqlite::SqliteMemory;
use super::traits::{Memory, MemoryCategory, MemoryEntry};

/// Composite Nostr+SQLite memory backend.
///
/// Writes go to both SQLite (local, fast) and relay (durable, portable).
/// Reads always go to SQLite for fast hybrid search.
/// On first use, syncs missing events from relay into SQLite.
pub struct NostrSqliteMemory {
    sqlite: SqliteMemory,
    relay_url: Option<String>,
    local_relay_url: Option<String>,
    nsec: Option<String>,
    relay_client: OnceCell<(Client, PublicKey)>,
    app_tag: String,
    synced: OnceCell<()>,
    encrypted: bool,
}

impl NostrSqliteMemory {
    /// Create a new composite Nostr+SQLite memory backend.
    ///
    /// SQLite DB is created at `{workspace_dir}/nostr_sqlite/memory/brain.db`.
    /// Relay connection and sync are lazy — established on first operation.
    pub fn new(
        relay_url: Option<&str>,
        local_relay_url: Option<&str>,
        nsec: Option<&str>,
        workspace_dir: &Path,
        embedder: Arc<dyn EmbeddingProvider>,
        vector_weight: f32,
        keyword_weight: f32,
        cache_max: usize,
        sqlite_open_timeout_secs: Option<u64>,
        encrypted: bool,
    ) -> Result<Self> {
        let sqlite_workspace = workspace_dir.join("nostr_sqlite");
        let sqlite = SqliteMemory::with_embedder(
            &sqlite_workspace,
            embedder,
            vector_weight,
            keyword_weight,
            cache_max,
            sqlite_open_timeout_secs,
        )?;

        Ok(Self {
            sqlite,
            relay_url: relay_url.map(String::from),
            local_relay_url: local_relay_url.map(String::from),
            nsec: nsec.map(String::from),
            relay_client: OnceCell::new(),
            app_tag: "snowclaw".to_string(),
            synced: OnceCell::new(),
            encrypted,
        })
    }

    /// Create with default noop embedder (keyword-only search).
    pub fn new_default(
        relay_url: Option<&str>,
        local_relay_url: Option<&str>,
        nsec: Option<&str>,
        workspace_dir: &Path,
    ) -> Result<Self> {
        Self::new(
            relay_url,
            local_relay_url,
            nsec,
            workspace_dir,
            Arc::new(super::embeddings::NoopEmbedding),
            0.7,
            0.3,
            10_000,
            None,
            false,
        )
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
                    "Nostr+SQLite memory relay connected (relay: {}, pubkey: {})",
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

    fn has_relay_config(&self) -> bool {
        self.relay_url.is_some() && self.nsec.is_some()
    }

    /// Parse the nsec into Keys for NIP-44 encrypt/decrypt.
    fn keys(&self) -> Option<Keys> {
        self.nsec.as_deref().and_then(|nsec| Keys::parse(nsec).ok())
    }

    /// Sync missing events from relay into local SQLite.
    ///
    /// Fetches all kind 30078 events authored by our pubkey and upserts them.
    /// Called lazily on first memory operation.
    async fn sync_from_relay(&self) -> Result<usize> {
        let (client, public_key) = match self.get_relay().await {
            Some(r) => r,
            None => return Ok(0),
        };

        let filter = Filter::new()
            .author(*public_key)
            .kind(Kind::Custom(30078));

        let events = client
            .fetch_events(filter, Duration::from_secs(15))
            .await
            .context("Failed to fetch events from relay for sync")?;

        let events: Vec<Event> = events.into_iter().collect();
        let total = events.len();
        let mut synced = 0;

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

            let (category, key) = match self.parse_d_tag(&d_tag) {
                Some(parsed) => parsed,
                None => continue,
            };

            let session_id = event
                .tags
                .iter()
                .find(|t| t.as_slice().first().map(|s| s.as_str()) == Some("session"))
                .and_then(|t| t.as_slice().get(1).map(|s| s.to_string()));

            // Decrypt NIP-44 encrypted content if present
            let is_encrypted = event.tags.iter().any(|t| {
                let s = t.as_slice();
                s.first().map(|v| v.as_str()) == Some("encrypted")
                    && s.get(1).map(|v| v.as_str()) == Some("nip44")
            });

            let content = if is_encrypted {
                if let Some(keys) = self.keys() {
                    match nip44::decrypt(keys.secret_key(), &event.pubkey, &event.content) {
                        Ok(plaintext) => plaintext,
                        Err(e) => {
                            warn!("NIP-44 decryption failed for {d_tag}, using raw content: {e}");
                            event.content.clone()
                        }
                    }
                } else {
                    warn!("Encrypted event but no nsec for {d_tag}");
                    event.content.clone()
                }
            } else {
                event.content.clone()
            };

            // Upsert into SQLite — SqliteMemory handles ON CONFLICT(key) DO UPDATE
            if let Err(e) = self
                .sqlite
                .store(&key, &content, category, session_id.as_deref())
                .await
            {
                warn!("Failed to sync event {d_tag} into SQLite: {e}");
                continue;
            }
            synced += 1;
        }

        if synced > 0 {
            info!(
                "Nostr→SQLite sync complete: {synced}/{total} events indexed"
            );
        } else {
            debug!("Nostr→SQLite sync: no new events to index ({total} total on relay)");
        }

        Ok(synced)
    }

    /// Ensure relay sync has happened at least once. Lazy, idempotent.
    async fn ensure_synced(&self) {
        if !self.has_relay_config() {
            return;
        }

        self.synced
            .get_or_init(|| async {
                if let Err(e) = self.sync_from_relay().await {
                    warn!("Initial relay sync failed: {e}");
                }
            })
            .await;
    }

    /// Publish a memory entry to the relay (best-effort, non-blocking to caller).
    async fn publish_to_relay(
        &self,
        key: &str,
        content: &str,
        category: &MemoryCategory,
        session_id: Option<&str>,
    ) {
        let Some((client, public_key)) = self.get_relay().await else {
            return;
        };

        let d_tag = self.d_tag(key, category);

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

        // Encrypt content with NIP-44 if enabled
        let publish_content = if self.encrypted {
            if let Some(keys) = self.keys() {
                match nip44::encrypt(
                    keys.secret_key(),
                    &public_key,
                    content,
                    nip44::Version::V2,
                ) {
                    Ok(ciphertext) => {
                        tags.push(Tag::custom(
                            TagKind::custom("encrypted"),
                            vec!["nip44".to_string()],
                        ));
                        ciphertext
                    }
                    Err(e) => {
                        warn!("NIP-44 encryption failed, publishing plaintext: {e}");
                        content.to_string()
                    }
                }
            } else {
                warn!("Encryption enabled but no nsec available, publishing plaintext");
                content.to_string()
            }
        } else {
            content.to_string()
        };

        let builder = EventBuilder::new(Kind::Custom(30078), &publish_content).tags(tags);

        match client.send_event_builder(builder).await {
            Ok(_) => debug!("Published memory to relay: {} ({})", key, category),
            Err(e) => warn!("Failed to publish memory to relay: {e} (persisted in SQLite)"),
        }
    }
}

#[async_trait]
impl Memory for NostrSqliteMemory {
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
        self.ensure_synced().await;

        // Write to SQLite first (fast, reliable)
        self.sqlite
            .store(key, content, category.clone(), session_id)
            .await?;

        // Publish to relay (best-effort)
        self.publish_to_relay(key, content, &category, session_id)
            .await;

        debug!("Stored memory (nostr+sqlite): {} ({})", key, category);
        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        self.ensure_synced().await;

        // Always query SQLite — it has hybrid search (vector + FTS5)
        self.sqlite.recall(query, limit, session_id).await
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        self.ensure_synced().await;

        // Try SQLite first
        let entry = self.sqlite.get(key).await?;
        if entry.is_some() {
            return Ok(entry);
        }

        // Fall back to relay fetch if SQLite miss
        if !self.has_relay_config() {
            return Ok(None);
        }

        let Some((_, public_key)) = self.get_relay().await else {
            return Ok(None);
        };

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

            match self.fetch_events(filter).await {
                Ok(events) => {
                    if let Some(event) = events.first() {
                        let session_id = event
                            .tags
                            .iter()
                            .find(|t| t.as_slice().first().map(|s| s.as_str()) == Some("session"))
                            .and_then(|t| t.as_slice().get(1).map(|s| s.to_string()));

                        // Cache in SQLite for next time
                        if let Err(e) = self
                            .sqlite
                            .store(key, &event.content, cat.clone(), session_id.as_deref())
                            .await
                        {
                            warn!("Failed to cache relay result in SQLite: {e}");
                        }

                        return self.sqlite.get(key).await;
                    }
                }
                Err(e) => {
                    warn!("Relay get failed for key {key}: {e}");
                    break;
                }
            }
        }

        Ok(None)
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        self.ensure_synced().await;
        self.sqlite.list(category, session_id).await
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        self.ensure_synced().await;

        // Delete from relay if available
        if let Some((client, _)) = self.get_relay().await {
            // Try to find the event to get its ID for deletion
            if let Ok(Some(entry)) = self.sqlite.get(key).await {
                if let Ok(event_id) = EventId::from_hex(&entry.id) {
                    let deletion = EventDeletionRequest::new().id(event_id);
                    let builder = EventBuilder::delete(deletion);
                    match client.send_event_builder(builder).await {
                        Ok(_) => debug!("Deleted memory from relay: {}", key),
                        Err(e) => warn!("Failed to delete from relay: {e}"),
                    }
                }
            }
        }

        // Delete from SQLite
        self.sqlite.forget(key).await
    }

    async fn count(&self) -> Result<usize> {
        self.ensure_synced().await;
        self.sqlite.count().await
    }

    async fn health_check(&self) -> bool {
        let sqlite_ok = self.sqlite.health_check().await;
        if !sqlite_ok {
            return false;
        }

        if let Some((client, _)) = self.get_relay().await {
            let relays = client.relays().await;
            relays
                .values()
                .any(|r| r.status() == RelayStatus::Connected)
        } else {
            true // local-only is healthy if SQLite is healthy
        }
    }
}

impl NostrSqliteMemory {
    /// Fetch events from relay with timeout.
    async fn fetch_events(&self, filter: Filter) -> Result<Vec<Event>> {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d_tag_format() {
        let mem = NostrSqliteMemory::new_default(None, None, None, Path::new("/tmp/test")).unwrap();
        let d_tag = mem.d_tag("user:profile", &MemoryCategory::Core);
        assert_eq!(d_tag, "snowclaw:core:user:profile");
    }

    #[test]
    fn parse_d_tag_roundtrip() {
        let mem = NostrSqliteMemory::new_default(None, None, None, Path::new("/tmp/test")).unwrap();
        let d_tag = "snowclaw:daily:2026-02-18";
        let (cat, key) = mem.parse_d_tag(d_tag).unwrap();
        assert_eq!(cat, MemoryCategory::Daily);
        assert_eq!(key, "2026-02-18");
    }

    #[test]
    fn parse_d_tag_custom_category() {
        let mem = NostrSqliteMemory::new_default(None, None, None, Path::new("/tmp/test")).unwrap();
        let d_tag = "snowclaw:project:my-key";
        let (cat, key) = mem.parse_d_tag(d_tag).unwrap();
        assert_eq!(cat, MemoryCategory::Custom("project".to_string()));
        assert_eq!(key, "my-key");
    }

    #[test]
    fn parse_d_tag_invalid_prefix() {
        let mem = NostrSqliteMemory::new_default(None, None, None, Path::new("/tmp/test")).unwrap();
        assert!(mem.parse_d_tag("other:core:key").is_none());
    }

    #[tokio::test]
    async fn local_only_store_and_recall() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mem = NostrSqliteMemory::new_default(None, None, None, tmp.path()).unwrap();

        mem.store("lang", "Rust is fast", MemoryCategory::Core, None)
            .await
            .unwrap();

        let results = mem.recall("Rust", 10, None).await.unwrap();
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r.key == "lang"));
    }

    #[tokio::test]
    async fn local_only_get_and_forget() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mem = NostrSqliteMemory::new_default(None, None, None, tmp.path()).unwrap();

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
        let mem = NostrSqliteMemory::new_default(None, None, None, tmp.path()).unwrap();

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
    async fn name_is_nostr() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mem = NostrSqliteMemory::new_default(None, None, None, tmp.path()).unwrap();
        assert_eq!(mem.name(), "nostr");
    }

    #[tokio::test]
    async fn health_check_local_only_is_true() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mem = NostrSqliteMemory::new_default(None, None, None, tmp.path()).unwrap();
        assert!(mem.health_check().await);
    }

    #[tokio::test]
    async fn has_relay_config_false_without_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mem = NostrSqliteMemory::new_default(None, None, None, tmp.path()).unwrap();
        assert!(!mem.has_relay_config());
    }

    #[tokio::test]
    async fn upsert_overwrites_in_sqlite() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mem = NostrSqliteMemory::new_default(None, None, None, tmp.path()).unwrap();

        mem.store("pref", "likes Rust", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("pref", "loves Rust", MemoryCategory::Core, None)
            .await
            .unwrap();

        let entry = mem.get("pref").await.unwrap().unwrap();
        assert_eq!(entry.content, "loves Rust");
        assert_eq!(mem.count().await.unwrap(), 1);
    }
}
