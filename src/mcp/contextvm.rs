//! ContextVM transport — MCP over Nostr relays.
//!
//! Uses the ContextVM protocol to discover and call MCP tools exposed by
//! Nostr identities. JSON-RPC messages are wrapped in Nostr events and
//! sent/received through relay connections.
//!
//! Protocol overview (from ContextVM SDK):
//! - Servers announce capabilities via kind 31990 (NIP-89 app handler) events
//! - Client discovers servers by querying relays for these announcements
//! - JSON-RPC request/response is transported via NIP-44 encrypted DMs (kind 14)
//!   or via ephemeral events (kind 20000+) depending on server configuration
//! - The ContextVM SDK defines specific event kinds and tag conventions

use super::types::*;
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use nostr_sdk::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{oneshot, Mutex, RwLock};
use tracing::{debug, info, warn};

/// ContextVM event kinds (from ContextVM protocol spec).
///
/// These may need updating as the ContextVM protocol evolves.
mod kinds {
    /// NIP-89 application handler — servers announce their MCP capabilities.
    pub const APP_HANDLER: u16 = 31990;

    /// Ephemeral event kind range used for JSON-RPC transport.
    /// ContextVM uses kind 21059 for encrypted request/response.
    pub const ENCRYPTED_MESSAGE: u16 = 21059;
}

/// Configuration for a ContextVM connection.
#[derive(Debug, Clone)]
pub struct ContextVmConfig {
    /// Nostr relays to connect to.
    pub relays: Vec<String>,
    /// Our keypair for signing events and decrypting responses.
    pub keys: Keys,
    /// Optional filter: only discover tools from these server npubs.
    /// If empty, discover all ContextVM servers on the relays.
    pub server_filter: Vec<PublicKey>,
    /// Timeout for individual RPC calls.
    pub call_timeout: Duration,
    /// Timeout for discovery queries.
    pub discovery_timeout: Duration,
}

impl Default for ContextVmConfig {
    fn default() -> Self {
        Self {
            relays: vec![],
            keys: Keys::generate(),
            server_filter: vec![],
            call_timeout: Duration::from_secs(30),
            discovery_timeout: Duration::from_secs(10),
        }
    }
}

/// A discovered ContextVM server with its available tools.
#[derive(Debug, Clone)]
struct ContextVmServer {
    /// Server's public key.
    pubkey: PublicKey,
    /// Human-readable name (from NIP-89 metadata).
    name: String,
    /// Available tools.
    tools: Vec<McpToolDef>,
}

/// Nostr-based MCP transport targeting a specific ContextVM server.
struct NostrTransport {
    client: Arc<Client>,
    server_pubkey: PublicKey,
    our_keys: Keys,
    timeout: Duration,
    /// Pending responses keyed by JSON-RPC request id (stringified).
    pending: Arc<RwLock<HashMap<String, oneshot::Sender<Result<JsonRpcResponse>>>>>,
}

impl NostrTransport {
    fn new(
        client: Arc<Client>,
        server_pubkey: PublicKey,
        our_keys: Keys,
        timeout: Duration,
    ) -> Self {
        Self {
            client,
            server_pubkey,
            our_keys,
            timeout,
            pending: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl McpTransport for NostrTransport {
    async fn request(&self, req: JsonRpcRequest) -> Result<JsonRpcResponse> {
        let id_key = req.id.to_string();
        let payload = serde_json::to_string(&req)?;

        // Register pending response
        let (tx, rx) = oneshot::channel();
        self.pending.write().await.insert(id_key.clone(), tx);

        // Encrypt and send via NIP-44
        let encrypted = nip44::encrypt(
            self.our_keys.secret_key(),
            &self.server_pubkey,
            payload.as_bytes(),
            nip44::Version::V2,
        )
        .map_err(|e| anyhow::anyhow!("NIP-44 encrypt error: {e}"))?;

        let event = EventBuilder::new(
            Kind::Custom(kinds::ENCRYPTED_MESSAGE),
            encrypted,
        )
        .tag(Tag::public_key(self.server_pubkey));

        self.client
            .send_event_builder(event)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send ContextVM request: {e}"))?;

        // Wait for response with timeout
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                self.pending.write().await.remove(&id_key);
                Err(anyhow::anyhow!("ContextVM response channel dropped"))
            }
            Err(_) => {
                self.pending.write().await.remove(&id_key);
                Err(anyhow::anyhow!(
                    "ContextVM request timed out after {:?}",
                    self.timeout
                ))
            }
        }
    }

    async fn shutdown(&self) -> Result<()> {
        // Drain pending with errors
        let mut pending = self.pending.write().await;
        for (_, tx) in pending.drain() {
            let _ = tx.send(Err(anyhow::anyhow!("Transport shutting down")));
        }
        Ok(())
    }
}

/// Bridge that discovers ContextVM servers on Nostr relays and exposes their tools.
pub struct McpContextVmBridge {
    tools: Vec<Box<dyn Tool>>,
    client: Arc<Client>,
}

impl McpContextVmBridge {
    /// Connect to relays, discover ContextVM servers, initialize, and collect tools.
    pub async fn connect(config: &ContextVmConfig) -> Result<Self> {
        if config.relays.is_empty() {
            anyhow::bail!("ContextVM: no relays configured");
        }

        info!(
            relays = ?config.relays,
            server_filter_count = config.server_filter.len(),
            "Connecting to ContextVM relays"
        );

        // Build nostr client
        let client = Client::builder().signer(config.keys.clone()).build();

        for relay in &config.relays {
            client
                .add_relay(relay.as_str())
                .await
                .with_context(|| format!("Failed to add relay: {relay}"))?;
        }

        client.connect().await;

        let client = Arc::new(client);

        // Discover ContextVM servers via NIP-89 app handler events
        let servers = discover_servers(&client, &config.server_filter, config.discovery_timeout)
            .await?;

        info!(
            server_count = servers.len(),
            "Discovered ContextVM servers"
        );

        let mut all_tools: Vec<Box<dyn Tool>> = Vec::new();

        for server in &servers {
            let transport = NostrTransport::new(
                client.clone(),
                server.pubkey,
                config.keys.clone(),
                config.call_timeout,
            );

            let mcp_client = Arc::new(Mutex::new(McpClient::new(transport, &server.name)));

            // Initialize MCP handshake
            let init_result = {
                let c = mcp_client.lock().await;
                c.initialize().await
            };

            match init_result {
                Ok(caps) => {
                    debug!(server = %server.name, ?caps, "ContextVM server initialized");
                }
                Err(e) => {
                    warn!(server = %server.name, error = %e, "Failed to initialize ContextVM server, skipping");
                    continue;
                }
            }

            // Discover tools
            let tool_defs = {
                let c = mcp_client.lock().await;
                c.list_tools().await
            };

            match tool_defs {
                Ok(defs) => {
                    info!(
                        server = %server.name,
                        tool_count = defs.len(),
                        "Discovered ContextVM tools"
                    );

                    for def in defs {
                        let wrapper =
                            McpToolWrapper::new(mcp_client.clone(), def, server.name.clone());
                        all_tools.push(Box::new(wrapper));
                    }
                }
                Err(e) => {
                    warn!(server = %server.name, error = %e, "Failed to list ContextVM tools");
                }
            }
        }

        Ok(Self {
            tools: all_tools,
            client,
        })
    }

