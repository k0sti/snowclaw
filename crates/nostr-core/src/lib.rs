//! Shared Nostr protocol functionality for Snowclaw
//!
//! This crate provides reusable components for Nostr protocol handling,
//! including relay clients, message context management, configuration,
//! and security filtering.

pub mod actions;
pub mod context;
pub mod key_filter;
pub mod memory;
pub mod mention;
pub mod relay;
pub mod respond;
pub mod ring_buffer;
pub mod tasks;

// Re-export commonly used types
pub use actions::{
    extract_action, extract_action_group, extract_action_params, targets_pubkey,
};
pub use context::{
    compact_group_header, compact_task_content, format_history_context, push_history,
    truncate_npub, HistoryMessage,
};
pub use key_filter::{log_flags, KeyFilter, KeyFilterMetrics, SecurityFlag, SecurityFlagKind};
pub use memory::{
    GroupMemory, NostrMemory, NostrMemoryStore, NpubMemory, ProfileMetadata,
};
pub use mention::{
    detect_mentions, extract_mentioned_pubkeys, is_mentioned, mentions_pubkey, 
    sanitize_content_preview, Mention, MentionType,
};
pub use relay::RelayClient;
pub use respond::{
    apply_config_entry, parse_config_event, respond_mode_for_group, DynamicConfig, GroupConfig,
    RespondMode,
};
pub use ring_buffer::{ConversationRingBuffer, GroupRingBuffer, MessageEntry};
pub use tasks::{build_task_metadata, is_task_status_kind, status_name_for_kind};

// Re-export nostr-sdk for convenience
pub use nostr_sdk;