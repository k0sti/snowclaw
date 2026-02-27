//! Conversion between Memory/AgentProfile and NIP-78 Nostr event structures.
//!
//! This module works with a generic tag-based representation rather than
//! depending on nostr-sdk directly, keeping the crate lightweight.
//! Integrators convert to/from their concrete Nostr event types.

use crate::types::{AgentProfile, Memory, MemoryTier};
use serde::{Deserialize, Serialize};

/// NIP-78 event kind for application-specific data.
pub const KIND_APP_SPECIFIC: u64 = 30078;
/// Kind 0 for metadata/profile.
pub const KIND_METADATA: u64 = 0;
/// Tag prefix for snow memory d-tags.
pub const D_TAG_PREFIX: &str = "snow:memory:";

/// Required snow: tag names.
pub const TAG_TIER: &str = "snow:tier";
pub const TAG_MODEL: &str = "snow:model";
pub const TAG_CONFIDENCE: &str = "snow:confidence";
pub const TAG_SOURCE: &str = "snow:source";
pub const TAG_VERSION: &str = "snow:version";
pub const TAG_SUPERSEDES: &str = "snow:supersedes";

/// A lightweight representation of a Nostr event for conversion purposes.
/// Integrators map this to/from their concrete event types (e.g. nostr_sdk::Event).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryEvent {
    pub id: String,
    pub kind: u64,
    pub pubkey: String,
    pub created_at: u64,
    /// Tags as `[key, value]` pairs.
    pub tags: Vec<(String, String)>,
    pub content: String,
}

/// Content payload serialized into the event content field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryContent {
    pub summary: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

/// Errors from event conversion.
#[derive(Debug, Clone, PartialEq)]
pub enum ConversionError {
    MissingTag(String),
    InvalidTag { tag: String, reason: String },
    InvalidContent(String),
    WrongKind(u64),
    InvalidDTag(String),
}

impl std::fmt::Display for ConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConversionError::MissingTag(t) => write!(f, "missing required tag: {}", t),
            ConversionError::InvalidTag { tag, reason } => {
                write!(f, "invalid tag '{}': {}", tag, reason)
            }
            ConversionError::InvalidContent(e) => write!(f, "invalid content: {}", e),
            ConversionError::WrongKind(k) => write!(f, "wrong event kind: {}, expected {}", k, KIND_APP_SPECIFIC),
            ConversionError::InvalidDTag(d) => {
                write!(f, "invalid d-tag: '{}', expected prefix '{}'", d, D_TAG_PREFIX)
            }
        }
    }
}

impl std::error::Error for ConversionError {}

