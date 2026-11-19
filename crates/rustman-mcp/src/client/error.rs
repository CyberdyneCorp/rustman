//! Errors of the MCP client. Every message starts with `mcp:` (or names the
//! file at fault), except the user's own `cancelled by user`, and never
//! contains a secret value.

use rustman_core::LoadError;
use rustman_core::trust::TrustError;
use rustman_core::vars::ResolveError;

use super::schema::Violation;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("mcp: server {0} must define either command or url, not both")]
    Ambiguous(String),
    #[error("mcp: server {0} must define command or url")]
    NoTransport(String),
    #[error("mcp: server {name}: {message}")]
    InvalidDefinition { name: String, message: String },
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error("mcp: unknown server {0}")]
    UnknownServer(String),
    #[error("mcp: server {name}: {source}")]
    Resolve {
        name: String,
        #[source]
        source: ResolveError,
    },
    #[error("mcp: server {0} is not trusted; review it or pass --trust")]
    Untrusted(String),
    #[error(transparent)]
    Trust(#[from] TrustError),
    #[error("mcp: cannot start server {name}: {message}")]
    Spawn { name: String, message: String },
    #[error("mcp: unsupported protocol version {0}")]
    UnsupportedVersion(String),
    /// Initialization or the transport failed; the message is redacted.
    #[error("mcp: cannot connect to server {name}: {message}")]
    Connect { name: String, message: String },
    #[error("mcp: server {0} is disconnected")]
    Disconnected(String),
    /// The operation needs a capability the server did not declare.
    #[error("mcp: {0}")]
    Disabled(String),
    /// A JSON-RPC error answered by the server; the message is redacted.
    #[error("mcp: protocol error {code}: {message}")]
    Protocol { code: i32, message: String },
    /// The transport failed mid-session; the message is redacted.
    #[error("mcp: server {name}: {message}")]
    Transport { name: String, message: String },
    #[error("mcp: request timed out after {0}")]
    Timeout(String),
    #[error("cancelled by user")]
    Cancelled,
    #[error("mcp: {0} returned more than 100 pages")]
    TooManyPages(String),
    #[error("mcp: unknown tool {0}")]
    UnknownTool(String),
    /// Arguments that break the tool's `inputSchema` (or a prompt's
    /// required arguments): the call was not sent.
    #[error("mcp: invalid arguments: {}", join(.0))]
    InvalidArguments(Vec<Violation>),
    #[error("mcp: {0}")]
    Invalid(String),
    #[error("mcp: oauth requires client_id for {0}")]
    OAuthClientId(String),
    /// The OAuth flow failed; the message is redacted.
    #[error("mcp: oauth: {0}")]
    OAuth(String),
}

impl ClientError {
    /// How a result view labels the error: `protocol error <code>` for
    /// JSON-RPC errors.
    pub fn label(&self) -> String {
        match self {
            Self::Protocol { code, .. } => format!("protocol error {code}"),
            Self::Cancelled => "cancelled".to_owned(),
            _ => "error".to_owned(),
        }
    }
}

fn join(violations: &[Violation]) -> String {
    violations
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}
