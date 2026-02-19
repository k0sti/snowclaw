use anyhow::{Result, Context};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::collections::HashSet;
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio::time::interval;
use tracing::{info, warn, error, debug};
use nostr_sdk::{EventId, PublicKey, Keys};

use crate::config::Config;
use crate::cache::{EventCache, CacheStats};
use crate::profiles::ProfileCache;
use crate::relay::{RelayClient, RelayEvent};
use crate::webhook::WebhookDeliverer;

pub struct BridgeState {
    pub config: Config,
    pub cache: EventCache,
    pub profiles: Arc<ProfileCache>,
    pub relay: Arc<RwLock<RelayClient>>,
    pub webhook: WebhookDeliverer,
    pub start_time: Instant,
}

pub struct Bridge {
    state: Arc<BridgeState>,
    shutdown_tx: broadcast::Sender<()>,
}

impl Bridge {
    pub async fn new(config: Config, keys: Keys) -> Result<Self> {
        let cache = EventCache::new(&config.cache.db_path).await
            .with_context(|| "Failed to initialize event cache")?;

        let profiles = Arc::new(ProfileCache::new());

        let relay = RelayClient::new(&config.relay.url, keys).await
            .with_context(|| "Failed to create relay client")?;

        let webhook = WebhookDeliverer::new(
            config.webhook.url.clone(),
            config.webhook.dm_url.clone(),
            config.webhook.token.clone(),
            config.webhook.preview_length,
        );

        let state = Arc::new(BridgeState {
            config: config.clone(),
            cache,
            profiles,
            relay: Arc::new(RwLock::new(relay)),
            webhook,
            start_time: Instant::now(),
        });

        let (shutdown_tx, _) = broadcast::channel(1);

        Ok(Bridge { state, shutdown_tx })
    }

