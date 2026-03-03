//! Conversation ring buffer for maintaining recent message context per group.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A single message entry in the ring buffer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageEntry {
    pub author_pubkey: String,
    pub author_display_name: String,
    pub content_preview: String,
    pub timestamp: i64,
    pub event_id: String,
}

/// Ring buffer for a single group conversation
#[derive(Debug)]
pub struct GroupRingBuffer {
    messages: Vec<MessageEntry>,
    capacity: usize,
    next_index: usize,
    is_full: bool,
}

impl GroupRingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            messages: Vec::with_capacity(capacity),
            capacity,
            next_index: 0,
            is_full: false,
        }
    }

    /// Push a new message to the ring buffer
    pub fn push(&mut self, entry: MessageEntry) {
        if self.is_full {
            // Overwrite the oldest message
            self.messages[self.next_index] = entry;
        } else {
            // Add to the end until we reach capacity
            self.messages.push(entry);
        }

        self.next_index = (self.next_index + 1) % self.capacity;
        if self.next_index == 0 && !self.is_full {
            self.is_full = true;
        }
    }

    /// Get the last N messages as a compact summary
    pub fn get_context(&self, n: usize) -> Vec<MessageEntry> {
        if self.messages.is_empty() {
            return Vec::new();
        }

        let available = self.messages.len();
        let count = n.min(available);
        let mut result = Vec::with_capacity(count);

        if !self.is_full {
            // Buffer not full yet, take from the end
            let start = available.saturating_sub(count);
            result.extend_from_slice(&self.messages[start..]);
        } else {
            // Buffer is full, need to consider wraparound
            let start_index = if count >= self.capacity {
                self.next_index
            } else {
                (self.next_index + self.capacity - count) % self.capacity
            };

            for i in 0..count {
                let index = (start_index + i) % self.capacity;
                result.push(self.messages[index].clone());
            }
        }

        result
    }

    /// Get the total number of messages stored (up to capacity)
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Check if the buffer is empty
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

/// Conversation ring buffer manager for multiple groups
#[derive(Debug)]
pub struct ConversationRingBuffer {
    groups: Arc<RwLock<HashMap<String, GroupRingBuffer>>>,
    default_capacity: usize,
}

impl ConversationRingBuffer {
    /// Create a new conversation ring buffer with default capacity per group
    pub fn new(default_capacity: usize) -> Self {
        Self {
            groups: Arc::new(RwLock::new(HashMap::new())),
            default_capacity,
        }
    }

    /// Push a message to a specific group's ring buffer
    pub async fn push(&self, group: &str, entry: MessageEntry) {
        let mut groups = self.groups.write().await;
        let group_buffer = groups
            .entry(group.to_string())
            .or_insert_with(|| GroupRingBuffer::new(self.default_capacity));
        group_buffer.push(entry);
    }

    /// Get context for a specific group (last N messages)
    pub async fn get_context(&self, group: &str, n: usize) -> Vec<MessageEntry> {
        let groups = self.groups.read().await;
        groups
            .get(group)
            .map(|buffer| buffer.get_context(n))
            .unwrap_or_default()
    }

    /// Get all group names that have messages
    pub async fn get_groups(&self) -> Vec<String> {
        let groups = self.groups.read().await;
        groups.keys().cloned().collect()
    }

    /// Get message count for a specific group
    pub async fn get_group_message_count(&self, group: &str) -> usize {
        let groups = self.groups.read().await;
        groups
            .get(group)
            .map(|buffer| buffer.len())
            .unwrap_or(0)
    }

    /// Clear messages for a specific group
    pub async fn clear_group(&self, group: &str) {
        let mut groups = self.groups.write().await;
        groups.remove(group);
    }

    /// Clear all groups
    pub async fn clear_all(&self) {
        let mut groups = self.groups.write().await;
        groups.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_group_ring_buffer_basic() {
        let mut buffer = GroupRingBuffer::new(3);
        
        // Test empty
        assert!(buffer.is_empty());
        assert_eq!(buffer.len(), 0);
        assert!(buffer.get_context(5).is_empty());

        // Add first message
        buffer.push(MessageEntry {
            author_pubkey: "author1".to_string(),
            author_display_name: "Alice".to_string(),
            content_preview: "Hello world".to_string(),
            timestamp: 1000,
            event_id: "event1".to_string(),
        });

        assert!(!buffer.is_empty());
        assert_eq!(buffer.len(), 1);
        let ctx = buffer.get_context(2);
        assert_eq!(ctx.len(), 1);
        assert_eq!(ctx[0].content_preview, "Hello world");
    }

    #[test]
    fn test_group_ring_buffer_wraparound() {
        let mut buffer = GroupRingBuffer::new(2);
        
        // Fill buffer
        buffer.push(MessageEntry {
            author_pubkey: "author1".to_string(),
            author_display_name: "Alice".to_string(),
            content_preview: "Message 1".to_string(),
            timestamp: 1000,
            event_id: "event1".to_string(),
        });
        
        buffer.push(MessageEntry {
            author_pubkey: "author2".to_string(),
            author_display_name: "Bob".to_string(),
            content_preview: "Message 2".to_string(),
            timestamp: 2000,
            event_id: "event2".to_string(),
        });

        // Now add a third message (should overwrite first)
        buffer.push(MessageEntry {
            author_pubkey: "author3".to_string(),
            author_display_name: "Charlie".to_string(),
            content_preview: "Message 3".to_string(),
            timestamp: 3000,
            event_id: "event3".to_string(),
        });

        let ctx = buffer.get_context(3);
        assert_eq!(ctx.len(), 2);
        assert_eq!(ctx[0].content_preview, "Message 2");
        assert_eq!(ctx[1].content_preview, "Message 3");
    }

    #[tokio::test]
    async fn test_conversation_ring_buffer() {
        let crb = ConversationRingBuffer::new(3);
        
        // Add messages to different groups
        crb.push("group1", MessageEntry {
            author_pubkey: "author1".to_string(),
            author_display_name: "Alice".to_string(),
            content_preview: "Group 1 message".to_string(),
            timestamp: 1000,
            event_id: "event1".to_string(),
        }).await;

        crb.push("group2", MessageEntry {
            author_pubkey: "author2".to_string(),
            author_display_name: "Bob".to_string(),
            content_preview: "Group 2 message".to_string(),
            timestamp: 2000,
            event_id: "event2".to_string(),
        }).await;

        // Test context retrieval
        let ctx1 = crb.get_context("group1", 5).await;
        assert_eq!(ctx1.len(), 1);
        assert_eq!(ctx1[0].content_preview, "Group 1 message");

        let ctx2 = crb.get_context("group2", 5).await;
        assert_eq!(ctx2.len(), 1);
        assert_eq!(ctx2[0].content_preview, "Group 2 message");

        // Test groups list
        let groups = crb.get_groups().await;
        assert_eq!(groups.len(), 2);
        assert!(groups.contains(&"group1".to_string()));
        assert!(groups.contains(&"group2".to_string()));
    }
}