    /// Consume the bridge and return the discovered tools.
    pub fn into_tools(self) -> Vec<Box<dyn Tool>> {
        self.tools
    }
}

/// Query relays for ContextVM server announcements (NIP-89 kind 31990).
async fn discover_servers(
    client: &Client,
    filter_pubkeys: &[PublicKey],
    timeout: Duration,
) -> Result<Vec<ContextVmServer>> {
    let mut filter = Filter::new().kind(Kind::Custom(kinds::APP_HANDLER));

    if !filter_pubkeys.is_empty() {
        filter = filter.authors(filter_pubkeys.iter().copied());
    }

    // Look for events with "mcp" or "contextvm" in the `d` tag
    // This is a heuristic — the exact tag convention depends on ContextVM spec evolution.

    let events = client
        .fetch_events(filter, timeout)
        .await
        .context("Failed to fetch ContextVM server announcements")?;

    let mut servers = Vec::new();

    for event in events.into_iter() {
        // Parse NIP-89 app handler content for server metadata
        let name = extract_server_name(&event).unwrap_or_else(|| {
            // Fallback: use truncated pubkey
            let hex = event.pubkey.to_hex();
            format!("cvm_{}", &hex[..8])
        });

        // Check if this is actually a ContextVM/MCP announcement
        let is_contextvm = event
            .tags
            .iter()
            .any(|tag| {
                let t = tag.as_slice();
                t.len() >= 2
                    && (t[1].contains("mcp") || t[1].contains("contextvm") || t[1].contains("MCP"))
            })
            || event.content.contains("mcp")
            || event.content.contains("contextvm");

        if !is_contextvm && filter_pubkeys.is_empty() {
            // Skip non-ContextVM app handlers when doing broad discovery
            continue;
        }

        servers.push(ContextVmServer {
            pubkey: event.pubkey,
            name,
            tools: vec![], // Tools are discovered via MCP protocol, not from the announcement
        });
    }

    Ok(servers)
}

/// Extract a human-readable name from a NIP-89 app handler event.
fn extract_server_name(event: &Event) -> Option<String> {
    // Try parsing content as JSON (common for NIP-89)
    if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&event.content) {
        if let Some(name) = meta.get("name").and_then(|n| n.as_str()) {
            return Some(name.to_string());
        }
        if let Some(name) = meta.get("display_name").and_then(|n| n.as_str()) {
            return Some(name.to_string());
        }
    }

    // Try `d` tag as fallback
    for tag in event.tags.iter() {
        let t = tag.as_slice();
        if t.len() >= 2 && t[0] == "d" {
            return Some(t[1].to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contextvm_config_defaults() {
        let config = ContextVmConfig::default();
        assert!(config.relays.is_empty());
        assert!(config.server_filter.is_empty());
        assert_eq!(config.call_timeout, Duration::from_secs(30));
    }

    #[test]
    fn contextvm_config_with_relays() {
        let config = ContextVmConfig {
            relays: vec!["wss://relay.example.com".into()],
            ..Default::default()
        };
        assert_eq!(config.relays.len(), 1);
    }
}