    pub async fn start(&mut self) -> Result<()> {
        info!("Starting Nostr bridge");

        // Connect to relay
        {
            let mut relay = self.state.relay.write().await;
            relay.connect().await
                .with_context(|| "Failed to connect to relay")?;

            if !self.state.config.groups.subscribe.is_empty() {
                relay.subscribe_groups(&self.state.config.groups.subscribe).await
                    .with_context(|| "Failed to subscribe to groups")?;
            }

            relay.subscribe_dms().await
                .with_context(|| "Failed to subscribe to DMs")?;
        }

        // Test webhook connectivity
        self.state.webhook.test_webhook().await
            .with_context(|| "Webhook connectivity test failed")?;

        // Start event stream via mpsc channel
        let (event_tx, event_rx) = mpsc::channel(1000);
        {
            let relay = self.state.relay.read().await;
            relay.start_event_stream(event_tx);
        }

        // Start event processing task
        let state = self.state.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        tokio::spawn(async move {
            Self::event_processing_loop(state, event_rx, &mut shutdown_rx).await;
        });

        // Start periodic maintenance task
        let state = self.state.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        tokio::spawn(async move {
            Self::maintenance_loop(state, &mut shutdown_rx).await;
        });

        info!("Nostr bridge started successfully");
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<()> {
        info!("Shutting down bridge");
        let _ = self.shutdown_tx.send(());
        let relay = self.state.relay.read().await;
        relay.disconnect().await?;
        info!("Bridge shutdown complete");
        Ok(())
    }

    pub fn state(&self) -> Arc<BridgeState> {
        self.state.clone()
    }

    async fn event_processing_loop(
        state: Arc<BridgeState>,
        mut event_rx: mpsc::Receiver<RelayEvent>,
        shutdown_rx: &mut broadcast::Receiver<()>,
    ) {
        info!("Event processing loop started");
        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    info!("Event processing loop shutting down");
                    break;
                },
                Some(relay_event) = event_rx.recv() => {
                    if let Err(e) = Self::handle_relay_event(&state, relay_event).await {
                        error!("Failed to handle relay event: {}", e);
                    }
                },
                else => {
                    warn!("Event stream ended");
                    break;
                }
            }
        }
    }

    async fn handle_relay_event(state: &BridgeState, relay_event: RelayEvent) -> Result<()> {
        match relay_event {
            RelayEvent::GroupMessage { event, group } => {
                let event_id_hex = event.id.to_hex();
                let author_hex = event.pubkey.to_hex();

                if state.cache.has_by_hex(&event_id_hex).await? {
                    debug!("Event {} already cached, skipping", &event_id_hex[..8]);
                    return Ok(());
                }

                state.cache.store_raw(
                    &event_id_hex,
                    &author_hex,
                    event.created_at.as_u64() as i64,
                    event.kind.as_u16() as i64,
                    &serde_json::to_string(&event.tags)?,
                    &event.content,
                    &event.sig.to_string(),
                    Some(&group),
                ).await?;

                let author_name = state.profiles.get_display_name_hex(&author_hex).await;

                let preview: String = event.content.chars().take(state.config.webhook.preview_length).collect();

                state.webhook.deliver_group_message_raw(
                    &event_id_hex, &group, &author_name, &preview, 
                    event.created_at.as_u64() as i64,
                ).await?;

                info!("#{} {} : {}", group, author_name, &preview[..preview.len().min(60)]);
            }
            RelayEvent::DirectMessage { event } => {
                let event_id_hex = event.id.to_hex();
                let author_hex = event.pubkey.to_hex();

                if state.cache.has_by_hex(&event_id_hex).await? {
                    return Ok(());
                }

                state.cache.store_raw(
                    &event_id_hex,
                    &author_hex,
                    event.created_at.as_u64() as i64,
                    event.kind.as_u16() as i64,
                    &serde_json::to_string(&event.tags)?,
                    &event.content,
                    &event.sig.to_string(),
                    None,
                ).await?;

                let author_name = state.profiles.get_display_name_hex(&author_hex).await;
                let preview: String = event.content.chars().take(state.config.webhook.preview_length).collect();

                state.webhook.deliver_dm_raw(
                    &event_id_hex, &author_name, &preview,
                    event.created_at.as_u64() as i64,
                ).await?;

                info!("DM from {}: {}", author_name, &preview[..preview.len().min(60)]);
            }
            RelayEvent::ProfileUpdate { event } => {
                let author_hex = event.pubkey.to_hex();
                if let Err(e) = state.profiles.store_profile_raw(&author_hex, &event.content).await {
                    warn!("Failed to store profile for {}: {}", &author_hex[..8], e);
                }
            }
        }
        Ok(())
    }

    async fn maintenance_loop(
        state: Arc<BridgeState>,
        shutdown_rx: &mut broadcast::Receiver<()>,
    ) {
        let mut cleanup_interval = interval(Duration::from_secs(3600));
        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => break,
                _ = cleanup_interval.tick() => {
                    if state.config.cache.retention_days > 0 {
                        if let Ok(n) = state.cache.cleanup(state.config.cache.retention_days).await {
                            if n > 0 { info!("Cleaned {} old events", n); }
                        }
                    }
                    let cleaned = state.profiles.cleanup_expired().await;
                    if cleaned > 0 { debug!("Cleaned {} expired profiles", cleaned); }
                }
            }
        }
    }
}

impl BridgeState {
    pub async fn send_group_message(&self, group: &str, content: &str) -> Result<EventId> {
        let relay = self.relay.read().await;
        relay.send_group_message(group, content).await
    }

    pub async fn send_direct_message(&self, recipient: &PublicKey, content: &str) -> Result<EventId> {
        let relay = self.relay.read().await;
        relay.send_dm(recipient, content).await
    }

    pub async fn query_events(
        &self, group: Option<&str>, author: Option<&PublicKey>, since: Option<i64>, limit: Option<i64>,
    ) -> Result<Vec<crate::cache::CachedEvent>> {
        self.cache.query(group, author, since, limit).await
    }

    pub async fn get_event(&self, event_id: &EventId) -> Result<Option<crate::cache::CachedEvent>> {
        self.cache.get(event_id).await
    }

    pub async fn get_display_name(&self, pubkey: &PublicKey) -> String {
        self.profiles.get_display_name(pubkey).await
    }

    pub async fn decrypt_dm_content(&self, _content: &str, _author_pubkey: &str) -> Result<String> {
        // TODO: implement NIP-04 decryption
        Ok("[encrypted]".to_string())
    }

    pub async fn get_stats(&self) -> Result<(CacheStats, Duration, bool, HashSet<String>, PublicKey)> {
        let cache_stats = self.cache.stats().await?;
        let uptime = self.start_time.elapsed();
        let relay = self.relay.read().await;
        let groups = relay.subscribed_groups().clone();
        let pubkey = relay.our_pubkey();
        Ok((cache_stats, uptime, true, groups, pubkey))
    }
}
