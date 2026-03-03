//! Snowclaw-specific memory backend extensions.
//!
//! Keeps `backend.rs` identical to upstream by isolating the Nostr and
//! Collective backend variants, profiles, and factory logic here.

use super::backend::{MemoryBackendKind, MemoryBackendProfile};

/// Snowclaw-only backend kinds that extend upstream's `MemoryBackendKind`.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SnowclawBackendKind {
    /// An upstream backend — delegate to `MemoryBackendKind` matching.
    Upstream(MemoryBackendKind),
    /// Nostr — NIP-78 relay storage with local SQLite semantic search.
    Nostr,
    /// Collective — snow-memory FTS5 index with trust-ranked search.
    Collective,
}

const NOSTR_PROFILE: MemoryBackendProfile = MemoryBackendProfile {
    key: "nostr",
    label: "Nostr — NIP-78 relay storage with local SQLite semantic search",
    auto_save_default: true,
    uses_sqlite_hygiene: false,
    sqlite_based: false,
    optional_dependency: false,
};

const COLLECTIVE_PROFILE: MemoryBackendProfile = MemoryBackendProfile {
    key: "collective",
    label: "Collective — snow-memory FTS5 index with trust-ranked search",
    auto_save_default: true,
    uses_sqlite_hygiene: false,
    sqlite_based: false,
    optional_dependency: false,
};

/// Classify a backend name, checking snowclaw-specific backends first.
pub fn classify(name: &str) -> SnowclawBackendKind {
    match name {
        "nostr" => SnowclawBackendKind::Nostr,
        "collective" => SnowclawBackendKind::Collective,
        other => SnowclawBackendKind::Upstream(super::backend::classify_memory_backend(other)),
    }
}

/// Return the profile for a snowclaw-specific backend, or `None` for upstream backends.
pub fn snowclaw_backend_profile(name: &str) -> Option<MemoryBackendProfile> {
    match classify(name) {
        SnowclawBackendKind::Nostr => Some(NOSTR_PROFILE),
        SnowclawBackendKind::Collective => Some(COLLECTIVE_PROFILE),
        SnowclawBackendKind::Upstream(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_snowclaw_backends() {
        assert_eq!(classify("nostr"), SnowclawBackendKind::Nostr);
        assert_eq!(classify("collective"), SnowclawBackendKind::Collective);
    }

    #[test]
    fn classify_upstream_backends_pass_through() {
        assert_eq!(
            classify("sqlite"),
            SnowclawBackendKind::Upstream(MemoryBackendKind::Sqlite)
        );
        assert_eq!(
            classify("markdown"),
            SnowclawBackendKind::Upstream(MemoryBackendKind::Markdown)
        );
        assert_eq!(
            classify("redis"),
            SnowclawBackendKind::Upstream(MemoryBackendKind::Unknown)
        );
    }

    #[test]
    fn snowclaw_profiles_have_expected_keys() {
        assert_eq!(
            snowclaw_backend_profile("nostr").unwrap().key,
            "nostr"
        );
        assert_eq!(
            snowclaw_backend_profile("collective").unwrap().key,
            "collective"
        );
        assert!(snowclaw_backend_profile("sqlite").is_none());
    }
}
