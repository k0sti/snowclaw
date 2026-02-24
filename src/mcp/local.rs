//! Local MCP transport — stdio and SSE.
//!
//! Spawns an MCP server as a child process and communicates via JSON-RPC
//! over stdin/stdout (stdio transport) or HTTP SSE (SSE transport).

use super::types::*;
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, error, info};

// ── Stdio transport ──

/// MCP transport over child process stdin/stdout.
pub struct StdioTransport {
    /// Channel to send requests to the I/O loop.
    tx: mpsc::Sender<(JsonRpcRequest, oneshot::Sender<Result<JsonRpcResponse>>)>,
    /// Handle to kill the child on shutdown.
    child: Mutex<Option<Child>>,
}

impl StdioTransport {
    /// Spawn an MCP server process and set up the stdio transport.
    pub async fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        working_dir: Option<&PathBuf>,
    ) -> Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        for (k, v) in env {
            cmd.env(k, v);
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to spawn MCP server: {command}"))?;

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");

        let (tx, rx) =
            mpsc::channel::<(JsonRpcRequest, oneshot::Sender<Result<JsonRpcResponse>>)>(32);

        // Spawn the I/O loop
        tokio::spawn(stdio_io_loop(stdin, stdout, rx));

        Ok(Self {
            tx,
            child: Mutex::new(Some(child)),
        })
    }
}

/// Background I/O loop: serializes requests to stdin, reads responses from stdout.
async fn stdio_io_loop(
    mut stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    mut rx: mpsc::Receiver<(JsonRpcRequest, oneshot::Sender<Result<JsonRpcResponse>>)>,
) {
    let mut reader = BufReader::new(stdout);
    let mut pending: HashMap<String, oneshot::Sender<Result<JsonRpcResponse>>> = HashMap::new();
    let mut line_buf = String::new();

    // We need to handle both sending requests and reading responses concurrently.
    // Use a separate task for reading.
    let (resp_tx, mut resp_rx) = mpsc::channel::<String>(64);

    // Reader task
    let reader_handle = tokio::spawn(async move {
        loop {
            line_buf.clear();
            match reader.read_line(&mut line_buf).await {
                Ok(0) => break,  // EOF
                Ok(_) => {
                    let trimmed = line_buf.trim().to_string();
                    if !trimmed.is_empty() {
                        if resp_tx.send(trimmed).await.is_err() {
                            break;
                        }
                    }
                }
                Err(e) => {
                    error!("MCP stdio read error: {e}");
                    break;
                }
            }
        }
    });

    loop {
        tokio::select! {
            // New request from caller
            req = rx.recv() => {
                match req {
                    Some((request, callback)) => {
                        let id_key = request.id.to_string();
                        pending.insert(id_key, callback);

                        let mut payload = serde_json::to_string(&request).unwrap_or_default();
                        payload.push('\n');

                        if let Err(e) = stdin.write_all(payload.as_bytes()).await {
                            error!("MCP stdio write error: {e}");
                            break;
                        }
                        let _ = stdin.flush().await;
                    }
                    None => break, // All senders dropped
                }
            }
            // Response line from reader
            line = resp_rx.recv() => {
                match line {
                    Some(text) => {
                        match serde_json::from_str::<JsonRpcResponse>(&text) {
                            Ok(resp) => {
                                let id_key = resp.id.to_string();
                                if let Some(callback) = pending.remove(&id_key) {
                                    let _ = callback.send(Ok(resp));
                                } else {
                                    debug!("MCP: received response for unknown id {id_key}");
                                }
                            }
                            Err(_) => {
                                // Could be a notification or malformed — skip
                                debug!("MCP: non-response line: {}", &text[..text.len().min(200)]);
                            }
                        }
                    }
                    None => break, // Reader closed
                }
            }
        }
    }

    // Clean up pending requests
    for (_, callback) in pending.drain() {
        let _ = callback.send(Err(anyhow::anyhow!("MCP transport closed")));
    }

    reader_handle.abort();
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn request(&self, req: JsonRpcRequest) -> Result<JsonRpcResponse> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send((req, tx))
            .await
            .map_err(|_| anyhow::anyhow!("MCP stdio transport closed"))?;
        rx.await
            .map_err(|_| anyhow::anyhow!("MCP response channel dropped"))?
    }

    async fn shutdown(&self) -> Result<()> {
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.kill().await;
        }
        Ok(())
    }
}

// ── SSE transport ──

