use nostr_sdk::{
    Event, EventId, Keys, Kind, PublicKey, Tag, TagKind,
    EventBuilder, Alphabet, SingleLetterTag,
    secp256k1::schnorr::Signature,
};
use anyhow::{Result, Context};
use std::collections::HashSet;
use std::str::FromStr;
use tokio::sync::mpsc;
use tracing::{info, debug, warn, error};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

pub struct RelayClient {
    keys: Keys,
    relay_url: String,
    our_pubkey: PublicKey,
    subscribed_groups: HashSet<String>,
    ws_tx: Option<futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
        WsMessage,
    >>,
}

#[derive(Debug, Clone)]
pub enum RelayEvent {
    GroupMessage { event: Event, group: String },
    DirectMessage { event: Event },
    ProfileUpdate { event: Event },
}

pub fn create_keys_from_nsec(nsec: &str) -> Result<Keys> {
    let secret_key = nostr_sdk::SecretKey::parse(nsec)
        .map_err(|e| anyhow::anyhow!("Failed to parse nsec: {}", e))?;
    Ok(Keys::new(secret_key))
}

impl RelayClient {
    pub async fn new(relay_url: &str, keys: Keys) -> Result<Self> {
        let our_pubkey = keys.public_key();
        Ok(Self {
            keys,
            relay_url: relay_url.to_string(),
            our_pubkey,
            subscribed_groups: HashSet::new(),
            ws_tx: None,
        })
    }

    pub async fn connect(&mut self) -> Result<()> {
        // Don't connect here — connect in start_event_stream which owns the WS
        info!("Connecting to {}...", self.relay_url);
        Ok(())
    }

    pub async fn subscribe_groups(&mut self, groups: &[String]) -> Result<()> {
        if groups.is_empty() { return Ok(()); }
        info!("Subscribing to groups: {:?}", groups);
        for g in groups {
            self.subscribed_groups.insert(g.clone());
        }
        info!("Subscribed to {} groups", groups.len());
        Ok(())
    }

    pub async fn subscribe_dms(&self) -> Result<()> {
        info!("Subscribing to DMs");
        info!("Subscribed to DMs");
        Ok(())
    }

