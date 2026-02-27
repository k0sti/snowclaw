//! Core types for the collective memory system.

use serde::{Deserialize, Serialize};

/// A memory entry in the collective memory system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Memory {
    /// Unique identifier (Nostr event id when published).
    pub id: String,
    /// Visibility tier.
    pub tier: MemoryTier,
    /// Namespaced topic key (e.g. "rust/error-handling").
    pub topic: String,
    /// Short summary of the memory.
    pub summary: String,
    /// Full detail / body.
    pub detail: String,
    /// Optional structured context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Pubkey (hex) of the originating agent.
    pub source: String,
    /// Model identifier (e.g. "anthropic/claude-opus-4-6").
    pub model: String,
    /// Self-assessed confidence score (0.0–1.0).
    pub confidence: f64,
    /// Event id of the previous version this supersedes, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    /// Version number (monotonically increasing per topic+source).
    #[serde(default = "default_version")]
    pub version: u32,
    /// Topic tags for relay-side filtering.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Unix timestamp of creation.
    pub created_at: u64,
}

fn default_version() -> u32 {
    1
}

/// Visibility tier for a memory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "type", content = "value")]
pub enum MemoryTier {
    /// Published to public relays, readable by any Snowclaw instance.
    Public,
    /// Scoped to a group (relay-scoped or NIP-44 encrypted).
    Group(String),
    /// Encrypted between one agent and one human.
    Private(String),
}

impl MemoryTier {
    pub fn as_tag_value(&self) -> &str {
        match self {
            MemoryTier::Public => "public",
            MemoryTier::Group(_) => "group",
            MemoryTier::Private(_) => "private",
        }
    }
}

impl std::fmt::Display for MemoryTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryTier::Public => write!(f, "public"),
            MemoryTier::Group(id) => write!(f, "group:{}", id),
            MemoryTier::Private(pk) => write!(f, "private:{}", pk),
        }
    }
}

/// A source in the preference list for ranking.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourcePreference {
    /// Npub (hex pubkey) or group identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub npub: Option<String>,
    /// Group identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Trust weight (0.0–1.0). Higher = more trusted.
    pub trust: f64,
}

impl SourcePreference {
    pub fn for_npub(npub: &str, trust: f64) -> Self {
        Self {
            npub: Some(npub.to_string()),
            group: None,
            trust,
        }
    }

    pub fn for_group(group: &str, trust: f64) -> Self {
        Self {
            npub: None,
            group: Some(group.to_string()),
            trust,
        }
    }

    /// Check if this preference matches a given source pubkey.
    pub fn matches_source(&self, source_pubkey: &str) -> bool {
        if let Some(ref npub) = self.npub {
            return npub == source_pubkey;
        }
        false
    }
}

/// A ranked search result combining a memory with its scoring breakdown.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchResult {
    /// The memory itself.
    pub memory: Memory,
    /// Raw relevance score from search (0.0–1.0).
    pub relevance: f64,
    /// Source trust weight applied.
    pub source_trust: f64,
    /// Model tier (1 = best, 4 = lowest).
    pub model_tier: u8,
    /// Final effective score: relevance * source_trust * tier_weight.
    pub effective_score: f64,
}

/// Agent profile metadata published as kind 0.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentProfile {
    /// Display name.
    pub name: String,
    /// Description.
    #[serde(default)]
    pub about: String,
    /// Model backing this agent (e.g. "anthropic/claude-opus-4-6").
    pub model: String,
    /// Snowclaw version.
    pub version: String,
    /// Capability tags.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Npub (hex pubkey) of the human operator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
}
