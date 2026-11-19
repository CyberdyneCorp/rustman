//! The `<server>` argument of `rustman mcp-client` (mcp-client "MCP client
//! CLI"): a server the workspace defines, `stdio:<command line>`, or an
//! `http(s)://` URL. An ad-hoc server gets a definition of its own; its
//! command line is the user's own, typed for this one invocation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rustman_core::trust::TrustStore;
use rustman_core::vars::Resolver;

use super::definition::{
    HttpDefinition, ServerDefinition, StdioDefinition, TransportDefinition, find_server,
};
use super::error::ClientError;
use super::resolve::ResolvedServer;
use super::trust::{Approved, approve_typed, require_trust};

/// Where `rustman mcp-client` connects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerTarget {
    /// A server of the workspace, by name.
    Named(String),
    /// A command line typed after `stdio:`.
    Stdio(Vec<String>),
    /// An `http://` or `https://` URL.
    Url(String),
}

/// The name of ad-hoc stdio servers (history, messages).
pub const AD_HOC_STDIO: &str = "stdio";

impl ServerTarget {
    pub fn parse(text: &str) -> Result<Self, ClientError> {
        if let Some(command) = text.strip_prefix("stdio:") {
            let words = shell_words::split(command)
                .map_err(|e| ClientError::Invalid(format!("stdio:{command}: {e}")))?;
            if words.is_empty() {
                return Err(ClientError::Invalid("stdio: needs a command line".into()));
            }
            return Ok(Self::Stdio(words));
        }
        let lower = text.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            return Ok(Self::Url(text.to_owned()));
        }
        Ok(Self::Named(text.to_owned()))
    }

    /// The definition of an ad-hoc server; `None` for a named one.
    pub fn ad_hoc(&self) -> Option<ServerDefinition> {
        let (name, transport) = match self {
            Self::Named(_) => return None,
            Self::Stdio(words) => (
                AD_HOC_STDIO.to_owned(),
                TransportDefinition::Stdio(StdioDefinition {
                    command: words[0].clone(),
                    args: words[1..].to_vec(),
                    env: BTreeMap::new(),
                    cwd: None,
                }),
            ),
            Self::Url(url) => (
                url_name(url),
                TransportDefinition::Http(HttpDefinition {
                    url: url.clone(),
                    headers: BTreeMap::new(),
                    auth: None,
                    oauth: None,
                }),
            ),
        };
        Some(ServerDefinition {
            name,
            transport,
            roots: None,
            timeout_ms: None,
            source: PathBuf::from("<command line>"),
        })
    }
}

impl ServerTarget {
    /// The definition of this target: the workspace's (`root`, when
    /// `in_workspace`) for a name, else the ad-hoc one.
    pub fn definition(
        &self,
        root: &Path,
        in_workspace: bool,
    ) -> Result<ServerDefinition, ClientError> {
        match (self, self.ad_hoc()) {
            (Self::Named(name), _) if in_workspace => find_server(root, name),
            (_, Some(definition)) => Ok(definition),
            (_, None) => Err(ClientError::UnknownServer(self.to_string())),
        }
    }

    /// The server to connect to, resolved through `resolver`, with its
    /// approval: a named stdio server needs this machine's trust (or
    /// `trust`, which records it); an ad-hoc command line is the user's own.
    /// `root` is the workspace root (for an ad-hoc server outside a
    /// workspace, the current folder).
    pub fn resolve(
        &self,
        root: &Path,
        in_workspace: bool,
        resolver: &Resolver<'_>,
        trust: bool,
    ) -> Result<(ResolvedServer, Approved), ClientError> {
        let server = self
            .definition(root, in_workspace)?
            .resolve(resolver, root)?;
        let approved = match self {
            Self::Named(_) => require_trust(&mut TrustStore::load(root)?, &server, trust)?,
            Self::Stdio(_) | Self::Url(_) => approve_typed(&server),
        };
        Ok((server, approved))
    }
}

impl std::fmt::Display for ServerTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Named(name) => f.write_str(name),
            Self::Stdio(words) => write!(f, "stdio:{}", shell_words::join(words)),
            Self::Url(url) => f.write_str(url),
        }
    }
}

/// The host of `url` (with its port): the name its OAuth tokens are kept
/// under, so `rustman mcp-client logout <url>` finds them.
fn url_name(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| {
            let host = u.host_str()?.to_owned();
            Some(match u.port() {
                Some(port) => format!("{host}-{port}"),
                None => host,
            })
        })
        .unwrap_or_else(|| "http".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_are_names_command_lines_or_urls() {
        assert_eq!(
            ServerTarget::parse("files").unwrap(),
            ServerTarget::Named("files".into())
        );
        assert_eq!(
            ServerTarget::parse("stdio:npx -y 'my server'").unwrap(),
            ServerTarget::Stdio(vec!["npx".into(), "-y".into(), "my server".into()])
        );
        assert_eq!(
            ServerTarget::parse("HTTPS://mcp.example.com/mcp").unwrap(),
            ServerTarget::Url("HTTPS://mcp.example.com/mcp".into())
        );
        assert!(ServerTarget::parse("stdio:  ").is_err());
    }

    #[test]
    fn ad_hoc_servers_get_a_definition() {
        let url = ServerTarget::Url("http://127.0.0.1:8080/mcp".into());
        let definition = url.ad_hoc().unwrap();
        assert_eq!(definition.name, "127.0.0.1-8080");
        assert_eq!(definition.transport_name(), "http");
        let stdio = ServerTarget::parse("stdio:server --flag")
            .unwrap()
            .ad_hoc()
            .unwrap();
        let TransportDefinition::Stdio(stdio) = stdio.transport else {
            panic!("a stdio definition");
        };
        assert_eq!((stdio.command.as_str(), stdio.args.len()), ("server", 1));
        assert!(ServerTarget::Named("x".into()).ad_hoc().is_none());
    }
}
