//! Context and history management for Nostr conversations.

use std::collections::VecDeque;

/// A message stored in the per-group history ring buffer.
#[derive(Debug, Clone)]
pub struct HistoryMessage {
    pub sender: String,
    pub npub: String,
    pub content: String,
    pub timestamp: u64,
    pub event_id: String,
    pub is_guardian: bool,
}

/// Push a message to the history ring buffer for a group.
pub fn push_history(
    group_history: &mut std::collections::HashMap<String, VecDeque<HistoryMessage>>,
    group: &str,
    msg: HistoryMessage,
    context_history: usize,
) {
    let history = group_history.entry(group.to_string()).or_insert_with(VecDeque::new);
    history.push_back(msg);
    // Keep only the most recent N messages
    while history.len() > context_history {
        history.pop_front();
    }
}

/// Format conversation history as LLM context, excluding a specific event.
pub fn format_history_context(
    group_history: &std::collections::HashMap<String, VecDeque<HistoryMessage>>,
    group: &str,
    exclude_event_id: &str,
    context_history: usize,
) -> String {
    let Some(history) = group_history.get(group) else {
        return String::new();
    };

    let mut lines = Vec::new();
    let max_messages = context_history.min(history.len());
    let start_idx = history.len().saturating_sub(max_messages);
    
    for msg in history.iter().skip(start_idx) {
        if msg.event_id == exclude_event_id {
            continue;
        }

        let header = compact_group_header(
            group,
            &msg.sender,
            &msg.npub,
            9, // kind 9 for group messages
            &msg.event_id,
            msg.is_guardian,
        );
        
        let content = if msg.content.len() > 280 {
            format!("{}…", &msg.content[..280])
        } else {
            msg.content.clone()
        };
        
        lines.push(format!("{}\n{}", header, content));
    }

    if lines.is_empty() {
        String::new()
    } else {
        format!("## Recent Group History\n{}\n", lines.join("\n\n"))
    }
}

/// Build compact header for group events with emoji indicators.
pub fn compact_group_header(
    group: &str,
    sender: &str,
    npub: &str,
    kind: u16,
    event_id: &str,
    is_guardian: bool,
) -> String {
    let guardian_badge = if is_guardian { " 👑" } else { "" };
    let short_npub = truncate_npub(npub);
    let short_id = &event_id[..8.min(event_id.len())];
    
    match kind {
        9 => format!("💬 #{} {} ({}){}  [{}]", group, sender, short_npub, guardian_badge, short_id),
        11 => format!("👋 {} joined #{} ({}){}  [{}]", sender, group, short_npub, guardian_badge, short_id),
        12 => format!("👋 {} left #{} ({}){}  [{}]", sender, group, short_npub, guardian_badge, short_id),
        _ => format!("📝 #{} {} ({}){}  [kind {} | {}]", group, sender, short_npub, guardian_badge, kind, short_id),
    }
}

/// Build compact header for task status events.
pub fn compact_task_content(event_id: &str, task_ref: &str, status: &str, detail: &str) -> String {
    let short_id = &event_id[..8.min(event_id.len())];
    let short_task = &task_ref[..8.min(task_ref.len())];
    if detail.is_empty() {
        format!("🔧 Task {}: {} [{}]", short_task, status, short_id)
    } else {
        format!("🔧 Task {}: {} - {} [{}]", short_task, status, detail, short_id)
    }
}

/// Truncate npub for compact display (first 12 chars after 'npub1').
pub fn truncate_npub(npub: &str) -> &str {
    if npub.starts_with("npub1") && npub.len() > 16 {
        &npub[..16]
    } else if npub.len() > 12 {
        &npub[..12]
    } else {
        npub
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn compact_group_header_format() {
        let header = compact_group_header("test", "Alice", "npub1abc123", 9, "event123456", false);
        assert!(header.contains("💬 #test Alice"));
        assert!(header.contains("npub1abc123"));
        assert!(header.contains("[event123456]"));
    }

    #[test]
    fn compact_group_header_guardian() {
        let header = compact_group_header("test", "Guardian", "npub1xyz789", 9, "event789", true);
        assert!(header.contains("👑"));
        assert!(header.contains("Guardian"));
    }

    #[test]
    fn compact_task_content_with_detail() {
        let content = compact_task_content("event12345678", "task87654321", "Done", "Fixed the bug");
        assert_eq!(content, "🔧 Task task8765: Done - Fixed the bug [event123]");
    }

    #[test]
    fn compact_task_content_without_detail() {
        let content = compact_task_content("event12345678", "task87654321", "Executing", "");
        assert_eq!(content, "🔧 Task task8765: Executing [event123]");
    }

    #[test]
    fn format_history_excludes_event() {
        let mut history = HashMap::new();
        let mut group_history = VecDeque::new();
        
        group_history.push_back(HistoryMessage {
            sender: "Alice".to_string(),
            npub: "npub1abc".to_string(),
            content: "Hello".to_string(),
            timestamp: 1000,
            event_id: "event1".to_string(),
            is_guardian: false,
        });
        
        group_history.push_back(HistoryMessage {
            sender: "Bob".to_string(),
            npub: "npub1def".to_string(),
            content: "Hi there".to_string(),
            timestamp: 1001,
            event_id: "event2".to_string(),
            is_guardian: false,
        });
        
        history.insert("test".to_string(), group_history);
        
        let formatted = format_history_context(&history, "test", "event2", 10);
        assert!(formatted.contains("Alice"));
        assert!(!formatted.contains("Hi there")); // excluded
        assert!(formatted.contains("Hello"));
    }
}