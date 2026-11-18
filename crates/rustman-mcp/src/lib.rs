//! Rustman over the Model Context Protocol (agent-mcp-interface).
//!
//! [`server`] lets AI agents drive the workspace through the same engine and
//! safety policy a human uses. The MCP client side (add-mcp-client) will
//! live in its own module next to it, sharing the pinned `rmcp`.

pub mod server;
