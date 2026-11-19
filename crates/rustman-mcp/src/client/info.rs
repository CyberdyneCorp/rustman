//! What a session shows after initialization (mcp-client spec "Server info
//! and capabilities display"): the server's identity, the negotiated
//! version, its instructions and capabilities, and which operations those
//! capabilities leave disabled.

use std::fmt;

use rmcp::model::{ServerCapabilities, ServerPeerInfo};
use serde::Serialize;

/// How the session reaches the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Stdio,
    StreamableHttp,
    /// The pre-2025 HTTP+SSE transport, after the fallback.
    LegacySse,
}

impl TransportKind {
    /// The label shown with the session.
    pub fn label(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::StreamableHttp => "streamable HTTP",
            Self::LegacySse => "legacy SSE",
        }
    }
}

impl fmt::Display for TransportKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// The capabilities a server declared, as Rustman uses them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    /// `Some(list_changed)` when `tools` is declared.
    pub tools: Option<bool>,
    pub resources: Option<ResourceCapabilities>,
    /// `Some(list_changed)` when `prompts` is declared.
    pub prompts: Option<bool>,
    pub logging: bool,
    pub completions: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ResourceCapabilities {
    pub subscribe: bool,
    pub list_changed: bool,
}

impl From<&ServerCapabilities> for Capabilities {
    fn from(c: &ServerCapabilities) -> Self {
        let flag = |value: Option<bool>| value.unwrap_or(false);
        Self {
            tools: c.tools.as_ref().map(|t| flag(t.list_changed)),
            resources: c.resources.as_ref().map(|r| ResourceCapabilities {
                subscribe: flag(r.subscribe),
                list_changed: flag(r.list_changed),
            }),
            prompts: c.prompts.as_ref().map(|p| flag(p.list_changed)),
            logging: c.logging.is_some(),
            completions: c.completions.is_some(),
        }
    }
}

/// An operation a session can perform, gated by a server capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    ListTools,
    CallTool,
    ListResources,
    ListResourceTemplates,
    ReadResource,
    Subscribe,
    ListPrompts,
    GetPrompt,
    SetLogLevel,
    Complete,
}

/// Why an operation is disabled, e.g. `server does not declare prompts`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Disabled {
    pub reason: String,
}

impl fmt::Display for Disabled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

impl Capabilities {
    /// `Ok` when `operation` is available, else the reason it is disabled.
    pub fn check(&self, operation: Operation) -> Result<(), Disabled> {
        let (declared, capability) = match operation {
            Operation::ListTools | Operation::CallTool => (self.tools.is_some(), "tools"),
            Operation::ListResources
            | Operation::ListResourceTemplates
            | Operation::ReadResource => (self.resources.is_some(), "resources"),
            Operation::Subscribe => (
                self.resources.is_some_and(|r| r.subscribe),
                "resources.subscribe",
            ),
            Operation::ListPrompts | Operation::GetPrompt => (self.prompts.is_some(), "prompts"),
            Operation::SetLogLevel => (self.logging, "logging"),
            Operation::Complete => (self.completions, "completions"),
        };
        if declared {
            Ok(())
        } else {
            Err(Disabled {
                reason: format!("server does not declare {capability}"),
            })
        }
    }
}

/// The server as initialization described it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionInfo {
    pub server_name: String,
    pub server_version: String,
    pub server_title: Option<String>,
    /// The negotiated protocol version.
    pub protocol_version: String,
    pub instructions: Option<String>,
    pub capabilities: Capabilities,
    pub transport: TransportKind,
}

impl SessionInfo {
    pub(crate) fn new(peer: &ServerPeerInfo, transport: TransportKind) -> Self {
        let server = peer.server_info.as_ref();
        Self {
            server_name: server.map(|s| s.name.clone()).unwrap_or_default(),
            server_version: server.map(|s| s.version.clone()).unwrap_or_default(),
            server_title: server.and_then(|s| s.title.clone()),
            protocol_version: peer.protocol_version.to_string(),
            instructions: peer.instructions.clone(),
            capabilities: Capabilities::from(&peer.capabilities),
            transport,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undeclared_capabilities_disable_their_operations() {
        let capabilities = Capabilities {
            tools: Some(false),
            ..Capabilities::default()
        };
        assert!(capabilities.check(Operation::CallTool).is_ok());
        assert_eq!(
            capabilities
                .check(Operation::GetPrompt)
                .unwrap_err()
                .to_string(),
            "server does not declare prompts"
        );
        assert_eq!(
            capabilities
                .check(Operation::ListResources)
                .unwrap_err()
                .reason,
            "server does not declare resources"
        );
        assert_eq!(
            capabilities
                .check(Operation::SetLogLevel)
                .unwrap_err()
                .reason,
            "server does not declare logging"
        );
        assert_eq!(
            capabilities.check(Operation::Complete).unwrap_err().reason,
            "server does not declare completions"
        );
    }

    #[test]
    fn subscribe_needs_the_subscribe_flag() {
        let mut capabilities = Capabilities {
            resources: Some(ResourceCapabilities::default()),
            ..Capabilities::default()
        };
        assert!(capabilities.check(Operation::ReadResource).is_ok());
        assert_eq!(
            capabilities.check(Operation::Subscribe).unwrap_err().reason,
            "server does not declare resources.subscribe"
        );
        capabilities.resources = Some(ResourceCapabilities {
            subscribe: true,
            list_changed: false,
        });
        assert!(capabilities.check(Operation::Subscribe).is_ok());
    }

    #[test]
    fn capabilities_are_read_from_the_server() {
        let declared: ServerCapabilities = serde_json::from_value(serde_json::json!({
            "tools": {"listChanged": true},
            "resources": {"subscribe": true},
            "logging": {}
        }))
        .unwrap();
        let capabilities = Capabilities::from(&declared);
        assert_eq!(capabilities.tools, Some(true));
        assert_eq!(
            capabilities.resources,
            Some(ResourceCapabilities {
                subscribe: true,
                list_changed: false
            })
        );
        assert!(capabilities.logging);
        assert!(!capabilities.completions);
        assert_eq!(capabilities.prompts, None);
    }

    #[test]
    fn transport_labels() {
        assert_eq!(TransportKind::LegacySse.to_string(), "legacy SSE");
        assert_eq!(TransportKind::StreamableHttp.label(), "streamable HTTP");
        assert_eq!(TransportKind::Stdio.label(), "stdio");
    }
}
