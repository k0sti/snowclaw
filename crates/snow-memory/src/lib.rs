//! Collective memory types, ranking, and Nostr event schema for Snowclaw.
//!
//! This crate defines the core data structures for Snowclaw's layered
//! collective memory system. Memories are published as NIP-78 Nostr events
//! and ranked by source trust, model tier, and recency.

pub mod config;
pub mod event;
pub mod ranking;
pub mod types;

pub use config::MemoryConfig;
pub use ranking::{detect_conflicts, rank_memories, resolve_conflict, Conflict};
pub use types::{AgentProfile, Memory, MemoryTier, SearchResult, SourcePreference};
