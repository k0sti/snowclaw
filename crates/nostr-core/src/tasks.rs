//! Task status event formatting (kinds 1630-1637).

/// Get the status name for a task event kind.
pub fn status_name_for_kind(kind: u16) -> &'static str {
    match kind {
        1630 => "Queued",
        1631 => "Done",
        1632 => "Cancelled",
        1633 => "Draft",
        1634 => "Executing",
        1635 => "Blocked",
        1636 => "Review",
        1637 => "Failed",
        _ => "Unknown",
    }
}

/// Check if a kind is a task status event.
pub fn is_task_status_kind(kind: u16) -> bool {
    (1630..=1637).contains(&kind)
}

/// Build metadata for task status events.
pub fn build_task_metadata(
    event: &nostr_sdk::Event,
    group: Option<&str>,
    is_owner: bool,
) -> std::collections::HashMap<String, String> {
    let mut meta = std::collections::HashMap::new();
    
    meta.insert("nostr_event_id".to_string(), event.id.to_hex());
    meta.insert("nostr_pubkey".to_string(), event.pubkey.to_hex());
    meta.insert("nostr_kind".to_string(), event.kind.as_u16().to_string());
    meta.insert("nostr_timestamp".to_string(), event.created_at.as_secs().to_string());
    meta.insert("nostr_task_status".to_string(), status_name_for_kind(event.kind.as_u16()).to_string());
    
    if let Some(g) = group {
        meta.insert("nostr_group".to_string(), g.to_string());
    }
    if is_owner {
        meta.insert("nostr_is_owner".to_string(), "true".to_string());
    }
    
    // Extract task reference from event tags
    for tag in event.tags.iter() {
        let s = tag.as_slice();
        if s.first().map(|v| v.as_str()) == Some("e") {
            if let Some(task_ref) = s.get(1) {
                meta.insert("nostr_task_ref".to_string(), task_ref.to_string());
                break;
            }
        }
    }
    
    meta
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::*;

    #[test]
    fn status_name_mapping() {
        assert_eq!(status_name_for_kind(1630), "Queued");
        assert_eq!(status_name_for_kind(1631), "Done");
        assert_eq!(status_name_for_kind(1632), "Cancelled");
        assert_eq!(status_name_for_kind(1637), "Failed");
        assert_eq!(status_name_for_kind(9999), "Unknown");
    }

    #[test]
    fn is_task_status_check() {
        assert!(is_task_status_kind(1630));
        assert!(is_task_status_kind(1637));
        assert!(!is_task_status_kind(1629));
        assert!(!is_task_status_kind(1638));
        assert!(!is_task_status_kind(9));
    }

    #[test]
    fn build_task_metadata_includes_fields() {
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(1631), "task completed")
            .tag(Tag::custom(TagKind::custom("e"), vec!["task123".to_string()]))
            .sign_with_keys(&keys)
            .unwrap();

        let meta = build_task_metadata(&event, Some("techteam"), true);
        
        assert_eq!(meta.get("nostr_kind").unwrap(), "1631");
        assert_eq!(meta.get("nostr_task_status").unwrap(), "Done");
        assert_eq!(meta.get("nostr_group").unwrap(), "techteam");
        assert_eq!(meta.get("nostr_is_owner").unwrap(), "true");
        assert_eq!(meta.get("nostr_task_ref").unwrap(), "task123");
        assert!(meta.contains_key("nostr_event_id"));
        assert!(meta.contains_key("nostr_pubkey"));
        assert!(meta.contains_key("nostr_timestamp"));
    }
}