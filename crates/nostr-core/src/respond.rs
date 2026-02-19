//! Response mode configuration for Nostr channels.

// use serde::{Deserialize, Serialize}; // Not needed yet
use std::collections::HashMap;

/// Respond mode for group messages
#[derive(Debug, Clone, PartialEq)]
pub enum RespondMode {
    /// Reply to all messages
    All,
    /// Only reply when mentioned by name/npub or replied to
    Mention,
    /// Respond only to guardian's messages
    Guardian,
    /// Listen only, never auto-reply
    None,
}

impl RespondMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "all" => Self::All,
            "guardian" => Self::Guardian,
            "none" | "silent" | "listen" => Self::None,
            _ => Self::Mention,
        }
    }
}

/// Per-group dynamic configuration loaded from NIP-78 kind 30078 events.
#[derive(Debug, Clone, Default)]
pub struct GroupConfig {
    pub respond_mode: Option<RespondMode>,
    pub context_history: Option<usize>,
}

/// Dynamic configuration loaded from NIP-78 events, keyed by scope.
#[derive(Debug, Clone, Default)]
pub struct DynamicConfig {
    pub global: Option<GroupConfig>,
    pub groups: HashMap<String, GroupConfig>,
    pub npubs: HashMap<String, GroupConfig>,
}

/// Parse a NIP-78 kind 30078 config event into a (scope, GroupConfig) pair.
pub fn parse_config_event(event: &nostr_sdk::Event) -> Option<(String, GroupConfig)> {
    let d_tag = event.tags.iter().find_map(|tag| {
        let s = tag.as_slice();
        if s.first().map(|v| v.as_str()) == Some("d") {
            s.get(1).map(|v| v.to_string())
        } else {
            None
        }
    })?;

    if !d_tag.starts_with("snowclaw:config:") {
        return None;
    }

    let mut gc = GroupConfig::default();

    for tag in event.tags.iter() {
        let s = tag.as_slice();
        match s.first().map(|v| v.as_str()) {
            Some("respond_mode") => {
                if let Some(val) = s.get(1) {
                    gc.respond_mode = Some(RespondMode::from_str(val));
                }
            }
            Some("context_history") => {
                if let Some(val) = s.get(1) {
                    if let Ok(n) = val.parse::<usize>() {
                        gc.context_history = Some(n);
                    }
                }
            }
            _ => {}
        }
    }

    Some((d_tag, gc))
}

/// Apply a parsed config entry to the dynamic config.
pub fn apply_config_entry(config: &mut DynamicConfig, (d_tag, gc): (String, GroupConfig)) {
    if d_tag == "snowclaw:config:global" {
        config.global = Some(gc);
    } else if let Some(group) = d_tag.strip_prefix("snowclaw:config:group:") {
        config.groups.insert(group.to_string(), gc);
    } else if let Some(npub) = d_tag.strip_prefix("snowclaw:config:npub:") {
        config.npubs.insert(npub.to_string(), gc);
    }
}

/// Get the effective respond mode for a group (dynamic > file > default).
pub async fn respond_mode_for_group(
    dynamic_config: &tokio::sync::RwLock<DynamicConfig>,
    group_respond_mode: &HashMap<String, RespondMode>,
    default_respond_mode: &RespondMode,
    group: &str,
) -> RespondMode {
    // Check dynamic config first
    let dc = dynamic_config.read().await;
    if let Some(gc) = dc.groups.get(group) {
        if let Some(ref mode) = gc.respond_mode {
            return mode.clone();
        }
    }
    if let Some(ref gc) = dc.global {
        if let Some(ref mode) = gc.respond_mode {
            return mode.clone();
        }
    }
    drop(dc);

    // Then file config
    if let Some(mode) = group_respond_mode.get(group) {
        return mode.clone();
    }

    // Then default
    default_respond_mode.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::*;

    #[test]
    fn respond_mode_from_str_guardian() {
        assert_eq!(RespondMode::from_str("guardian"), RespondMode::Guardian);
        assert_eq!(RespondMode::from_str("Guardian"), RespondMode::Guardian);
        assert_eq!(RespondMode::from_str("GUARDIAN"), RespondMode::Guardian);
    }

    #[test]
    fn parse_config_event_valid() {
        let keys = Keys::generate();
        let tags = vec![
            Tag::custom(TagKind::custom("d"), vec!["snowclaw:config:group:techteam".to_string()]),
            Tag::custom(TagKind::custom("respond_mode"), vec!["all".to_string()]),
            Tag::custom(TagKind::custom("context_history"), vec!["30".to_string()]),
        ];
        let event = EventBuilder::new(Kind::Custom(30078), "")
            .tags(tags)
            .sign_with_keys(&keys)
            .unwrap();

        let (d_tag, gc) = parse_config_event(&event).unwrap();
        assert_eq!(d_tag, "snowclaw:config:group:techteam");
        assert_eq!(gc.respond_mode, Some(RespondMode::All));
        assert_eq!(gc.context_history, Some(30));
    }

    #[test]
    fn parse_config_event_global() {
        let keys = Keys::generate();
        let tags = vec![
            Tag::custom(TagKind::custom("d"), vec!["snowclaw:config:global".to_string()]),
            Tag::custom(TagKind::custom("respond_mode"), vec!["guardian".to_string()]),
        ];
        let event = EventBuilder::new(Kind::Custom(30078), "")
            .tags(tags)
            .sign_with_keys(&keys)
            .unwrap();

        let (d_tag, gc) = parse_config_event(&event).unwrap();
        assert_eq!(d_tag, "snowclaw:config:global");
        assert_eq!(gc.respond_mode, Some(RespondMode::Guardian));
        assert_eq!(gc.context_history, None);
    }

    #[test]
    fn apply_config_entry_scopes() {
        let mut dc = DynamicConfig::default();
        
        apply_config_entry(&mut dc, ("snowclaw:config:global".into(), GroupConfig {
            respond_mode: Some(RespondMode::Guardian),
            context_history: Some(10),
        }));
        assert!(dc.global.is_some());
        
        apply_config_entry(&mut dc, ("snowclaw:config:group:test".into(), GroupConfig {
            respond_mode: Some(RespondMode::All),
            context_history: None,
        }));
        assert!(dc.groups.contains_key("test"));

        apply_config_entry(&mut dc, ("snowclaw:config:npub:abc123".into(), GroupConfig {
            respond_mode: Some(RespondMode::Mention),
            context_history: Some(5),
        }));
        assert!(dc.npubs.contains_key("abc123"));
    }
}