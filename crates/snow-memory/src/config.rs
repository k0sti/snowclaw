//! Configuration for the collective memory system.

use crate::types::SourcePreference;
use serde::{Deserialize, Serialize};

/// Top-level memory configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryConfig {
    /// Ordered source preference list. Higher trust = more trusted.
    #[serde(default)]
    pub sources: Vec<SourcePreference>,

    /// Tier 1 models (highest capability).
    #[serde(default)]
    pub tier1: Vec<String>,
    /// Tier 2 models.
    #[serde(default)]
    pub tier2: Vec<String>,
    /// Tier 3 models.
    #[serde(default)]
    pub tier3: Vec<String>,
    /// Tier 4 models (lowest capability, includes wildcards like "meta/llama-*").
    #[serde(default)]
    pub tier4: Vec<String>,

    /// Relay URLs for public tier memories.
    #[serde(default)]
    pub relays_public: Vec<String>,
    /// Relay URLs for group tier memories.
    #[serde(default)]
    pub relays_group: Vec<String>,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            sources: vec![],
            tier1: vec![
                "anthropic/claude-opus-4".to_string(),
                "anthropic/claude-opus-4-6".to_string(),
                "openai/o3".to_string(),
                "openai/gpt-5".to_string(),
            ],
            tier2: vec![
                "anthropic/claude-sonnet-4".to_string(),
                "anthropic/claude-sonnet-4-6".to_string(),
                "openai/gpt-4.1".to_string(),
                "google/gemini-2.5-pro".to_string(),
            ],
            tier3: vec![
                "anthropic/claude-haiku".to_string(),
                "openai/gpt-4.1-mini".to_string(),
            ],
            tier4: vec![
                "meta/llama-*".to_string(),
                "mistral/*".to_string(),
                "local/*".to_string(),
            ],
            relays_public: vec![],
            relays_group: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_model_tiers() {
        let config = MemoryConfig::default();
        assert!(!config.tier1.is_empty());
        assert!(!config.tier2.is_empty());
        assert!(!config.tier3.is_empty());
        assert!(!config.tier4.is_empty());
    }

    #[test]
    fn toml_roundtrip() {
        let config = MemoryConfig {
            sources: vec![
                SourcePreference::for_npub("abc123", 1.0),
                SourcePreference::for_group("dev-team", 0.9),
            ],
            relays_public: vec!["wss://relay.example.com".to_string()],
            ..Default::default()
        };

        let toml_str = toml::to_string_pretty(&config).unwrap();
        let recovered: MemoryConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(recovered, config);
    }

    #[test]
    fn deserialize_empty_config() {
        let toml_str = "";
        let config: MemoryConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.sources.len(), 0);
        assert_eq!(config.tier1.len(), 0); // empty string deserializes with no defaults
    }

    #[test]
    fn deserialize_partial_config() {
        let toml_str = r#"
tier1 = ["anthropic/claude-opus-4-6"]
relays_public = ["wss://relay.damus.io"]
"#;
        let config: MemoryConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.tier1, vec!["anthropic/claude-opus-4-6"]);
        assert_eq!(config.relays_public, vec!["wss://relay.damus.io"]);
        assert!(config.sources.is_empty());
    }
}
