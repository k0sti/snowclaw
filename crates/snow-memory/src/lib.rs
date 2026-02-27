//! Collective memory types, ranking, and Nostr event schema for Snowclaw.
//!
//! This crate defines the core data structures for Snowclaw's layered
//! collective memory system. Memories are published as NIP-78 Nostr events
//! and ranked by source trust, model tier, and recency.

pub mod cache;
pub mod config;
pub mod event;
pub mod publish;
pub mod ranking;
pub mod search;
pub mod subscribe;
pub mod types;

pub use cache::MemoryCache;
pub use config::MemoryConfig;
pub use publish::{build_memory_event, build_profile_event, UnsignedEvent};
pub use ranking::{detect_conflicts, rank_memories, resolve_conflict, Conflict};
pub use search::SqliteMemoryIndex;
pub use subscribe::{parse_relay_message, EventDedup, RelayMessage};
pub use types::{AgentProfile, Memory, MemoryTier, SearchResult, SourcePreference};
