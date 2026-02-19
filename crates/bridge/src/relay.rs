use nostr_sdk::{
    Client, Event, EventId, Filter, Keys, Kind, PublicKey, RelayUrl, Tag, TagKind,
    EventBuilder, Alphabet, SingleLetterTag,
    RelayPoolNotification,
};
use anyhow::{Result, Context};
use std::collections::HashSet;
use std::str::FromStr;
use tokio::sync::mpsc;
use tracing::{info, debug, warn, error, trace};

pub struct RelayClient {
    client: Client,
    relay_url: String,
    our_pubkey: PublicKey,
    subscribed_groups: HashSet<String>,
}

#[derive(Debug, Clone)]
pub enum RelayEvent {
    GroupMessage { event: Event, group: String },
    DirectMessage { event: Event },
    ProfileUpdate { event: Event },
}

impl RelayClient {
    pub async fn new(relay_url: &str, keys: Keys) -> Result<Self> {
        let our_pubkey = keys.public_key();
        let client = Client::new(keys);

        Ok(Self {
            client,
            relay_url: relay_url.to_string(),
            our_pubkey,
            subscribed_groups: HashSet::new(),
        })
    }

    /// Get a notifications receiver — call BEFORE subscribe to catch backfill
    pub fn notifications(&self) -> tokio::sync::broadcast::Receiver<RelayPoolNotification> {
        self.client.notifications()
    }

    pub async fn connect(&mut self) -> Result<()> {
        let url = RelayUrl::from_str(&self.relay_url)
            .with_context(|| format!("Invalid relay URL: {}", self.relay_url))?;
        self.client.add_relay(url).await
            .with_context(|| "Failed to add relay")?;
        
        info!("Connecting to {}...", self.relay_url);
        self.client.connect().await;
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        info!("Connected");
        Ok(())
    }

    pub async fn subscribe_groups(&mut self, groups: &[String]) -> Result<()> {
        if groups.is_empty() { return Ok(()); }
        info!("Subscribing to groups: {:?}", groups);

        // Only fetch recent events (last hour) to avoid replaying entire history
        let since = nostr_sdk::Timestamp::now() - 3600u64;
        let filter = Filter::new()
            .kind(Kind::Custom(9))
            .custom_tags(SingleLetterTag::lowercase(Alphabet::H), groups.iter().map(|s| s.as_str()))
            .since(since);

        self.client.subscribe(filter, None).await
            .with_context(|| "Failed to subscribe to groups")?;

        for g in groups {
            self.subscribed_groups.insert(g.clone());
        }
        info!("Subscribed to {} groups", groups.len());
        Ok(())
    }

    pub async fn subscribe_dms(&self) -> Result<()> {
        info!("Subscribing to DMs");
        let since = nostr_sdk::Timestamp::now() - 3600u64;
        let filter = Filter::new()
            .kind(Kind::Custom(4))
            .pubkey(self.our_pubkey)
            .since(since);

        self.client.subscribe(filter, None).await
            .with_context(|| "Failed to subscribe to DMs")?;
        info!("Subscribed to DMs");
        Ok(())
    }

    pub async fn subscribe_profiles(&self, pubkeys: &[PublicKey]) -> Result<()> {
        if pubkeys.is_empty() { return Ok(()); }
        let filter = Filter::new()
            .kind(Kind::Metadata)
            .authors(pubkeys.to_vec());
        self.client.subscribe(filter, None).await
            .with_context(|| "Failed to subscribe to profiles")?;
        Ok(())
    }

    pub fn start_event_stream(&self, tx: mpsc::Sender<RelayEvent>, mut notifications: tokio::sync::broadcast::Receiver<RelayPoolNotification>) {
        let our_pubkey = self.our_pubkey;

        tokio::spawn(async move {
            info!("Event stream listening with pre-created receiver...");

            loop {
                match notifications.recv().await {
                    Ok(notification) => {
                        match notification {
                            RelayPoolNotification::Event { event, .. } => {
                                if event.pubkey == our_pubkey {
                                    continue;
                                }

                                let kind_num = event.kind.as_u16();
                                let relay_event = match kind_num {
                                    9 => {
                                        let group = event.tags.iter()
                                            .find(|t| t.kind() == TagKind::h())
                                            .and_then(|t| t.content())
                                            .map(|s| s.to_string())
                                            .unwrap_or_else(|| "unknown".to_string());
                                        
                                        info!("Event received: group msg #{} from {}", group, &event.pubkey.to_hex()[..8]);
                                        Some(RelayEvent::GroupMessage { 
                                            event: event.as_ref().clone(), group 
                                        })
                                    }
                                    4 => {
                                        info!("Event received: DM from {}", &event.pubkey.to_hex()[..8]);
                                        Some(RelayEvent::DirectMessage { 
                                            event: event.as_ref().clone() 
                                        })
                                    }
                                    0 => {
                                        debug!("Event received: profile from {}", &event.pubkey.to_hex()[..8]);
                                        Some(RelayEvent::ProfileUpdate { 
                                            event: event.as_ref().clone() 
                                        })
                                    }
                                    _ => {
                                        debug!("Event received: kind {} from {}", kind_num, &event.pubkey.to_hex()[..8]);
                                        None
                                    }
                                };

                                if let Some(re) = relay_event {
                                    if tx.send(re).await.is_err() {
                                        warn!("Event receiver dropped, exiting stream");
                                        return;
                                    }
                                }
                            }
                            RelayPoolNotification::Message { message, .. } => {
                                // Log relay protocol messages at debug
                                trace!("Relay protocol msg: {:?}", message);
                            }
                            _ => {}
                        }
                    }
                    Err(e) => {
                        warn!("Notification recv error (lagged?): {}", e);
                        // tokio broadcast::Receiver can return Lagged error
                        // if we fall behind — just continue
                        continue;
                    }
                }
            }
        });
    }

    pub async fn send_group_message(&self, group: &str, content: &str) -> Result<EventId> {
        let builder = EventBuilder::new(Kind::Custom(9), content)
            .tag(Tag::custom(TagKind::h(), vec![group.to_string()]));

        let output = self.client.send_event_builder(builder).await
            .map_err(|e| anyhow::anyhow!("Failed to send group message: {}", e))?;
        
        info!("Sent to #{}: {}", group, output.val);
        Ok(output.val)
    }

    pub async fn send_dm(&self, recipient: &PublicKey, content: &str) -> Result<EventId> {
        let builder = EventBuilder::new(Kind::Custom(4), content)
            .tag(Tag::public_key(*recipient));

        let output = self.client.send_event_builder(builder).await
            .map_err(|e| anyhow::anyhow!("Failed to send DM: {}", e))?;

        info!("Sent DM to {}: {}", &recipient.to_hex()[..8], output.val);
        Ok(output.val)
    }

    pub async fn disconnect(&self) -> Result<()> {
        info!("Disconnecting");
        self.client.disconnect().await;
        Ok(())
    }

    pub fn our_pubkey(&self) -> PublicKey { self.our_pubkey }
    pub fn subscribed_groups(&self) -> &HashSet<String> { &self.subscribed_groups }
}

pub fn create_keys_from_nsec(nsec: &str) -> Result<Keys> {
    Keys::parse(nsec).with_context(|| "Failed to parse nsec key")
}
