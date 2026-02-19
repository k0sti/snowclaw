//! Action protocol for Nostr agent communication (kind 1121).

/// Extract action from a kind 1121 event content.
pub fn extract_action(event: &nostr_sdk::Event) -> Option<String> {
    // Look for "action" tag first
    for tag in event.tags.iter() {
        let s = tag.as_slice();
        if s.first().map(|v| v.as_str()) == Some("action") {
            if let Some(action_val) = s.get(1) {
                return Some(action_val.to_string());
            }
        }
    }

    // Fallback: parse JSON from content
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&event.content) {
        if let Some(action) = json.get("action").and_then(|v| v.as_str()) {
            return Some(action.to_string());
        }
    }

    None
}

/// Extract parameters from a kind 1121 action event.
pub fn extract_action_params(event: &nostr_sdk::Event) -> Vec<(String, String)> {
    let mut params = Vec::new();

    // Parse from tags (param:<key> = <value>)
    for tag in event.tags.iter() {
        let s = tag.as_slice();
        if let Some(tag_name) = s.first().map(|v| v.as_str()) {
            if tag_name.starts_with("param:") {
                let key = tag_name.strip_prefix("param:").unwrap().to_string();
                if let Some(value) = s.get(1) {
                    params.push((key, value.to_string()));
                }
            }
        }
    }

    // Also parse from JSON content
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&event.content) {
        if let Some(params_obj) = json.get("params").and_then(|v| v.as_object()) {
            for (key, value) in params_obj {
                if let Some(value_str) = value.as_str() {
                    params.push((key.clone(), value_str.to_string()));
                }
            }
        }
    }

    params
}

/// Extract group from a kind 1121 action event.
pub fn extract_action_group(event: &nostr_sdk::Event) -> Option<String> {
    // Look for "group" tag first
    for tag in event.tags.iter() {
        let s = tag.as_slice();
        if s.first().map(|v| v.as_str()) == Some("group") {
            if let Some(group_val) = s.get(1) {
                return Some(group_val.to_string());
            }
        }
    }

    // Fallback: parse from JSON content
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&event.content) {
        if let Some(group) = json.get("group").and_then(|v| v.as_str()) {
            return Some(group.to_string());
        }
    }

    None
}

/// Check if a kind 1121 event targets a specific pubkey.
pub fn targets_pubkey(event: &nostr_sdk::Event, target_pubkey: &nostr_sdk::PublicKey) -> bool {
    event.tags.iter().any(|tag| {
        let s = tag.as_slice();
        if s.first().map(|v| v.as_str()) == Some("p") {
            if let Some(hex) = s.get(1) {
                if let Ok(pk) = nostr_sdk::PublicKey::from_hex(hex) {
                    return pk == *target_pubkey;
                }
            }
        }
        false
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::*;

    #[test]
    fn extract_action_from_tag() {
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(1121), "")
            .tag(Tag::custom(TagKind::custom("action"), vec!["control.stop".to_string()]))
            .sign_with_keys(&keys)
            .unwrap();

        let action = extract_action(&event);
        assert_eq!(action, Some("control.stop".to_string()));
    }

    #[test]
    fn extract_action_from_json() {
        let keys = Keys::generate();
        let content = r#"{"action": "config.get", "params": {"scope": "global"}}"#;
        let event = EventBuilder::new(Kind::Custom(1121), content)
            .sign_with_keys(&keys)
            .unwrap();

        let action = extract_action(&event);
        assert_eq!(action, Some("config.get".to_string()));
    }

    #[test]
    fn extract_action_params_from_tags() {
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(1121), "")
            .tag(Tag::custom(TagKind::custom("param:scope"), vec!["global".to_string()]))
            .tag(Tag::custom(TagKind::custom("param:mode"), vec!["all".to_string()]))
            .sign_with_keys(&keys)
            .unwrap();

        let params = extract_action_params(&event);
        assert_eq!(params.len(), 2);
        assert!(params.contains(&("scope".to_string(), "global".to_string())));
        assert!(params.contains(&("mode".to_string(), "all".to_string())));
    }

    #[test]
    fn targets_pubkey_check() {
        let keys1 = Keys::generate();
        let keys2 = Keys::generate();
        
        let event = EventBuilder::new(Kind::Custom(1121), "test action")
            .tag(Tag::custom(TagKind::custom("p"), vec![keys1.public_key().to_hex()]))
            .sign_with_keys(&keys2)
            .unwrap();

        assert!(targets_pubkey(&event, &keys1.public_key()));
        assert!(!targets_pubkey(&event, &keys2.public_key()));
    }
}