impl MemoryEvent {
    fn get_tag(&self, key: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn require_tag(&self, key: &str) -> Result<&str, ConversionError> {
        self.get_tag(key)
            .ok_or_else(|| ConversionError::MissingTag(key.to_string()))
    }
}

/// Convert a Memory to a MemoryEvent.
pub fn memory_to_event(memory: &Memory) -> MemoryEvent {
    let content = serde_json::to_string(&MemoryContent {
        summary: memory.summary.clone(),
        detail: memory.detail.clone(),
        context: memory.context.clone(),
    })
    .expect("MemoryContent is always serializable");

    let mut tags = vec![
        ("d".to_string(), format!("{}{}", D_TAG_PREFIX, memory.topic)),
        (TAG_TIER.to_string(), memory.tier.as_tag_value().to_string()),
        (TAG_MODEL.to_string(), memory.model.clone()),
        (
            TAG_CONFIDENCE.to_string(),
            format!("{:.2}", memory.confidence),
        ),
        (TAG_SOURCE.to_string(), memory.source.clone()),
        (TAG_VERSION.to_string(), memory.version.to_string()),
    ];

    if let Some(ref sup) = memory.supersedes {
        tags.push((TAG_SUPERSEDES.to_string(), sup.clone()));
    }

    for tag in &memory.tags {
        tags.push(("t".to_string(), tag.clone()));
    }

    MemoryEvent {
        id: memory.id.clone(),
        kind: KIND_APP_SPECIFIC,
        pubkey: memory.source.clone(),
        created_at: memory.created_at,
        tags,
        content,
    }
}

/// Convert a MemoryEvent back to a Memory. Validates required tags.
pub fn memory_from_event(event: &MemoryEvent) -> Result<Memory, ConversionError> {
    if event.kind != KIND_APP_SPECIFIC {
        return Err(ConversionError::WrongKind(event.kind));
    }

    let d_tag = event.require_tag("d")?;
    let topic = d_tag
        .strip_prefix(D_TAG_PREFIX)
        .ok_or_else(|| ConversionError::InvalidDTag(d_tag.to_string()))?
        .to_string();

    let tier_str = event.require_tag(TAG_TIER)?;
    let tier = match tier_str {
        "public" => MemoryTier::Public,
        "group" => MemoryTier::Group(String::new()),
        "private" => MemoryTier::Private(String::new()),
        other => {
            return Err(ConversionError::InvalidTag {
                tag: TAG_TIER.to_string(),
                reason: format!("unknown tier: {}", other),
            })
        }
    };

    let model = event.require_tag(TAG_MODEL)?.to_string();
    let confidence_str = event.require_tag(TAG_CONFIDENCE)?;
    let confidence: f64 = confidence_str.parse().map_err(|_| ConversionError::InvalidTag {
        tag: TAG_CONFIDENCE.to_string(),
        reason: format!("not a valid f64: {}", confidence_str),
    })?;

    if !(0.0..=1.0).contains(&confidence) {
        return Err(ConversionError::InvalidTag {
            tag: TAG_CONFIDENCE.to_string(),
            reason: format!("out of range [0.0, 1.0]: {}", confidence),
        });
    }

    let source = event.require_tag(TAG_SOURCE)?.to_string();
    let version_str = event.require_tag(TAG_VERSION)?;
    let version: u32 = version_str.parse().map_err(|_| ConversionError::InvalidTag {
        tag: TAG_VERSION.to_string(),
        reason: format!("not a valid u32: {}", version_str),
    })?;

    let supersedes = event.get_tag(TAG_SUPERSEDES).map(|s| s.to_string());

    let content: MemoryContent = serde_json::from_str(&event.content)
        .map_err(|e| ConversionError::InvalidContent(e.to_string()))?;

    let tags: Vec<String> = event
        .tags
        .iter()
        .filter(|(k, _)| k == "t")
        .map(|(_, v)| v.clone())
        .collect();

    Ok(Memory {
        id: event.id.clone(),
        tier,
        topic,
        summary: content.summary,
        detail: content.detail,
        context: content.context,
        source,
        model,
        confidence,
        supersedes,
        version,
        tags,
        created_at: event.created_at,
    })
}

/// Convert an AgentProfile to a kind-0-style metadata JSON string.
pub fn profile_to_metadata(profile: &AgentProfile) -> String {
    let mut map = serde_json::Map::new();
    map.insert("name".to_string(), serde_json::Value::String(profile.name.clone()));
    map.insert("about".to_string(), serde_json::Value::String(profile.about.clone()));
    map.insert(
        "snow:model".to_string(),
        serde_json::Value::String(profile.model.clone()),
    );
    map.insert(
        "snow:version".to_string(),
        serde_json::Value::String(profile.version.clone()),
    );
    map.insert(
        "snow:capabilities".to_string(),
        serde_json::Value::Array(
            profile
                .capabilities
                .iter()
                .map(|c| serde_json::Value::String(c.clone()))
                .collect(),
        ),
    );
    if let Some(ref op) = profile.operator {
        map.insert(
            "snow:operator".to_string(),
            serde_json::Value::String(op.clone()),
        );
    }
    serde_json::to_string(&map).expect("profile metadata is always serializable")
}

/// Parse an AgentProfile from a kind-0 metadata JSON string.
pub fn profile_from_metadata(json: &str) -> Result<AgentProfile, ConversionError> {
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(json).map_err(|e| ConversionError::InvalidContent(e.to_string()))?;

    let name = map
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let about = map
        .get("about")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let model = map
        .get("snow:model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConversionError::MissingTag("snow:model".to_string()))?
        .to_string();
    let version = map
        .get("snow:version")
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0")
        .to_string();
    let capabilities = map
        .get("snow:capabilities")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let operator = map
        .get("snow:operator")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(AgentProfile {
        name,
        about,
        model,
        version,
        capabilities,
        operator,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MemoryTier;

    fn sample_memory() -> Memory {
        Memory {
            id: "abc123".to_string(),
            tier: MemoryTier::Public,
            topic: "rust/error-handling".to_string(),
            summary: "Use anyhow for application errors".to_string(),
            detail: "In application code, prefer anyhow::Result for ergonomic error propagation.".to_string(),
            context: None,
            source: "deadbeef".to_string(),
            model: "anthropic/claude-opus-4-6".to_string(),
            confidence: 0.92,
            supersedes: None,
            version: 1,
            tags: vec!["rust".to_string(), "error-handling".to_string()],
            created_at: 1700000000,
        }
    }

    #[test]
    fn roundtrip_memory_event() {
        let mem = sample_memory();
        let event = memory_to_event(&mem);

        assert_eq!(event.kind, KIND_APP_SPECIFIC);
        assert_eq!(event.pubkey, "deadbeef");

        let recovered = memory_from_event(&event).unwrap();
        assert_eq!(recovered.topic, mem.topic);
        assert_eq!(recovered.summary, mem.summary);
        assert_eq!(recovered.detail, mem.detail);
        assert_eq!(recovered.model, mem.model);
        assert_eq!(recovered.confidence, mem.confidence);
        assert_eq!(recovered.version, mem.version);
        assert_eq!(recovered.tags, mem.tags);
        assert_eq!(recovered.source, mem.source);
    }

    #[test]
    fn roundtrip_with_supersedes() {
        let mut mem = sample_memory();
        mem.supersedes = Some("prev_event_id".to_string());
        mem.version = 2;

        let event = memory_to_event(&mem);
        let recovered = memory_from_event(&event).unwrap();
        assert_eq!(recovered.supersedes, Some("prev_event_id".to_string()));
        assert_eq!(recovered.version, 2);
    }

    #[test]
    fn reject_wrong_kind() {
        let event = MemoryEvent {
            id: "x".to_string(),
            kind: 1,
            pubkey: "pk".to_string(),
            created_at: 0,
            tags: vec![],
            content: "{}".to_string(),
        };
        assert!(matches!(
            memory_from_event(&event),
            Err(ConversionError::WrongKind(1))
        ));
    }

    #[test]
    fn reject_missing_d_tag() {
        let event = MemoryEvent {
            id: "x".to_string(),
            kind: KIND_APP_SPECIFIC,
            pubkey: "pk".to_string(),
            created_at: 0,
            tags: vec![],
            content: "{}".to_string(),
        };
        assert!(matches!(
            memory_from_event(&event),
            Err(ConversionError::MissingTag(_))
        ));
    }

    #[test]
    fn reject_invalid_confidence() {
        let mut mem = sample_memory();
        mem.confidence = 1.5;
        let mut event = memory_to_event(&mem);
        // Manually fix the confidence tag to an out-of-range value
        for tag in &mut event.tags {
            if tag.0 == TAG_CONFIDENCE {
                tag.1 = "1.50".to_string();
            }
        }
        assert!(matches!(
            memory_from_event(&event),
            Err(ConversionError::InvalidTag { .. })
        ));
    }

    #[test]
    fn roundtrip_agent_profile() {
        let profile = AgentProfile {
            name: "snow-studio".to_string(),
            about: "Snowclaw instance on studio".to_string(),
            model: "anthropic/claude-opus-4-6".to_string(),
            version: "0.1.0".to_string(),
            capabilities: vec!["memory".to_string(), "code".to_string()],
            operator: Some("operator_npub".to_string()),
        };

        let json = profile_to_metadata(&profile);
        let recovered = profile_from_metadata(&json).unwrap();
        assert_eq!(recovered.name, profile.name);
        assert_eq!(recovered.model, profile.model);
        assert_eq!(recovered.capabilities, profile.capabilities);
        assert_eq!(recovered.operator, profile.operator);
    }

    #[test]
    fn profile_missing_model() {
        let json = r#"{"name": "test"}"#;
        assert!(matches!(
            profile_from_metadata(json),
            Err(ConversionError::MissingTag(_))
        ));
    }
}
