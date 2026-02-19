//! Mention detection logic for Nostr events.

use nostr_sdk::{Event, PublicKey, ToBech32, FromBech32};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use regex::Regex;

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

/// A detected mention in content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mention {
    pub mention_type: MentionType,
    pub raw_text: String,
    pub resolved_name: Option<String>,
    pub pubkey: Option<String>,
}

/// Type of mention detected
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MentionType {
    /// npub1... Bech32 encoded public key
    Npub,
    /// Hex public key (64 characters)
    HexPubkey,
    /// NIP-05 identifier (name@domain.tld)
    Nip05,
    /// @name mention in content text
    AtName,
}

/// Detect all mentions in content text and return structured results
pub fn detect_mentions(content: &str, known_pubkeys: &HashMap<String, String>) -> Vec<Mention> {
    let mut mentions = Vec::new();
    
    // Detect npub mentions (nostr: prefixed or standalone)
    if let Ok(npub_regex) = Regex::new(r"(?:nostr:)?(npub1[ac-hj-np-z02-9]{58})") {
        for cap in npub_regex.captures_iter(content) {
            if let Some(npub) = cap.get(1) {
                let npub_str = npub.as_str();
                let pubkey = PublicKey::from_bech32(npub_str).ok()
                    .map(|pk: PublicKey| pk.to_hex());
                let resolved_name = pubkey.as_ref()
                    .and_then(|pk| known_pubkeys.get(pk))
                    .cloned();
                    
                mentions.push(Mention {
                    mention_type: MentionType::Npub,
                    raw_text: cap.get(0).unwrap().as_str().to_string(),
                    resolved_name,
                    pubkey,
                });
            }
        }
    }
    
    // Detect hex pubkey mentions (64 hex characters)
    if let Ok(hex_regex) = Regex::new(r"\b([a-fA-F0-9]{64})\b") {
        for cap in hex_regex.captures_iter(content) {
            if let Some(hex_match) = cap.get(1) {
                let hex_str = hex_match.as_str();
                if let Ok(pubkey) = PublicKey::from_hex(hex_str) {
                    let pubkey_hex = pubkey.to_hex();
                    let resolved_name = known_pubkeys.get(&pubkey_hex).cloned();
                    
                    mentions.push(Mention {
                        mention_type: MentionType::HexPubkey,
                        raw_text: hex_str.to_string(),
                        resolved_name,
                        pubkey: Some(pubkey_hex),
                    });
                }
            }
        }
    }
    
    // Detect NIP-05 identifiers (name@domain.tld)
    if let Ok(nip05_regex) = Regex::new(r"\b([a-zA-Z0-9._-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,})\b") {
        for cap in nip05_regex.captures_iter(content) {
            if let Some(nip05_match) = cap.get(1) {
                let nip05_str = nip05_match.as_str();
                
                // Try to find if we know this NIP-05 identifier
                let pubkey_and_name = known_pubkeys.iter()
                    .find(|(_, name)| name.contains(nip05_str))
                    .map(|(pk, name)| (pk.clone(), name.clone()));
                    
                mentions.push(Mention {
                    mention_type: MentionType::Nip05,
                    raw_text: nip05_str.to_string(),
                    resolved_name: pubkey_and_name.as_ref().map(|(_, name)| name.clone()),
                    pubkey: pubkey_and_name.map(|(pk, _)| pk),
                });
            }
        }
    }
    
    // Detect @name mentions in content text
    if let Ok(at_mention_regex) = Regex::new(r"@([a-zA-Z0-9._-]+)") {
        for cap in at_mention_regex.captures_iter(content) {
            if let Some(name_match) = cap.get(1) {
                let name_str = name_match.as_str();
                
                // Try to find if we know someone with this name
                let pubkey_and_name = known_pubkeys.iter()
                    .find(|(_, known_name)| {
                        known_name.to_lowercase().contains(&name_str.to_lowercase()) ||
                        name_str.to_lowercase() == known_name.to_lowercase()
                    })
                    .map(|(pk, name)| (pk.clone(), name.clone()));
                
                mentions.push(Mention {
                    mention_type: MentionType::AtName,
                    raw_text: cap.get(0).unwrap().as_str().to_string(),
                    resolved_name: pubkey_and_name.as_ref().map(|(_, name)| name.clone()),
                    pubkey: pubkey_and_name.map(|(pk, _)| pk),
                });
            }
        }
    }
    
    mentions
}

/// Check if any mention in the list targets a specific pubkey
pub fn mentions_pubkey(mentions: &[Mention], target_pubkey: &str) -> bool {
    mentions.iter().any(|mention| {
        mention.pubkey.as_ref()
            .map(|pk| pk == target_pubkey)
            .unwrap_or(false)
    })
}

/// Extract all unique pubkeys mentioned in the content
pub fn extract_mentioned_pubkeys(mentions: &[Mention]) -> Vec<String> {
    let mut pubkeys = Vec::new();
    let mut seen = std::collections::HashSet::new();
    
    for mention in mentions {
        if let Some(pubkey) = &mention.pubkey {
            if seen.insert(pubkey.clone()) {
                pubkeys.push(pubkey.clone());
            }
        }
    }
    
    pubkeys
}

/// Sanitize content for preview display
pub fn sanitize_content_preview(content: &str, max_length: usize) -> String {
    // Strip duplicate whitespace and normalize newlines
    let normalized = content
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    
    // Remove multiple spaces
    let cleaned = normalized
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    
    // Strip nostr: URI prefixes for readability
    let de_nostrized = cleaned
        .replace("nostr:", "")
        .trim()
        .to_string();
    
    // Truncate to max length
    if de_nostrized.len() <= max_length {
        de_nostrized
    } else {
        // Try to break at word boundary near the limit
        let mut end = max_length;
        
        // Look for a space within the last 20 characters
        if let Some(space_pos) = de_nostrized[..max_length]
            .rfind(' ')
            .filter(|&pos| pos > max_length.saturating_sub(20))
        {
            end = space_pos;
        }

        format!("{}...", &de_nostrized[..end])
    }
}