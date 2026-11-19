//! A server definition with its `{{variable}}` templates resolved, ready to
//! connect. The secrets substituted are kept so every output of the session
//! can be redacted.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rustman_core::Redactor;
use rustman_core::model::Auth;
use rustman_core::vars::{
    Interpolator, Resolved, ResolvedPair, Resolver, UsedSecret, resolve_auth,
};

use super::definition::{HttpDefinition, ServerDefinition, StdioDefinition, TransportDefinition};
use super::error::ClientError;

#[derive(Clone, Debug)]
pub struct ResolvedServer {
    pub name: String,
    pub transport: ResolvedTransport,
    /// The definition's `roots`, still as written (answered by `roots/list`).
    pub roots: Option<Vec<String>>,
    /// The workspace root: the default root and cwd.
    pub workspace: PathBuf,
    pub timeout: Option<Duration>,
    /// Every secret substituted anywhere in the definition.
    pub secrets: Vec<UsedSecret>,
}

#[derive(Clone, Debug)]
pub enum ResolvedTransport {
    Stdio(ResolvedStdio),
    Http(ResolvedHttp),
}

#[derive(Clone, Debug)]
pub struct ResolvedStdio {
    pub command: Resolved,
    pub args: Vec<Resolved>,
    pub env: Vec<(String, Resolved)>,
    /// The working directory: `cwd` under the workspace root, else the root.
    pub cwd: PathBuf,
    /// `cwd` as resolved from the definition, `None` for the default.
    pub cwd_written: Option<Resolved>,
}

#[derive(Clone, Debug)]
pub struct ResolvedHttp {
    pub url: Resolved,
    pub headers: Vec<ResolvedPair>,
    pub auth: Option<Auth>,
    pub oauth: ResolvedOAuthClient,
}

/// The definition's OAuth client, resolved; empty without `oauth`.
#[derive(Clone, Debug, Default)]
pub struct ResolvedOAuthClient {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub scopes: Vec<String>,
}

impl ResolvedServer {
    /// A redactor for everything the session shows: logs, errors, stderr.
    pub fn redactor(&self) -> Redactor {
        let mut redactor = Redactor::new();
        for secret in &self.secrets {
            redactor.add(secret);
        }
        if let ResolvedTransport::Http(http) = &self.transport {
            for header in &http.headers {
                redactor.add_header_value(&header.name.text, &header.value.text);
            }
            if let Some(auth) = &http.auth {
                redactor.add_auth(auth);
            }
            if let Some(secret) = &http.oauth.client_secret {
                redactor.add_value("oauth_client_secret", secret);
            }
        }
        redactor
    }

    pub fn transport_name(&self) -> &'static str {
        match self.transport {
            ResolvedTransport::Stdio(_) => "stdio",
            ResolvedTransport::Http(_) => "http",
        }
    }
}

impl ServerDefinition {
    /// Resolve every template through `resolver`; relative paths are taken
    /// from the workspace `root`. Every unresolved name is reported at once.
    pub fn resolve(
        &self,
        resolver: &Resolver<'_>,
        root: &Path,
    ) -> Result<ResolvedServer, ClientError> {
        let mut i = resolver.interpolator();
        let transport = match &self.transport {
            TransportDefinition::Stdio(stdio) => {
                ResolvedTransport::Stdio(stdio_of(&mut i, stdio, root))
            }
            TransportDefinition::Http(http) => ResolvedTransport::Http(http_of(&mut i, http)),
        };
        let (transport, secrets) = i.finish(transport).map_err(|source| ClientError::Resolve {
            name: self.name.clone(),
            source,
        })?;
        Ok(ResolvedServer {
            name: self.name.clone(),
            transport,
            roots: self.roots.clone(),
            workspace: root.to_path_buf(),
            timeout: self.timeout_ms.map(Duration::from_millis),
            secrets,
        })
    }
}

fn stdio_of(i: &mut Interpolator<'_, '_>, stdio: &StdioDefinition, root: &Path) -> ResolvedStdio {
    let cwd_written = stdio.cwd.as_deref().map(|cwd| i.text(cwd));
    let cwd = cwd_written
        .as_ref()
        .map_or_else(|| root.to_path_buf(), |cwd| root.join(&cwd.text));
    ResolvedStdio {
        command: i.text(&stdio.command),
        args: stdio.args.iter().map(|a| i.text(a)).collect(),
        env: stdio
            .env
            .iter()
            .map(|(name, value)| (name.clone(), i.text(value)))
            .collect(),
        cwd,
        cwd_written,
    }
}

