//! MCP (Model Context Protocol) integration module.
//!
//! Provides two transports for connecting to MCP servers:
//!
//! - **Local** (`local.rs`): stdio/SSE transport for local MCP server processes
//! - **ContextVM** (`contextvm.rs`): Nostr relay transport via the ContextVM protocol
//!
//! Both transports discover tools from MCP servers and wrap them as standard
//! [`Tool`](crate::tools::Tool) instances, so the agent loop treats them
//! identically to native tools.

pub mod contextvm;
pub mod local;
pub mod types;

pub use local::McpLocalBridge;
pub use contextvm::McpContextVmBridge;
pub use types::*;
