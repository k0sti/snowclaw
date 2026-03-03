use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use snow_memory::types::MemoryTier;

/// A single memory entry
#[derive(Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub key: String,
    pub content: String,
    pub category: MemoryCategory,
    pub timestamp: String,
    pub session_id: Option<String>,
    pub score: Option<f64>,
}

impl std::fmt::Debug for MemoryEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryEntry")
            .field("id", &self.id)
            .field("key", &self.key)
            .field("content", &self.content)
            .field("category", &self.category)
            .field("timestamp", &self.timestamp)
            .field("score", &self.score)
            .finish_non_exhaustive()
    }
}

/// Memory categories for organization
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCategory {
    /// Long-term facts, preferences, decisions
    Core,
    /// Daily session logs
    Daily,
    /// Conversation context
    Conversation,
    /// User-defined custom category
    Custom(String),
}

impl std::fmt::Display for MemoryCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Core => write!(f, "core"),
            Self::Daily => write!(f, "daily"),
            Self::Conversation => write!(f, "conversation"),
            Self::Custom(name) => write!(f, "{name}"),
        }
    }
}

/// Context for tier-aware recall filtering.
///
/// Determines which privacy tiers of memories are visible based on
/// the current session/channel context.
#[derive(Debug, Clone)]
pub struct RecallContext {
    /// True when this is the main/DM session with the guardian.
    pub is_main_session: bool,
    /// Channel name (e.g. "nostr", "telegram").
    pub channel: Option<String>,
    /// NIP-29 group id, if in a group chat.
    pub group_id: Option<String>,
}

/// Core memory trait — implement for any persistence backend
#[async_trait]
pub trait Memory: Send + Sync {
    /// Backend name
    fn name(&self) -> &str;

    /// Store a memory entry, optionally scoped to a session
    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()>;

    /// Recall memories matching a query (keyword search), optionally scoped to a session.
    ///
    /// When `context` is provided, results are filtered by privacy tier:
    /// - Main session: Public + Private + matching Group
    /// - Group chat: Public + matching Group only
    /// - Other: Public only
    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        context: Option<&RecallContext>,
    ) -> anyhow::Result<Vec<MemoryEntry>>;

    /// Get a specific memory by key
    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>>;

    /// List all memory keys, optionally filtered by category and/or session
    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>>;

    /// Remove a memory by key
    async fn forget(&self, key: &str) -> anyhow::Result<bool>;

    /// Count total memories
    async fn count(&self) -> anyhow::Result<usize>;

    /// Store a memory with an explicit privacy tier.
    ///
    /// Default implementation ignores the tier and delegates to `store()`.
    /// Backends that support tiered storage (e.g. `CollectiveMemory`) override this.
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
        anyhow::bail!("{} backend does not support memory promotion", self.name())
    }

    /// Health check
    async fn health_check(&self) -> bool;

    /// Rebuild embeddings for all memories using the current embedding provider.
    /// Returns the number of memories reindexed, or an error if not supported.
    ///
    /// Use this after changing the embedding model to ensure vector search
    /// works correctly with the new embeddings.
    async fn reindex(
        &self,
        progress_callback: Option<Box<dyn Fn(usize, usize) + Send + Sync>>,
    ) -> anyhow::Result<usize> {
        let _ = progress_callback;
        anyhow::bail!("Reindex not supported by {} backend", self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_category_display_outputs_expected_values() {
        assert_eq!(MemoryCategory::Core.to_string(), "core");
        assert_eq!(MemoryCategory::Daily.to_string(), "daily");
        assert_eq!(MemoryCategory::Conversation.to_string(), "conversation");
        assert_eq!(
            MemoryCategory::Custom("project_notes".into()).to_string(),
            "project_notes"
        );
    }

    #[test]
    fn memory_category_serde_uses_snake_case() {
        let core = serde_json::to_string(&MemoryCategory::Core).unwrap();
        let daily = serde_json::to_string(&MemoryCategory::Daily).unwrap();
        let conversation = serde_json::to_string(&MemoryCategory::Conversation).unwrap();

        assert_eq!(core, "\"core\"");
        assert_eq!(daily, "\"daily\"");
        assert_eq!(conversation, "\"conversation\"");
    }

    #[test]
    fn memory_entry_roundtrip_preserves_optional_fields() {
        let entry = MemoryEntry {
            id: "id-1".into(),
            key: "favorite_language".into(),
            content: "Rust".into(),
            category: MemoryCategory::Core,
            timestamp: "2026-02-16T00:00:00Z".into(),
            session_id: Some("session-abc".into()),
            score: Some(0.98),
        };

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: MemoryEntry = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.id, "id-1");
        assert_eq!(parsed.key, "favorite_language");
        assert_eq!(parsed.content, "Rust");
        assert_eq!(parsed.category, MemoryCategory::Core);
        assert_eq!(parsed.session_id.as_deref(), Some("session-abc"));
        assert_eq!(parsed.score, Some(0.98));
    }
}
