//! Mention detection logic for Nostr events.

use nostr_sdk::{Event, PublicKey, ToBech32};

/// Check if an event mentions a given pubkey (by name, npub, or is a reply to their event).
pub fn is_mentioned(
    event: &Event,
    our_pubkey: &PublicKey,
    mention_names: &[String],
) -> bool {
    // Check p-tags for our pubkey (explicit mention or reply)
    for tag in event.tags.iter() {
        let slice = tag.as_slice();
        if slice.first().map(|v| v.as_str()) == Some("p") {
            if let Some(hex) = slice.get(1) {
                if let Ok(pk) = PublicKey::from_hex(hex) {
                    if pk == *our_pubkey {
                        return true;
                    }
                }
            }
        }
    }

    // Check content for mentions
    let content = event.content.to_lowercase();
    let our_npub = our_pubkey.to_bech32().unwrap_or_else(|_| our_pubkey.to_hex());
    let our_npub_lower = our_npub.to_lowercase();

    // Check if content contains our npub
    if content.contains(&our_npub_lower) {
        return true;
    }

    // Check if content contains any of our mention names
    for name in mention_names {
        if content.contains(name) {
            return true;
        }
    }

    false
}