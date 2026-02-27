//! Snowclaw-specific agent extensions (MCP tool registration).
//!
//! Extracted from `agent.rs` to minimize upstream diff. All MCP wiring
//! (local stdio/SSE servers + ContextVM over Nostr) lives here.

use crate::config::Config;
use crate::tools::Tool;

/// Register configured MCP tools (local stdio/SSE servers + ContextVM) into the
/// tools vector. Called during agent build to extend the base tool set.
pub(crate) fn register_mcp_tools(config: &Config, tools: &mut Vec<Box<dyn Tool>>) {
    use crate::mcp;
    let handle = tokio::runtime::Handle::current();

    // Local MCP servers
    for server_entry in &config.mcp.servers {
        let result: Option<anyhow::Result<mcp::McpLocalBridge>> =
            tokio::task::block_in_place(|| {
                handle.block_on(async {
                    match server_entry.transport {
                        crate::config::schema::McpTransport::Sse
                        | crate::config::schema::McpTransport::Http => {
                            if let Some(ref url) = server_entry.url {
                                let sse_cfg = mcp::local::McpSseConfig {
                                    name: server_entry.name.clone(),
                                    url: url.clone(),
                                };
                                Some(mcp::McpLocalBridge::from_sse(&sse_cfg).await)
                            } else {
                                tracing::warn!(server = %server_entry.name, "MCP SSE/HTTP server missing 'url'");
                                None
                            }
                        }
                        crate::config::schema::McpTransport::Stdio => {
                            if server_entry.command.is_empty() {
                                tracing::warn!(server = %server_entry.name, "MCP stdio server missing 'command'");
                                None
                            } else {
                                let stdio_cfg = mcp::local::McpServerConfig {
                                    name: server_entry.name.clone(),
                                    command: server_entry.command.clone(),
                                    args: server_entry.args.clone(),
                                    env: server_entry.env.clone(),
                                    working_dir: None,
                                };
                                Some(mcp::McpLocalBridge::from_stdio(&stdio_cfg).await)
                            }
                        }
                    }
                })
            });

        if let Some(Ok(bridge)) = result {
            let mcp_tools = bridge.into_tools();
            tracing::info!(
                server = %server_entry.name,
                tool_count = mcp_tools.len(),
                "Registered MCP tools"
            );
            tools.extend(mcp_tools);
        } else if let Some(Err(e)) = result {
            tracing::warn!(server = %server_entry.name, error = %e, "Failed to connect MCP server");
        }
    }

    // ContextVM (MCP over Nostr)
    if let Some(cvm) = &config.mcp.contextvm {
        if cvm.enabled && !cvm.relays.is_empty() {
            let server_filter: Vec<nostr_sdk::PublicKey> = cvm
                .servers
                .iter()
                .filter_map(|s| s.parse::<nostr_sdk::PublicKey>().ok())
                .collect();

            let cvm_config = mcp::contextvm::ContextVmConfig {
                relays: cvm.relays.clone(),
                keys: nostr_sdk::Keys::generate(),
                server_filter,
                call_timeout: std::time::Duration::from_secs(cvm.timeout_secs),
                discovery_timeout: std::time::Duration::from_secs(10),
            };

            let cvm_result: anyhow::Result<mcp::McpContextVmBridge> =
                tokio::task::block_in_place(|| {
                    handle.block_on(mcp::McpContextVmBridge::connect(&cvm_config))
                });

            match cvm_result {
                Ok(bridge) => {
                    let cvm_tools: Vec<Box<dyn crate::tools::Tool>> = bridge.into_tools();
                    tracing::info!(tool_count = cvm_tools.len(), "Registered ContextVM tools");
                    tools.extend(cvm_tools);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to connect ContextVM");
                }
            }
        }
    }
}
