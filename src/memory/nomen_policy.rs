//! Policy helpers for Snowclaw ↔ Nomen compatibility mapping.
//!
//! This module intentionally keeps policy small and explicit.
//! The current `Memory` trait only exposes `(key, content, category, session_id)`.
//! That is not enough to derive canonical Nomen scope/channel in the general case,
//! so we only use context that is actually present and avoid inventing semantics.

use super::traits::MemoryCategory;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorePolicy {
    /// Tier/visibility string passed into Nomen's current compatibility surface.
    pub tier: String,
    /// Best-effort channel hint extracted from host context, for diagnostics only.
    pub channel_hint: Option<String>,
    /// Why we had to fall back instead of deriving canonical Nomen scope/channel.
    pub limitation: Option<&'static str>,
}

const LIMITATION_TRAIT_SURFACE: &str =
    "Snowclaw Memory::store lacks canonical runtime scope/channel context; session_id is only treated as an opaque compatibility hint";

/// Resolve the best available Nomen write policy from the legacy compatibility inputs.
///
/// Rules:
/// - Canonical Nomen visibility values already present in `custom(...)` are honored.
/// - Scoped forms like `group:<scope>` / `circle:<scope>` are honored.
/// - Plain `group` / `circle` are not treated as canonical because scope is missing.
/// - `session_id` is never reinterpreted as canonical scope; it can only provide a
///   best-effort provider/channel hint for diagnostics.
/// - When the trait surface does not provide enough information, we fall back to a
///   conservative personal tier and record the limitation explicitly.
pub fn resolve_store_policy(category: &MemoryCategory, session_id: Option<&str>) -> StorePolicy {
    let channel_hint = channel_hint_from_session_id(session_id);

    if let Some(tier) = canonical_tier_from_category(category) {
        return StorePolicy {
            tier,
            channel_hint,
            limitation: None,
        };
    }

    StorePolicy {
        tier: "personal".to_string(),
        channel_hint,
        limitation: Some(LIMITATION_TRAIT_SURFACE),
    }
}

fn canonical_tier_from_category(category: &MemoryCategory) -> Option<String> {
    match category {
        MemoryCategory::Custom(name) => canonicalize_custom_tier(name),
        MemoryCategory::Core | MemoryCategory::Daily | MemoryCategory::Conversation => None,
    }
}

fn canonicalize_custom_tier(name: &str) -> Option<String> {
    let normalized = name.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "public" | "personal" | "internal" => Some(normalized),
        _ if has_scoped_prefix(&normalized, "group:") => Some(normalized),
        _ if has_scoped_prefix(&normalized, "circle:") => Some(normalized),
        _ => None,
    }
}

fn has_scoped_prefix(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(prefix)
        .map(|rest| !rest.trim().is_empty())
        .unwrap_or(false)
}

fn channel_hint_from_session_id(session_id: Option<&str>) -> Option<String> {
    let sid = session_id?.trim();
    if sid.is_empty() {
        return None;
    }

    let sid_lower = sid.to_ascii_lowercase();
    if sid_lower == "sess"
        || sid_lower == "session"
        || sid_lower.starts_with("sess-")
        || sid_lower.starts_with("session-")
    {
        return None;
    }

    let prefix = sid
        .split([':', '_'])
        .next()
        .map(str::trim)
        .filter(|part| !part.is_empty())?;

    if prefix.eq_ignore_ascii_case("sess") || prefix.eq_ignore_ascii_case("session") {
        return None;
    }

    Some(prefix.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn honors_canonical_visibility_from_custom_category() {
        let policy = resolve_store_policy(&MemoryCategory::Custom("public".into()), None);
        assert_eq!(policy.tier, "public");
        assert_eq!(policy.limitation, None);
    }

    #[test]
    fn honors_scoped_group_category() {
        let policy = resolve_store_policy(
            &MemoryCategory::Custom("group:techteam".into()),
            Some("telegram_-1003821690204_k0"),
        );
        assert_eq!(policy.tier, "group:techteam");
        assert_eq!(policy.channel_hint.as_deref(), Some("telegram"));
        assert_eq!(policy.limitation, None);
    }

    #[test]
    fn plain_group_without_scope_is_not_treated_as_canonical() {
        let policy = resolve_store_policy(&MemoryCategory::Custom("group".into()), None);
        assert_eq!(policy.tier, "personal");
        assert!(policy.limitation.is_some());
    }

    #[test]
    fn core_category_falls_back_with_explicit_limitation() {
        let policy = resolve_store_policy(&MemoryCategory::Core, Some("sess-42"));
        assert_eq!(policy.tier, "personal");
        assert_eq!(policy.channel_hint, None);
        assert!(policy
            .limitation
            .unwrap()
            .contains("lacks canonical runtime scope/channel context"));
    }

    #[test]
    fn extracts_only_best_effort_channel_hint_from_session_id() {
        let policy =
            resolve_store_policy(&MemoryCategory::Conversation, Some("discord_thread_123"));
        assert_eq!(policy.channel_hint.as_deref(), Some("discord"));
        assert_eq!(policy.tier, "personal");
    }
}
