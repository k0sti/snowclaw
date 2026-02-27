//! Subscribe to memory events from Nostr relays.
//!
//! Handles parsing incoming relay messages into Memory structs.
//! Actual WebSocket transport is handled by the caller.

use crate::event;
use crate::types::Memory;
use std::collections::HashSet;

/// Tracks seen event IDs for deduplication.
pub struct EventDedup {
    seen: HashSet<String>,
    max_size: usize,
}

impl EventDedup {
    pub fn new(max_size: usize) -> Self {
        Self {
            seen: HashSet::new(),
            max_size,
        }
    }

    /// Returns true if the event is new (not seen before).
    pub fn check_and_insert(&mut self, event_id: &str) -> bool {
        if self.seen.contains(event_id) {
            return false;
        }

        // Evict oldest if at capacity (simple: just clear half)
        if self.seen.len() >= self.max_size {
            let to_remove: Vec<String> = self.seen.iter().take(self.max_size / 2).cloned().collect();
            for id in to_remove {
                self.seen.remove(&id);
            }
        }

        self.seen.insert(event_id.to_string());
        true
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

/// Parse a relay message and extract a Memory if it's a valid snow: memory event.
///
/// Relay messages are JSON arrays like:
/// - `["EVENT", <sub_id>, <event>]`
/// - `["EOSE", <sub_id>]`
/// - `["NOTICE", <message>]`
pub fn parse_relay_message(msg: &str) -> RelayMessage {
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(msg);
    let parsed = match parsed {
        Ok(v) => v,
        Err(_) => return RelayMessage::Unknown(msg.to_string()),
    };

    let arr = match parsed.as_array() {
        Some(a) => a,
        None => return RelayMessage::Unknown(msg.to_string()),
    };

    match arr.first().and_then(|v| v.as_str()) {
        Some("EVENT") => {
            if arr.len() < 3 {
                return RelayMessage::Unknown(msg.to_string());
            }
            let sub_id = arr[1].as_str().unwrap_or("").to_string();
            let event = &arr[2];

            // Check if it's a snow: memory event (kind 30078 with snow:memory d-tag)
            let kind = event.get("kind").and_then(|k| k.as_u64()).unwrap_or(0);

            if kind == 30078 {
                match event::event_json_to_memory(event) {
                    Some(memory) => RelayMessage::MemoryEvent { sub_id, memory },
                    None => RelayMessage::OtherEvent {
                        sub_id,
                        kind: kind as u32,
                    },
                }
            } else if kind == 0 {
                RelayMessage::ProfileEvent {
                    sub_id,
                    event_json: event.to_string(),
                }
            } else {
                RelayMessage::OtherEvent {
                    sub_id,
                    kind: kind as u32,
                }
            }
        }
        Some("EOSE") => {
            let sub_id = arr.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
            RelayMessage::EndOfStoredEvents { sub_id }
        }
        Some("NOTICE") => {
            let message = arr.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
            RelayMessage::Notice { message }
        }
        Some("OK") => {
            let event_id = arr.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let accepted = arr.get(2).and_then(|v| v.as_bool()).unwrap_or(false);
            let message = arr.get(3).and_then(|v| v.as_str()).unwrap_or("").to_string();
            RelayMessage::Ok {
                event_id,
                accepted,
                message,
            }
        }
        _ => RelayMessage::Unknown(msg.to_string()),
    }
}

/// Parsed relay message types.
#[derive(Debug, Clone)]
pub enum RelayMessage {
    /// A snow: memory event was received.
    MemoryEvent {
        sub_id: String,
        memory: Memory,
    },
    /// A kind 0 profile event (may contain agent metadata).
    ProfileEvent {
        sub_id: String,
        event_json: String,
    },
    /// Non-memory event.
    OtherEvent {
        sub_id: String,
        kind: u32,
    },
    /// End of stored events — real-time events follow.
    EndOfStoredEvents {
        sub_id: String,
    },
    /// Relay notice.
    Notice {
        message: String,
    },
    /// Event acceptance confirmation.
    Ok {
        event_id: String,
        accepted: bool,
        message: String,
    },
    /// Unparseable message.
    Unknown(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dedup() {
        let mut dedup = EventDedup::new(100);
        assert!(dedup.check_and_insert("aaa"));
        assert!(!dedup.check_and_insert("aaa")); // duplicate
        assert!(dedup.check_and_insert("bbb"));
        assert_eq!(dedup.len(), 2);
    }

    #[test]
    fn test_parse_eose() {
        let msg = r#"["EOSE","sub1"]"#;
        match parse_relay_message(msg) {
            RelayMessage::EndOfStoredEvents { sub_id } => assert_eq!(sub_id, "sub1"),
            other => panic!("Expected EOSE, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_notice() {
        let msg = r#"["NOTICE","rate limited"]"#;
        match parse_relay_message(msg) {
            RelayMessage::Notice { message } => assert_eq!(message, "rate limited"),
            other => panic!("Expected Notice, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_ok() {
        let msg = r#"["OK","abc123",true,""]"#;
        match parse_relay_message(msg) {
            RelayMessage::Ok { event_id, accepted, .. } => {
                assert_eq!(event_id, "abc123");
                assert!(accepted);
            }
            other => panic!("Expected Ok, got {:?}", other),
        }
    }
}