    pub fn start_event_stream(
        &self,
        tx: mpsc::Sender<RelayEvent>,
        _notifications: tokio::sync::broadcast::Receiver<nostr_sdk::RelayPoolNotification>,
    ) {
        let relay_url = self.relay_url.clone();
        let our_pubkey = self.our_pubkey;
        let keys = self.keys.clone();
        let groups: Vec<String> = self.subscribed_groups.iter().cloned().collect();

        tokio::spawn(async move {
            loop {
                match run_ws_loop(&relay_url, &keys, our_pubkey, &groups, &tx).await {
                    Ok(()) => {
                        info!("WebSocket loop ended cleanly, reconnecting...");
                    }
                    Err(e) => {
                        error!("WebSocket error: {}, reconnecting in 5s...", e);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
    }

    // Keep these for API compatibility
    pub fn notifications(&self) -> tokio::sync::broadcast::Receiver<nostr_sdk::RelayPoolNotification> {
        let (tx, rx) = tokio::sync::broadcast::channel(1);
        drop(tx);
        rx
    }

    pub fn subscribed_groups(&self) -> &HashSet<String> {
        &self.subscribed_groups
    }

    pub fn our_pubkey(&self) -> PublicKey {
        self.our_pubkey
    }

    pub async fn send_group_message(&self, group: &str, content: &str) -> Result<EventId> {
        let builder = EventBuilder::new(Kind::Custom(9), content)
            .tag(Tag::custom(TagKind::h(), vec![group.to_string()]));
        let event = builder.sign_with_keys(&self.keys)?;
        let event_id = event.id;
        
        // Send via a new temporary connection
        let (ws, _) = connect_async(&self.relay_url).await
            .context("Failed to connect for sending")?;
        let (mut write, _) = ws.split();
        let event_json = serde_json::to_string(&event)?;
        let msg = format!(r#"["EVENT",{}]"#, event_json);
        write.send(WsMessage::Text(msg.into())).await?;
        write.close().await.ok();
        
        Ok(event_id)
    }

    pub async fn send_dm(&self, recipient: &PublicKey, content: &str) -> Result<EventId> {
        // Simplified — just sign and send
        let builder = EventBuilder::new(Kind::Custom(4), content)
            .tag(Tag::public_key(*recipient));
        let event = builder.sign_with_keys(&self.keys)?;
        let event_id = event.id;
        
        let (ws, _) = connect_async(&self.relay_url).await
            .context("Failed to connect for sending DM")?;
        let (mut write, _) = ws.split();
        let event_json = serde_json::to_string(&event)?;
        let msg = format!(r#"["EVENT",{}]"#, event_json);
        write.send(WsMessage::Text(msg.into())).await?;
        write.close().await.ok();
        
        Ok(event_id)
    }

    pub async fn disconnect(&self) -> Result<()> {
        info!("Disconnecting");
        Ok(())
    }
}

async fn run_ws_loop(
    relay_url: &str,
    keys: &Keys,
    our_pubkey: PublicKey,
    groups: &[String],
    tx: &mpsc::Sender<RelayEvent>,
) -> Result<()> {
    info!("Connecting to {}...", relay_url);
    let (ws, _) = connect_async(relay_url).await
        .context("WebSocket connection failed")?;
    info!("Connected to '{}'", relay_url);

    let (mut write, mut read) = ws.split();

    // Handle AUTH challenge and subscriptions
    let since = chrono::Utc::now().timestamp() - 3600;
    let mut authenticated = false;
    let mut subscribed = false;

    while let Some(msg) = read.next().await {
        let msg = msg.context("WebSocket read error")?;
        let text = match msg {
            WsMessage::Text(t) => t.to_string(),
            WsMessage::Ping(d) => {
                write.send(WsMessage::Pong(d)).await.ok();
                continue;
            }
            WsMessage::Close(_) => {
                info!("Relay sent close frame");
                return Ok(());
            }
            _ => continue,
        };

        // Parse JSON array
        let parsed: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let arr = match parsed.as_array() {
            Some(a) => a,
            None => continue,
        };

        let msg_type = arr.get(0).and_then(|v| v.as_str()).unwrap_or("");

        match msg_type {
            "AUTH" => {
                let challenge = arr.get(1).and_then(|v| v.as_str()).unwrap_or("");
                debug!("AUTH challenge: {}", challenge);
                
                let auth_event = EventBuilder::new(Kind::Custom(22242), "")
                    .tag(Tag::custom(TagKind::Custom("challenge".into()), vec![challenge.to_string()]))
                    .tag(Tag::custom(TagKind::Custom("relay".into()), vec![relay_url.to_string()]))
                    .sign_with_keys(keys)?;
                
                let auth_json = serde_json::to_string(&auth_event)?;
                let auth_msg = format!(r#"["AUTH",{}]"#, auth_json);
                write.send(WsMessage::Text(auth_msg.into())).await?;
                info!("Sent AUTH response");
            }
            "OK" => {
                if !authenticated {
                    authenticated = true;
                    info!("Authenticated to relay");
                    
                    // Now subscribe
                    if !groups.is_empty() {
                        let filter = serde_json::json!({
                            "kinds": [9],
                            "#h": groups,
                            "since": since
                        });
                        let sub_msg = serde_json::json!(["REQ", "groups", filter]).to_string();
                        write.send(WsMessage::Text(sub_msg.into())).await?;
                        info!("Subscribed to groups: {:?}", groups);
                    }
                    
                    // Subscribe to DMs
                    let dm_filter = serde_json::json!({
                        "kinds": [4],
                        "#p": [our_pubkey.to_hex()],
                        "since": since
                    });
                    let dm_msg = serde_json::json!(["REQ", "dms", dm_filter]).to_string();
                    write.send(WsMessage::Text(dm_msg.into())).await?;
                    info!("Subscribed to DMs");
                    subscribed = true;
                }
            }
            "EVENT" => {
                // Parse the event
                let event_json = match arr.get(2) {
                    Some(v) => v,
                    None => continue,
                };
                
                let event: Event = match serde_json::from_value(event_json.clone()) {
                    Ok(e) => e,
                    Err(e) => {
                        warn!("Failed to parse event: {}", e);
                        continue;
                    }
                };

                // Skip own events
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
                        
                        info!("Live event: #{} from {} : {}", group, &event.pubkey.to_hex()[..8], 
                              &event.content[..event.content.len().min(60)]);
                        Some(RelayEvent::GroupMessage { event, group })
                    }
                    4 => {
                        info!("Live event: DM from {}", &event.pubkey.to_hex()[..8]);
                        Some(RelayEvent::DirectMessage { event })
                    }
                    0 => {
                        debug!("Profile update from {}", &event.pubkey.to_hex()[..8]);
                        Some(RelayEvent::ProfileUpdate { event })
                    }
                    _ => None,
                };

                if let Some(re) = relay_event {
                    if tx.send(re).await.is_err() {
                        warn!("Event receiver dropped");
                        return Ok(());
                    }
                }
            }
            "EOSE" => {
                let sub_id = arr.get(1).and_then(|v| v.as_str()).unwrap_or("?");
                info!("End of stored events for sub '{}'", sub_id);
            }
            "NOTICE" => {
                let notice = arr.get(1).and_then(|v| v.as_str()).unwrap_or("");
                warn!("Relay notice: {}", notice);
            }
            _ => {
                debug!("Relay msg: {}", msg_type);
            }
        }
    }

    Ok(())
}