fn http_of(i: &mut Interpolator<'_, '_>, http: &HttpDefinition) -> ResolvedHttp {
    ResolvedHttp {
        url: i.text(&http.url),
        headers: http
            .headers
            .iter()
            .map(|(name, value)| ResolvedPair {
                name: Resolved::literal(name.clone()),
                value: i.text(value),
            })
            .collect(),
        auth: http.auth.as_ref().map(|auth| resolve_auth(i, auth)),
        oauth: http
            .oauth
            .as_ref()
            .map(|o| ResolvedOAuthClient {
                client_id: o.client_id.as_deref().map(|c| i.text(c).text),
                client_secret: o.client_secret.as_deref().map(|c| i.text(c).text),
                scopes: o.scopes.clone(),
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use rustman_core::model::{VarValue, Variables};
    use rustman_core::vars::{Layer, VariablesSource};

    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Variables {
        Variables::new(
            pairs
                .iter()
                .map(|(n, v)| ((*n).to_owned(), VarValue::String((*v).to_owned())))
                .collect(),
        )
    }

    fn stdio(args: &[&str]) -> ServerDefinition {
        ServerDefinition {
            name: "files".into(),
            transport: TransportDefinition::Stdio(StdioDefinition {
                command: "{{tool}}".into(),
                args: args.iter().map(|a| (*a).to_owned()).collect(),
                env: BTreeMap::from([("TOKEN".to_owned(), "{{token}}".to_owned())]),
                cwd: Some("sub".into()),
            }),
            roots: None,
            timeout_ms: Some(1500),
            source: PathBuf::from("rustman.toml"),
        }
    }

    #[test]
    fn every_string_is_interpolated() {
        let values = vars(&[("tool", "node"), ("dir", "src"), ("token", "t0k")]);
        let resolver = Resolver::new().with(VariablesSource::new(Layer::Settings, &values));
        let root = Path::new("/w");
        let server = stdio(&["serve", "{{dir}}"])
            .resolve(&resolver, root)
            .unwrap();
        let ResolvedTransport::Stdio(s) = &server.transport else {
            panic!("expected stdio");
        };
        assert_eq!(s.command.text, "node");
        assert_eq!(s.args[1].text, "src");
        assert_eq!(s.env[0].1.text, "t0k");
        assert_eq!(s.cwd, root.join("sub"));
        assert_eq!(server.timeout, Some(Duration::from_millis(1500)));
    }

    #[test]
    fn every_missing_variable_is_reported() {
        let resolver = Resolver::new();
        let err = stdio(&["{{dir}}"])
            .resolve(&resolver, Path::new("/w"))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "mcp: server files: unresolved variable(s): tool, dir, token"
        );
    }

    #[test]
    fn http_headers_and_auth_are_interpolated() {
        let values = vars(&[("base", "http://h"), ("team", "a"), ("t", "s3cr3t")]);
        let resolver = Resolver::new().with(VariablesSource::new(Layer::Settings, &values));
        let definition = ServerDefinition {
            name: "remote".into(),
            transport: TransportDefinition::Http(HttpDefinition {
                url: "{{base}}/mcp".into(),
                headers: BTreeMap::from([("X-Team".to_owned(), "{{team}}".to_owned())]),
                auth: Some(Auth::Bearer {
                    token: "{{t}}".into(),
                }),
                oauth: None,
            }),
            roots: None,
            timeout_ms: None,
            source: PathBuf::from("rustman.toml"),
        };
        let server = definition.resolve(&resolver, Path::new("/w")).unwrap();
        let ResolvedTransport::Http(http) = &server.transport else {
            panic!("expected http");
        };
        assert_eq!(http.url.text, "http://h/mcp");
        assert_eq!(http.headers[0].value.text, "a");
        assert!(matches!(&http.auth, Some(Auth::Bearer { token }) if token == "s3cr3t"));
        assert!(!server.redactor().text("token s3cr3t").contains("s3cr3t"));
    }
}
