//! Snowclaw-specific configuration types.
//!
//! Extracted from `schema.rs` to minimize upstream diff on rebase.
//! Types defined here are Snowclaw additions to the ZeroClaw config
//! model (Nostr channel, ContextVM, etc.).

use crate::config::traits::ChannelConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ── Identity ────────────────────────────────────────────────────

/// Default config directory name under `$HOME`.
///
/// Centralized here so the snowclaw rename doesn't scatter across files.
pub const APP_DIR_NAME: &str = ".snowclaw";

// ── Nostr channel config ────────────────────────────────────────

/// Nostr channel configuration (NIP-04 + NIP-17 private messages)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NostrConfig {
    /// Nostr secret key (nsec1... or hex). Can also be set via SNOWCLAW_NSEC env var.
    #[serde(default)]
    pub nsec: Option<String>,
    /// Relay URLs (wss://). Defaults to popular public relays if omitted.
    #[serde(default = "default_nostr_relays")]
    pub relays: Vec<String>,
    /// Allowed sender public keys (hex or npub). Empty = deny all, "*" = allow all
    #[serde(default)]
    pub allowed_pubkeys: Vec<String>,
    /// Owner pubkey (hex or npub) for admin commands
    #[serde(default)]
    pub owner: Option<String>,
    /// NIP-29 group IDs to join
    #[serde(default)]
    pub groups: Vec<String>,
    /// How to respond: "always" | "mention_only" | "never"
    #[serde(default = "default_respond_mode")]
    pub respond_mode: String,
    /// Group-specific respond mode overrides (group_id -> mode)
    #[serde(default)]
    pub group_respond_mode: std::collections::HashMap<String, String>,
    /// Names that trigger mention detection (e.g. ["snowclaw", "snow"])
    #[serde(default)]
    pub mention_names: Vec<String>,
    /// Listen for DMs
    #[serde(default = "default_true")]
    pub listen_dms: bool,
    /// Number of recent messages to include as context
    #[serde(default = "default_context_history")]
    pub context_history: usize,
}

impl ChannelConfig for NostrConfig {
    fn name() -> &'static str {
        "Nostr"
    }
    fn desc() -> &'static str {
        "Nostr DMs"
    }
}

fn default_respond_mode() -> String {
    "mention_only".into()
}
fn default_context_history() -> usize {
    10
}
fn default_true() -> bool {
    true
}

pub fn default_nostr_relays() -> Vec<String> {
    vec![
        "wss://relay.damus.io".to_string(),
        "wss://nos.lol".to_string(),
        "wss://relay.primal.net".to_string(),
        "wss://relay.snort.social".to_string(),
    ]
}

// ── ContextVM (MCP over Nostr) ──────────────────────────────────

/// ContextVM (MCP over Nostr) configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContextVmEntry {
    /// Enable ContextVM discovery and tool registration.
    #[serde(default)]
    pub enabled: bool,
    /// Nostr relays to connect to for ContextVM.
    #[serde(default)]
    pub relays: Vec<String>,
    /// Optional: only discover tools from these server npubs.
    #[serde(default)]
    pub servers: Vec<String>,
    /// RPC call timeout in seconds (default: 30).
    #[serde(default = "default_contextvm_timeout")]
    pub timeout_secs: u64,
}

pub fn default_contextvm_timeout() -> u64 {
    30
}

// ── MCP server entry (alternative/simplified representation) ────

/// A local MCP server entry (simplified config representation).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct McpServerEntry {
    /// Human-readable name (used as tool name prefix).
    pub name: String,
    /// Transport type: "stdio" (default) or "sse".
    #[serde(default = "default_mcp_transport")]
    pub transport: String,
    /// Command to spawn (stdio transport).
    #[serde(default)]
    pub command: Option<String>,
    /// Arguments for the command.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables for the process.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    /// Working directory for the process.
    #[serde(default)]
    pub working_dir: Option<String>,
    /// URL endpoint (SSE transport).
    #[serde(default)]
    pub url: Option<String>,
}

pub fn default_mcp_transport() -> String {
    "stdio".into()
}

// ── Memory Nostr extensions ─────────────────────────────────────

/// Default re-index interval in minutes.
pub fn default_index_interval_minutes() -> u64 {
    30
}

/// Default for cost tracking (enabled in snowclaw).
pub fn default_cost_enabled() -> bool {
    true
}

// ── Browser pinchtab extension ──────────────────────────────────

/// Default Pinchtab HTTP API base URL.
pub fn default_pinchtab_url() -> String {
    "http://127.0.0.1:9867".into()
}
