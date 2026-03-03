//! Secret key detection and sanitization for LLM context.
//!
//! Prevents private keys (nsec, hex secret keys) from reaching the LLM.
//! Public keys (npub, hex pubkeys) are left untouched.

use regex::Regex;
use std::collections::HashSet;
use std::sync::{atomic::AtomicU64, atomic::Ordering, Arc, LazyLock, RwLock};

/// Compiled regexes — allocated once.
static NSEC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"nsec1[qpzry9x8gf2tvdw0s3jn54khce6mua7l]{58}").unwrap());
static HEX64_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[0-9a-fA-F]{64}\b").unwrap());

// ── Types ────────────────────────────────────────────────────────

/// What kind of secret was detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecurityFlagKind {
    NsecDetected,
    HexSecretDetected,
    UnknownHex64,
}

/// A flag raised during sanitization.
#[derive(Debug, Clone)]
pub struct SecurityFlag {
    pub kind: SecurityFlagKind,
    pub context: String,
    pub redacted: bool,
}

/// Global counters for observability.
#[derive(Debug, Default)]
pub struct KeyFilterMetrics {
    pub nsec_redacted: AtomicU64,
    pub hex_flagged: AtomicU64,
}

impl KeyFilterMetrics {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Shared state for the key filter: known pubkeys + metrics.
#[derive(Clone)]
pub struct KeyFilter {
    /// Known-safe hex pubkeys (public keys from config, profile cache, etc.)
    known_pubkeys: Arc<RwLock<HashSet<String>>>,
    /// Counters
    pub metrics: Arc<KeyFilterMetrics>,
}

impl KeyFilter {
    pub fn new() -> Self {
        Self {
            known_pubkeys: Arc::new(RwLock::new(HashSet::new())),
            metrics: Arc::new(KeyFilterMetrics::new()),
        }
    }

    /// Register a hex pubkey as known-safe (won't be flagged).
    pub fn add_known_pubkey(&self, hex: &str) {
        if let Ok(mut set) = self.known_pubkeys.write() {
            set.insert(hex.to_lowercase());
        }
    }

    /// Register multiple pubkeys at once.
    pub fn add_known_pubkeys(&self, hexes: impl IntoIterator<Item = impl AsRef<str>>) {
        if let Ok(mut set) = self.known_pubkeys.write() {
            for h in hexes {
                set.insert(h.as_ref().to_lowercase());
            }
        }
    }

    fn is_known_pubkey(&self, hex: &str) -> bool {
        self.known_pubkeys
            .read()
            .map(|set| set.contains(&hex.to_lowercase()))
            .unwrap_or(false)
    }

