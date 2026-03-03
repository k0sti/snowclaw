//! Publish memories and agent profiles as Nostr events.
//!
//! This module handles serialization and signing of memory events.
//! Actual relay transport is handled by the caller (agent runtime or CLI).

use crate::event;
use crate::types::{AgentProfile, Memory};
use sha2::{Digest, Sha256};

/// A signed Nostr event ready to be sent to relays.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SignedEvent {
    pub id: String,
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

/// An unsigned event that needs signing before publishing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UnsignedEvent {
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
}

impl UnsignedEvent {
    /// Compute the event ID (SHA-256 of the canonical serialization).
    pub fn compute_id(&self) -> String {
        let canonical = serde_json::json!([
            0,
            self.pubkey,
            self.created_at,
            self.kind,
            self.tags,
            self.content,
        ]);
        let serialized = serde_json::to_string(&canonical).unwrap_or_default();
        let hash = Sha256::digest(serialized.as_bytes());
        hex::encode(hash)
    }
}

/// Build an unsigned NIP-78 memory event.
pub fn build_memory_event(memory: &Memory, pubkey: &str) -> UnsignedEvent {
    let nostr_event = event::memory_to_event(memory);

    UnsignedEvent {
        pubkey: pubkey.to_string(),
        created_at: nostr_event.created_at,
        kind: nostr_event.kind as u32,
        tags: nostr_event.tags.into_iter().map(|(k, v)| vec![k, v]).collect(),
        content: nostr_event.content,
    }
}

/// Build an unsigned kind 0 agent profile event.
pub fn build_profile_event(profile: &AgentProfile, pubkey: &str) -> UnsignedEvent {
    let nostr_event = event::profile_to_event(profile, pubkey);

    UnsignedEvent {
        pubkey: pubkey.to_string(),
        created_at: nostr_event.created_at,
        kind: nostr_event.kind as u32,
        tags: nostr_event.tags.into_iter().map(|(k, v)| vec![k, v]).collect(),
        content: nostr_event.content,
    }
}

/// Serialize a signed event as a Nostr ["EVENT", <event>] message for relay submission.
pub fn to_relay_message(event: &SignedEvent) -> String {
    serde_json::json!(["EVENT", event]).to_string()
}

/// Build a ["REQ", subscription_id, filter] message for subscribing to memory events.
pub fn build_memory_subscription(sub_id: &str, since: Option<u64>) -> String {
    let mut filter = serde_json::json!({
        "kinds": [30078],
        "#d": ["snow:memory:"]
    });

    if let Some(since) = since {
        filter["since"] = serde_json::json!(since);
    }

    serde_json::json!(["REQ", sub_id, filter]).to_string()
}

/// Build a filter for agent profile events with snow: metadata.
pub fn build_profile_subscription(sub_id: &str, pubkeys: Option<&[&str]>) -> String {
    let mut filter = serde_json::json!({
        "kinds": [0],
    });

    if let Some(pks) = pubkeys {
        filter["authors"] = serde_json::json!(pks);
    }

    serde_json::json!(["REQ", sub_id, filter]).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Memory, MemoryTier};

    #[test]
    fn test_build_memory_event() {
        let memory = Memory {
            id: String::new(),
            tier: MemoryTier::Public,
            topic: "rust/errors".to_string(),
            summary: "How to handle errors".to_string(),
            detail: "Use Result type".to_string(),
            context: None,
            source: "aabbccdd".to_string(),
            model: "test/model".to_string(),
            confidence: 0.9,
            supersedes: None,
            version: 1,
            tags: vec!["rust".to_string()],
            created_at: 1700000000,
        };

        let event = build_memory_event(&memory, "aabbccdd");
        assert_eq!(event.kind, 30078);
        assert!(!event.compute_id().is_empty());
        assert_eq!(event.pubkey, "aabbccdd");
    }

    #[test]
    fn test_build_profile_event() {
        let profile = AgentProfile {
            name: "snow-test".to_string(),
            about: "Test agent".to_string(),
            model: "test/model".to_string(),
            version: "0.1.0".to_string(),
            capabilities: vec!["memory".to_string()],
            operator: None,
        };

        let event = build_profile_event(&profile, "aabbccdd");
        assert_eq!(event.kind, 0);
    }

    #[test]
    fn test_relay_messages() {
        let sub = build_memory_subscription("sub1", Some(1700000000));
        assert!(sub.contains("REQ"));
        assert!(sub.contains("sub1"));

        let prof_sub = build_profile_subscription("sub2", Some(&["aabb"]));
        assert!(prof_sub.contains("aabb"));
    }
}
