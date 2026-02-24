//! Shared MCP JSON-RPC types and schema conversion utilities.

use crate::tools::{Tool, ToolResult, ToolSpec};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

// ── JSON-RPC 2.0 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

// ── MCP protocol types ──

/// MCP server capabilities from `initialize` response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpServerCapabilities {
    #[serde(default)]
    pub tools: Option<serde_json::Value>,
    #[serde(default)]
    pub resources: Option<serde_json::Value>,
    #[serde(default)]
    pub prompts: Option<serde_json::Value>,
}

/// MCP tool definition as returned by `tools/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Option<serde_json::Value>,
}

/// MCP tool call result content item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpContent {
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
}

// ── Transport trait ──

/// Abstract MCP transport — send a JSON-RPC request, get a response.
#[async_trait]
pub trait McpTransport: Send + Sync + 'static {
    /// Send a JSON-RPC request and await the response.
    async fn request(&self, req: JsonRpcRequest) -> anyhow::Result<JsonRpcResponse>;

    /// Shut down the transport (kill process, close connection, etc.).
    async fn shutdown(&self) -> anyhow::Result<()>;
}

// ── Request ID counter ──

static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub fn next_request_id() -> serde_json::Value {
    let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    serde_json::Value::Number(id.into())
}

// ── MCP client (transport-agnostic) ──

/// MCP client that works with any transport.
pub struct McpClient<T: McpTransport> {
    transport: T,
    server_name: String,
}

impl<T: McpTransport> McpClient<T> {
    pub fn new(transport: T, server_name: impl Into<String>) -> Self {
        Self {
            transport,
            server_name: server_name.into(),
        }
    }

    /// Perform MCP `initialize` handshake.
    pub async fn initialize(&self) -> anyhow::Result<McpServerCapabilities> {
        let resp = self
            .transport
            .request(JsonRpcRequest {
                jsonrpc: "2.0".into(),
                id: next_request_id(),
                method: "initialize".into(),
                params: Some(serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "snowclaw",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                })),
            })
            .await?;

        if let Some(err) = &resp.error {
            anyhow::bail!("MCP initialize error: {} (code {})", err.message, err.code);
        }

        let capabilities = resp
            .result
            .and_then(|r| r.get("capabilities").cloned())
            .map(|c| serde_json::from_value(c).unwrap_or_default())
            .unwrap_or_default();

        // Send initialized notification
        let _ = self
            .transport
            .request(JsonRpcRequest {
                jsonrpc: "2.0".into(),
                id: next_request_id(),
                method: "notifications/initialized".into(),
                params: None,
            })
            .await;

        Ok(capabilities)
    }

    /// List available tools from the MCP server.
    pub async fn list_tools(&self) -> anyhow::Result<Vec<McpToolDef>> {
        let resp = self
            .transport
            .request(JsonRpcRequest {
                jsonrpc: "2.0".into(),
                id: next_request_id(),
                method: "tools/list".into(),
                params: None,
            })
            .await?;

        if let Some(err) = &resp.error {
            anyhow::bail!("MCP tools/list error: {} (code {})", err.message, err.code);
        }

        let tools: Vec<McpToolDef> = resp
            .result
            .and_then(|r| r.get("tools").cloned())
            .map(|t| serde_json::from_value(t).unwrap_or_default())
            .unwrap_or_default();

        Ok(tools)
    }

    /// Call a tool on the MCP server.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> anyhow::Result<Vec<McpContent>> {
        let resp = self
            .transport
            .request(JsonRpcRequest {
                jsonrpc: "2.0".into(),
                id: next_request_id(),
                method: "tools/call".into(),
                params: Some(serde_json::json!({
                    "name": name,
                    "arguments": arguments
                })),
            })
            .await?;

        if let Some(err) = &resp.error {
            anyhow::bail!("MCP tools/call error: {} (code {})", err.message, err.code);
        }

        let content: Vec<McpContent> = resp
            .result
            .and_then(|r| r.get("content").cloned())
            .map(|c| serde_json::from_value(c).unwrap_or_default())
            .unwrap_or_default();

        Ok(content)
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }
}

// ── MCP tool wrapper (adapts MCP tool to native Tool trait) ──

/// Wraps a single MCP tool definition + client reference as a native `Tool`.
pub struct McpToolWrapper<T: McpTransport> {
    client: Arc<Mutex<McpClient<T>>>,
    def: McpToolDef,
    /// Optional prefix to avoid name collisions (e.g., "myserver_")
    prefix: String,
}

impl<T: McpTransport> McpToolWrapper<T> {
    pub fn new(client: Arc<Mutex<McpClient<T>>>, def: McpToolDef, prefix: String) -> Self {
        Self {
            client,
            def,
            prefix,
        }
    }

    fn prefixed_name(&self) -> String {
        if self.prefix.is_empty() {
            self.def.name.clone()
        } else {
            format!("{}_{}", self.prefix, self.def.name)
        }
    }
}

#[async_trait]
impl<T: McpTransport> Tool for McpToolWrapper<T> {
    fn name(&self) -> &str {
        // We need a stable reference, so we leak a little here.
        // In practice tools are long-lived singletons.
        // A better approach would be caching the string, but for now this works.
        // Actually, let's use the def name directly and handle prefix in spec().
        &self.def.name
    }

    fn description(&self) -> &str {
        self.def
            .description
            .as_deref()
            .unwrap_or("MCP tool (no description)")
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.def
            .input_schema
            .clone()
            .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}))
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let client = self.client.lock().await;
        match client.call_tool(&self.def.name, args).await {
            Ok(content) => {
                let output = content
                    .iter()
                    .filter_map(|c| c.text.as_deref())
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.prefixed_name(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_rpc_request_serializes() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(1),
            method: "tools/list".into(),
            params: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("tools/list"));
    }

    #[test]
    fn json_rpc_response_deserializes_result() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn json_rpc_response_deserializes_error() {
        let json = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Invalid"}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32600);
    }

    #[test]
    fn mcp_tool_def_deserializes() {
        let json = r#"{"name":"read_file","description":"Read a file","inputSchema":{"type":"object","properties":{"path":{"type":"string"}}}}"#;
        let def: McpToolDef = serde_json::from_str(json).unwrap();
        assert_eq!(def.name, "read_file");
        assert!(def.input_schema.is_some());
    }

    #[test]
    fn next_request_id_increments() {
        let a = next_request_id();
        let b = next_request_id();
        assert_ne!(a, b);
    }
}
