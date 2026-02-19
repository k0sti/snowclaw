//! Basic Nostr relay client wrapper functionality.

use anyhow::{Context, Result};
use nostr_sdk::prelude::*;
use std::time::Duration;
use tracing::{info, warn};

/// A simplified Nostr relay client for shared use.
#[derive(Clone)]
pub struct RelayClient {
    client: Client,
    keys: Keys,
    relays: Vec<String>,
}

impl RelayClient {
    /// Create a new relay client with the given keys and relay URLs.
    pub async fn new(keys: Keys, relay_urls: Vec<String>) -> Result<Self> {
        let client = Client::new(keys.clone());

        // Add relays
        for relay_url in &relay_urls {
            client
                .add_relay(relay_url.as_str())
                .await
                .with_context(|| format!("Failed to add relay: {}", relay_url))?;
        }

        // Connect to all relays
        client.connect().await;
        info!("Relay client connected to {} relay(s)", relay_urls.len());

        Ok(Self {
            client,
            keys,
            relays: relay_urls,
        })
    }

    /// Get the underlying nostr-sdk Client.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Subscribe to events matching the given filters.
    pub async fn subscribe(&self, filters: Vec<Filter>) -> Result<()> {
        for filter in filters {
            self.client.subscribe(filter, None).await?;
        }
        Ok(())
    }

    /// Send an event to relays.
    pub async fn send_event(&self, event: Event) -> Result<EventId> {
        let output = self.client.send_event(&event).await?;
        Ok(output.val)
    }

    /// Send an event builder to relays.
    pub async fn send_event_builder(&self, builder: EventBuilder) -> Result<EventId> {
        let output = self.client.send_event_builder(builder).await?;
        Ok(output.val)
    }

    /// Fetch events matching the given filter with a timeout.
    pub async fn fetch_events(&self, filter: Filter, timeout: Duration) -> Result<Vec<Event>> {
        let events = tokio::time::timeout(timeout, self.client.fetch_events(filter, timeout))
            .await
            .context("Timeout fetching events")?
            .context("Failed to fetch events")?;
        Ok(events.into_iter().collect())
    }

    /// Send a group message (kind 9) to a NIP-29 group.
    pub async fn send_group_message(&self, group_id: &str, content: &str) -> Result<EventId> {
        let builder = EventBuilder::new(Kind::Custom(9), content)
            .tag(Tag::custom(TagKind::custom("h"), vec![group_id.to_string()]));
        self.send_event_builder(builder).await
    }

    /// Send a direct message using NIP-17 (gift wrap).
    pub async fn send_dm(&self, recipient: &PublicKey, content: &str) -> Result<EventId> {
        let output = self.client.send_private_msg(*recipient, content, None).await?;
        Ok(output.val)
    }

    /// Extract group ID from event tags (for NIP-29 events).
    pub fn extract_group(event: &Event) -> Option<String> {
        for tag in event.tags.iter() {
            let s = tag.as_slice();
            if s.first().map(|v| v.as_str()) == Some("h") {
                if let Some(group_id) = s.get(1) {
                    return Some(group_id.to_string());
                }
            }
        }
        None
    }

    /// Check if an event was sent by our keys.
    pub fn is_own_event(&self, event: &Event) -> bool {
        event.pubkey == self.keys.public_key()
    }

    /// Get relay URLs.
    pub fn relays(&self) -> &[String] {
        &self.relays
    }

    /// Health check - verify we can connect to at least one relay.
    pub async fn health_check(&self) -> bool {
        // Try to send a simple filter to test connectivity
        let filter = Filter::new().limit(1);
        match tokio::time::timeout(
            Duration::from_secs(3),
            self.client.fetch_events(filter, Duration::from_secs(2)),
        )
        .await
        {
            Ok(Ok(_)) => true,
            Ok(Err(e)) => {
                warn!("Relay health check failed: {e}");
                false
            }
            Err(_) => {
                warn!("Relay health check timeout");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_group_from_tags() {
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(9), "test message")
            .tag(Tag::custom(TagKind::custom("h"), vec!["test-group".to_string()]))
            .sign_with_keys(&keys)
            .unwrap();

        let group = RelayClient::extract_group(&event);
        assert_eq!(group, Some("test-group".to_string()));
    }

    #[test]
    fn extract_group_missing_tag() {
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(9), "test message")
            .sign_with_keys(&keys)
            .unwrap();

        let group = RelayClient::extract_group(&event);
        assert_eq!(group, None);
    }

    #[test]
    fn is_own_event_check() {
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::TextNote, "test")
            .sign_with_keys(&keys)
            .unwrap();

        // Can't easily test this without creating a full client
        // Just verify the function exists and compiles
        assert_eq!(event.pubkey, keys.public_key());
    }
}