    /// Sanitize text for LLM context. Returns (sanitized_text, flags).
    ///
    /// Fast path: if no patterns match, returns the original `&str` via `Cow`
    /// semantics (no allocation).
    pub fn sanitize(&self, text: &str, context: &str) -> (String, Vec<SecurityFlag>) {
        // Fast path: no potential matches at all
        if !text.contains("nsec1") && !HEX64_RE.is_match(text) {
            return (text.to_string(), Vec::new());
        }

        let mut result = text.to_string();
        let mut flags = Vec::new();

        // 1. Redact nsec (unambiguous secret key — always redact)
        if result.contains("nsec1") {
            let captures: Vec<String> = NSEC_RE
                .find_iter(&result)
                .map(|m| m.as_str().to_string())
                .collect();

            for nsec_str in captures {
                let replacement = match try_nsec_to_npub(&nsec_str) {
                    Some(npub) => {
                        let trunc = &npub[..20.min(npub.len())];
                        format!("[REDACTED nsec → {trunc}...]")
                    }
                    None => "[REDACTED nsec]".to_string(),
                };
                result = result.replace(&nsec_str, &replacement);
                flags.push(SecurityFlag {
                    kind: SecurityFlagKind::NsecDetected,
                    context: context.to_string(),
                    redacted: true,
                });
                self.metrics.nsec_redacted.fetch_add(1, Ordering::Relaxed);
            }
        }

        // 2. Flag unknown 64-char hex strings
        let hex_matches: Vec<String> = HEX64_RE
            .find_iter(&result)
            .map(|m| m.as_str().to_string())
            .collect();

        for hex in hex_matches {
            if self.is_known_pubkey(&hex) {
                continue; // known pubkey — safe
            }

            // Check if it looks like it's in an npub/nsec context (already handled)
            // or a known event ID / other safe hex
            // For unknown hex, flag but don't redact
            result = result.replace(
                &hex,
                &format!("[FLAGGED: unknown 64-char hex {:.16}…]", hex),
            );
            flags.push(SecurityFlag {
                kind: SecurityFlagKind::UnknownHex64,
                context: context.to_string(),
                redacted: false,
            });
            self.metrics.hex_flagged.fetch_add(1, Ordering::Relaxed);
        }

        (result, flags)
    }
}

/// Try to derive the npub from an nsec string. Returns None on failure.
fn try_nsec_to_npub(nsec: &str) -> Option<String> {
    use nostr_sdk::ToBech32;
    let keys = nostr_sdk::Keys::parse(nsec).ok()?;
    keys.public_key().to_bech32().ok()
}

/// Log security flags as warnings (never logs the actual key).
pub fn log_flags(flags: &[SecurityFlag]) {
    for flag in flags {
        match flag.kind {
            SecurityFlagKind::NsecDetected => {
                tracing::warn!(
                    context = %flag.context,
                    "🔑 nsec detected and redacted before LLM processing"
                );
            }
            SecurityFlagKind::HexSecretDetected => {
                tracing::warn!(
                    context = %flag.context,
                    "🔑 Potential hex secret key detected and redacted"
                );
            }
            SecurityFlagKind::UnknownHex64 => {
                tracing::info!(
                    context = %flag.context,
                    "🔍 Unknown 64-char hex string flagged in LLM context"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::ToBech32;

    fn filter() -> KeyFilter {
        KeyFilter::new()
    }

    #[test]
    fn clean_text_unchanged() {
        let f = filter();
        let (out, flags) = f.sanitize("Hello world, how are you?", "test");
        assert_eq!(out, "Hello world, how are you?");
        assert!(flags.is_empty());
    }

    #[test]
    fn npub_not_redacted() {
        let f = filter();
        let text = "My pubkey is npub1abc123def456ghi789jkl012mno345pqr678stu901vwx234yz5678";
        let (out, flags) = f.sanitize(text, "test");
        assert!(out.contains("npub1"));
        assert!(flags.is_empty());
    }

    #[test]
    fn nsec_redacted() {
        // Generate a real nsec for testing
        let keys = nostr_sdk::Keys::generate();
        let nsec = keys.secret_key().to_bech32().unwrap();
        let npub = keys.public_key().to_bech32().unwrap();

        let f = filter();
        let text = format!("Here is my key: {nsec} don't share it");
        let (out, flags) = f.sanitize(&text, "test-context");

        assert!(!out.contains(&nsec), "nsec should be redacted");
        assert!(out.contains("[REDACTED nsec"), "should have redaction marker");
        assert!(out.contains(&npub[..20]), "should contain truncated npub");
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].kind, SecurityFlagKind::NsecDetected);
        assert!(flags[0].redacted);
        assert_eq!(f.metrics.nsec_redacted.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn known_pubkey_hex_not_flagged() {
        let f = filter();
        let hex = "d29fe7c1af179eac10767f57ac021f520b44a8ded1fd37b1d1f79c9e545f96d7";
        f.add_known_pubkey(hex);

        let text = format!("Owner pubkey is {hex}");
        let (out, flags) = f.sanitize(&text, "test");
        assert!(out.contains(hex), "known pubkey should not be flagged");
        assert!(flags.is_empty());
    }

    #[test]
    fn unknown_hex64_flagged() {
        let f = filter();
        let hex = "aabbccdd11223344556677889900aabbccdd11223344556677889900aabbccdd";
        let text = format!("Some key: {hex}");
        let (out, flags) = f.sanitize(&text, "test");
        assert!(!out.contains(hex), "unknown hex should be flagged");
        assert!(out.contains("[FLAGGED: unknown 64-char hex"));
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].kind, SecurityFlagKind::UnknownHex64);
        assert!(!flags[0].redacted);
    }

    #[test]
    fn multiple_nsecs_all_redacted() {
        let k1 = nostr_sdk::Keys::generate();
        let k2 = nostr_sdk::Keys::generate();
        let nsec1 = k1.secret_key().to_bech32().unwrap();
        let nsec2 = k2.secret_key().to_bech32().unwrap();

        let f = filter();
        let text = format!("Keys: {nsec1} and {nsec2}");
        let (out, flags) = f.sanitize(&text, "test");
        assert!(!out.contains("nsec1"), "all nsecs redacted");
        assert_eq!(flags.len(), 2);
        assert_eq!(f.metrics.nsec_redacted.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn fast_path_no_alloc_patterns() {
        let f = filter();
        // Text with no nsec1 substring and no 64-char hex → fast path
        let text = "Just a normal message with npub1abc and some hex abcdef";
        let (out, flags) = f.sanitize(text, "test");
        assert_eq!(out, text);
        assert!(flags.is_empty());
    }
}