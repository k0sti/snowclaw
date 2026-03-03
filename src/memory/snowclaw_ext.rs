//! Snowclaw-specific memory extensions.
//!
//! These are extensions to the upstream `Memory` trait that support
//! tier-aware storage and privacy-scoped recall used by the collective
//! memory backend.
//!
//! By keeping these in a separate extension trait with a blanket impl,
//! `src/memory/traits.rs` stays identical to upstream and new upstream
//! Memory implementations merge with zero conflicts.

use super::traits::{Memory, MemoryCategory};
use async_trait::async_trait;
use snow_memory::types::MemoryTier;

/// Context for tier-aware recall filtering.
///
/// Determines which privacy tiers of memories are visible based on
/// the current session/channel context. Used by `CollectiveMemory`'s
/// `recall_with_context()` inherent method.
#[derive(Debug, Clone)]
pub struct RecallContext {
    /// True when this is the main/DM session with the guardian.
    pub is_main_session: bool,
    /// Channel name (e.g. "nostr", "telegram").
    pub channel: Option<String>,
    /// NIP-29 group id, if in a group chat.
    pub group_id: Option<String>,
}

/// Snowclaw-specific extensions to the `Memory` trait.
///
/// Provides tiered storage and promotion via a blanket impl.
/// Default methods delegate to the base `Memory` trait, ignoring
/// the extra parameters.
///
/// Note: context-aware recall (`recall_with_context`) lives as an
/// inherent method on `CollectiveMemory` since only that backend
/// needs real tier filtering.
#[async_trait]
pub trait SnowclawMemoryExt: Memory {
    /// Store a memory with an explicit privacy tier.
    ///
    /// Default implementation ignores the tier and delegates to `store()`.
    async fn store_with_tier(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        _tier: MemoryTier,
    ) -> anyhow::Result<()> {
        self.store(key, content, category, None).await
    }

    /// Promote a memory to a higher visibility tier.
    ///
    /// Tier direction: Private -> Group -> Public. Demotions are rejected.
    /// Default implementation returns an error for backends that don't support promotion.
    async fn promote(&self, _key: &str, _new_tier: MemoryTier) -> anyhow::Result<()> {
        anyhow::bail!(
            "{} backend does not support memory promotion",
            self.name()
        )
    }
}

/// Blanket implementation: every `Memory` implementor gets the default
/// (no-op) versions of the snowclaw extensions automatically.
impl<T: Memory + ?Sized> SnowclawMemoryExt for T {}
