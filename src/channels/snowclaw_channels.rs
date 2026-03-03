//! Snowclaw-specific channel construction logic.
//!
//! Extracted from `mod.rs` to minimize upstream diff. The Nostr channel
//! config builder (groups, DMs, respond modes, mentions, etc.) lives here.

use crate::channels::nostr::{NostrChannel, NostrChannelConfig, RespondMode};
use crate::config::Config;
use std::sync::Arc;

use super::ConfiguredChannel;

/// Build and append the Nostr channel from Snowclaw config, returning an
/// error reason string if initialization fails (or `None` on success).
pub(crate) async fn append_nostr_channel(
    config: &Config,
    channels: &mut Vec<ConfiguredChannel>,
    startup_context: &str,
) -> Option<String> {
    let ns = config.channels_config.nostr.as_ref()?;
    let nsec_str = ns
        .nsec
        .clone()
        .or_else(|| std::env::var("SNOWCLAW_NSEC").ok())?;
    let keys = match nostr_sdk::Keys::parse(&nsec_str) {
        Ok(k) => k,
        Err(e) => {
            let reason = format!("Nostr key parse failed during {startup_context}: {e}");
            tracing::warn!("{reason}");
            return Some(reason);
        }
    };
    let channel_config = NostrChannelConfig {
        relays: ns.relays.clone(),
        keys: keys.clone(),
        groups: ns.groups.clone(),
        listen_dms: ns.listen_dms,
        allowed_pubkeys: ns
            .allowed_pubkeys
            .iter()
            .filter_map(|s| nostr_sdk::PublicKey::parse(s).ok())
            .collect(),
        respond_mode: RespondMode::from_str(&ns.respond_mode),
        group_respond_mode: ns
            .group_respond_mode
            .iter()
            .map(|(k, v)| (k.clone(), RespondMode::from_str(v)))
            .collect(),
        mention_names: {
            let mut names: Vec<String> = ns
                .mention_names
                .iter()
                .map(|n| n.to_lowercase())
                .collect();
            if !names.iter().any(|n| n == "snowclaw") {
                names.push("snowclaw".to_string());
            }
            names
        },
        owner: ns
            .owner
            .as_ref()
            .and_then(|s| nostr_sdk::PublicKey::parse(s).ok()),
        context_history: ns.context_history,
        extra_kinds: ns.extra_kinds.clone(),
        persist_dir: config
            .config_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf(),
        indexed_paths: config.memory.indexed_paths.clone(),
        index_interval_minutes: config.memory.index_interval_minutes,
    };
    match NostrChannel::new(channel_config).await {
        Ok(channel) => {
            channels.push(ConfiguredChannel {
                display_name: "Nostr",
                channel: Arc::new(channel),
            });
            None
        }
        Err(err) => {
            let reason = format!("Nostr init failed during {startup_context}: {err}");
            tracing::warn!("{reason}");
            Some(reason)
        }
    }
}
