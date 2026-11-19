//! Rustman over the Model Context Protocol.
//!
//! [`server`] lets AI agents drive the workspace through the same engine and
//! safety policy a human uses (agent-mcp-interface). [`client`] connects to
//! other MCP servers to explore and test them (mcp-client). Both share the
//! pinned `rmcp`.

pub mod client;
pub mod server;