/// MCP transport over HTTP SSE (Server-Sent Events).
///
/// Connects to an MCP server's SSE endpoint. Requests are sent via HTTP POST,
/// responses arrive as SSE events.
pub struct SseTransport {
    endpoint: String,
    client: reqwest::Client,
}

impl SseTransport {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl McpTransport for SseTransport {
    async fn request(&self, req: JsonRpcRequest) -> Result<JsonRpcResponse> {
        let resp = self
            .client
            .post(&self.endpoint)
            .json(&req)
            .send()
            .await
            .with_context(|| format!("MCP SSE POST to {}", self.endpoint))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("MCP SSE error {status}: {body}");
        }

        let json_resp: JsonRpcResponse = resp
            .json()
            .await
            .context("Failed to parse MCP SSE response")?;

        Ok(json_resp)
    }

    async fn shutdown(&self) -> Result<()> {
        // SSE is stateless from our side; nothing to close.
        Ok(())
    }
}

// ── Bridge: connects, discovers tools, produces Vec<Box<dyn Tool>> ──

/// Configuration for a local MCP server.
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub working_dir: Option<PathBuf>,
}

/// Configuration for an SSE MCP server.
#[derive(Debug, Clone)]
pub struct McpSseConfig {
    pub name: String,
    pub url: String,
}

/// Bridge that manages local MCP server connections and exposes their tools.
pub struct McpLocalBridge {
    tools: Vec<Box<dyn Tool>>,
}

impl McpLocalBridge {
    /// Connect to a stdio MCP server, initialize, discover tools.
    pub async fn from_stdio(config: &McpServerConfig) -> Result<Self> {
        info!(
            server = %config.name,
            command = %config.command,
            "Connecting to MCP server (stdio)"
        );

        let transport = StdioTransport::spawn(
            &config.command,
            &config.args,
            &config.env,
            config.working_dir.as_ref(),
        )
        .await?;

        let client = Arc::new(Mutex::new(McpClient::new(transport, &config.name)));

        // Initialize
        {
            let c = client.lock().await;
            let caps = c.initialize().await?;
            debug!(server = %config.name, ?caps, "MCP server initialized");
        }

        // Discover tools
        let tool_defs = {
            let c = client.lock().await;
            c.list_tools().await?
        };

        info!(
            server = %config.name,
            tool_count = tool_defs.len(),
            "Discovered MCP tools"
        );

        let tools: Vec<Box<dyn Tool>> = tool_defs
            .into_iter()
            .map(|def| {
                let wrapper = McpToolWrapper::new(client.clone(), def, config.name.clone());
                Box::new(wrapper) as Box<dyn Tool>
            })
            .collect();

        Ok(Self { tools })
    }

    /// Connect to an SSE MCP server, initialize, discover tools.
    pub async fn from_sse(config: &McpSseConfig) -> Result<Self> {
        info!(
            server = %config.name,
            url = %config.url,
            "Connecting to MCP server (SSE)"
        );

        let transport = SseTransport::new(&config.url);
        let client = Arc::new(Mutex::new(McpClient::new(transport, &config.name)));

        // Initialize
        {
            let c = client.lock().await;
            let caps = c.initialize().await?;
            debug!(server = %config.name, ?caps, "MCP server initialized");
        }

        // Discover tools
        let tool_defs = {
            let c = client.lock().await;
            c.list_tools().await?
        };

        info!(
            server = %config.name,
            tool_count = tool_defs.len(),
            "Discovered MCP tools"
        );

        let tools: Vec<Box<dyn Tool>> = tool_defs
            .into_iter()
            .map(|def| {
                let wrapper = McpToolWrapper::new(client.clone(), def, config.name.clone());
                Box::new(wrapper) as Box<dyn Tool>
            })
            .collect();

        Ok(Self { tools })
    }

    /// Consume the bridge and return the discovered tools.
    pub fn into_tools(self) -> Vec<Box<dyn Tool>> {
        self.tools
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_server_config_builds() {
        let config = McpServerConfig {
            name: "test".into(),
            command: "echo".into(),
            args: vec![],
            env: HashMap::new(),
            working_dir: None,
        };
        assert_eq!(config.name, "test");
    }

    #[test]
    fn sse_config_builds() {
        let config = McpSseConfig {
            name: "remote".into(),
            url: "http://localhost:8080/mcp".into(),
        };
        assert_eq!(config.name, "remote");
    }
}
