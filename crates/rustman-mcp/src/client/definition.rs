//! MCP server definitions (mcp-client spec "MCP server definitions"): by
//! name under `[mcp_servers.<name>]` in `rustman.toml`, or one per file in
//! `mcp/servers/<name>.yaml`. Values are kept as written; `{{variable}}`
//! templates are resolved when a session is opened ([`super::resolve`]).

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use rustman_core::format::from_yaml;
use rustman_core::model::Auth;
use rustman_core::settings::SETTINGS_FILE;
use rustman_core::{LoadError, Position};
use serde::Deserialize;

use super::error::ClientError;

/// The folder of one-file-per-server definitions, under the workspace root.
pub const SERVERS_DIR: &str = "mcp/servers";

/// A named MCP server, as written in the workspace.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerDefinition {
    pub name: String,
    pub transport: TransportDefinition,
    /// `roots`: the roots answered to `roots/list`; `None` means the
    /// workspace root.
    pub roots: Option<Vec<String>>,
    /// `timeout_ms`: the request timeout of this server.
    pub timeout_ms: Option<u64>,
    /// The file it was read from, relative to the workspace root.
    pub source: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransportDefinition {
    Stdio(StdioDefinition),
    Http(HttpDefinition),
}

/// A server spawned as a child process speaking JSON-RPC over stdio.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StdioDefinition {
    pub command: String,
    pub args: Vec<String>,
    /// Set over the few variables inherited from ours (see
    /// [`super::stdio::INHERITED_ENV`]).
    pub env: BTreeMap<String, String>,
    /// Relative to the workspace root, which is also the default.
    pub cwd: Option<String>,
}

/// A server reached over Streamable HTTP (or legacy HTTP+SSE).
#[derive(Clone, Debug, PartialEq)]
pub struct HttpDefinition {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub auth: Option<Auth>,
    /// `oauth`: the client to use when the server asks for OAuth and its
    /// authorization server offers no dynamic registration.
    pub oauth: Option<OAuthClient>,
}

/// A pre-registered OAuth client (`[mcp_servers.<name>.oauth]`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OAuthClient {
    pub client_id: Option<String>,
    /// A confidential client's secret; use a `{{secret}}` template.
    pub client_secret: Option<String>,
    pub scopes: Vec<String>,
}

impl ServerDefinition {
    /// `stdio` or `http`, as listed to the user.
    pub fn transport_name(&self) -> &'static str {
        match self.transport {
            TransportDefinition::Stdio(_) => "stdio",
            TransportDefinition::Http(_) => "http",
        }
    }
}

/// One definition as written, before its transport is decided.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawServer {
    command: Option<String>,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    cwd: Option<String>,
    url: Option<String>,
    headers: BTreeMap<String, String>,
    auth: Option<Auth>,
    oauth: Option<OAuthClient>,
    roots: Option<Vec<String>>,
    timeout_ms: Option<u64>,
}

impl RawServer {
    fn into_definition(self, name: &str, source: &Path) -> Result<ServerDefinition, ClientError> {
        check_name(name)?;
        let transport = match (self.command.clone(), self.url.clone()) {
            (Some(_), Some(_)) => return Err(ClientError::Ambiguous(name.to_owned())),
            (None, None) => return Err(ClientError::NoTransport(name.to_owned())),
            (Some(command), None) => self.stdio(name, command)?,
            (None, Some(url)) => self.http(name, url)?,
        };
        Ok(ServerDefinition {
            name: name.to_owned(),
            transport,
            roots: self.roots,
            timeout_ms: self.timeout_ms,
            source: source.to_path_buf(),
        })
    }

    fn stdio(&self, name: &str, command: String) -> Result<TransportDefinition, ClientError> {
        if !self.headers.is_empty() || self.auth.is_some() || self.oauth.is_some() {
            return Err(invalid(
                name,
                "headers, auth and oauth apply to url servers only",
            ));
        }
        if command.trim().is_empty() {
            return Err(invalid(name, "command is empty"));
        }
        Ok(TransportDefinition::Stdio(StdioDefinition {
            command,
            args: self.args.clone(),
            env: self.env.clone(),
            cwd: self.cwd.clone(),
        }))
    }

    fn http(&self, name: &str, url: String) -> Result<TransportDefinition, ClientError> {
        if !self.args.is_empty() || !self.env.is_empty() || self.cwd.is_some() {
            return Err(invalid(
                name,
                "args, env and cwd apply to command servers only",
            ));
        }
        Ok(TransportDefinition::Http(HttpDefinition {
            url,
            headers: self.headers.clone(),
            auth: self.auth.clone(),
            oauth: self.oauth.clone(),
        }))
    }
}

fn invalid(name: &str, message: &str) -> ClientError {
    ClientError::InvalidDefinition {
        name: name.to_owned(),
        message: message.to_owned(),
    }
}

/// Names are used in file names and trust subjects: letters, digits, `-`,
/// `_` and `.`, not starting with `.`.
fn check_name(name: &str) -> Result<(), ClientError> {
    let valid = !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if valid {
        Ok(())
    } else {
        Err(invalid(
            name,
            "names may only use letters, digits, '-', '_' and '.'",
        ))
    }
}

/// Every server of the workspace at `root`, by name.
pub fn load_servers(root: &Path) -> Result<BTreeMap<String, ServerDefinition>, ClientError> {
    let mut servers = from_settings(root)?;
    for (name, file) in server_files(root)? {
        let text = fs::read_to_string(root.join(&file))
            .map_err(|e| LoadError::new(&file, None, e.to_string()))?;
        let raw: RawServer = from_yaml(&file, &text)?;
        if servers.contains_key(&name) {
            return Err(invalid(
                &name,
                &format!("defined in both {SETTINGS_FILE} and {}", slash_path(&file)),
            ));
        }
        let definition = raw.into_definition(&name, &file)?;
        servers.insert(name, definition);
    }
    Ok(servers)
}

/// The server named `name` of the workspace at `root`.
pub fn find_server(root: &Path, name: &str) -> Result<ServerDefinition, ClientError> {
    load_servers(root)?
        .remove(name)
        .ok_or_else(|| ClientError::UnknownServer(name.to_owned()))
}

/// The `[mcp_servers]` table of `rustman.toml`; absent file or table is empty.
fn from_settings(root: &Path) -> Result<BTreeMap<String, ServerDefinition>, ClientError> {
    let file = Path::new(SETTINGS_FILE);
    let text = match fs::read_to_string(root.join(file)) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(LoadError::new(file, None, e.to_string()).into()),
    };
    parse_settings(&text)?
        .into_iter()
        .map(|(name, raw)| {
            let definition = raw.into_definition(&name, file)?;
            Ok((name, definition))
        })
        .collect()
}

fn parse_settings(text: &str) -> Result<BTreeMap<String, RawServer>, LoadError> {
    #[derive(Deserialize)]
    struct Section {
        #[serde(default)]
        mcp_servers: BTreeMap<String, RawServer>,
    }
    toml::from_str::<Section>(text)
        .map(|s| s.mcp_servers)
        .map_err(|e| {
            let position = e.span().map(|span| position_at(text, span.start));
            LoadError::new(Path::new(SETTINGS_FILE), position, e.message())
        })
}

fn position_at(text: &str, offset: usize) -> Position {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let column = before.len() - before.rfind('\n').map_or(0, |i| i + 1) + 1;
    Position { line, column }
}

/// `mcp/servers/*.yaml` (and `.yml`), sorted, as (name, relative path).
fn server_files(root: &Path) -> Result<Vec<(String, PathBuf)>, ClientError> {
    let dir = root.join(SERVERS_DIR);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(LoadError::new(Path::new(SERVERS_DIR), None, e.to_string()).into()),
    };
    let mut files: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let yaml = matches!(path.extension()?.to_str()?, "yaml" | "yml");
            let name = path.file_stem()?.to_str()?.to_owned();
            yaml.then(|| (name, Path::new(SERVERS_DIR).join(entry.file_name())))
        })
        .collect();
    files.sort();
    Ok(files)
}

/// A relative path with `/` separators on every platform, for messages.
fn slash_path(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(toml: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let settings = format!("[workspace]\nname = \"w\"\nversion = 1\n{toml}");
        fs::write(dir.path().join(SETTINGS_FILE), settings).unwrap();
        dir
    }

    fn server_file(dir: &tempfile::TempDir, name: &str, yaml: &str) {
        let folder = dir.path().join(SERVERS_DIR);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join(format!("{name}.yaml")), yaml).unwrap();
    }

    #[test]
    fn stdio_server_definition() {
        let dir = workspace(
            "[mcp_servers.files]\ncommand = \"npx\"\nargs = [\"-y\", \"@modelcontextprotocol/server-filesystem\", \".\"]\n",
        );
        let servers = load_servers(dir.path()).unwrap();
        let files = &servers["files"];
        assert_eq!(files.transport_name(), "stdio");
        let TransportDefinition::Stdio(stdio) = &files.transport else {
            panic!("expected stdio");
        };
        assert_eq!(stdio.command, "npx");
        assert_eq!(
            stdio.args,
            ["-y", "@modelcontextprotocol/server-filesystem", "."]
        );
        assert_eq!(files.source, Path::new(SETTINGS_FILE));
    }

    #[test]
    fn ambiguous_definition_fails() {
        let dir = workspace("[mcp_servers.both]\ncommand = \"x\"\nurl = \"http://h/mcp\"\n");
        let err = load_servers(dir.path()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "mcp: server both must define either command or url, not both"
        );
        let dir = workspace("");
        server_file(&dir, "both", "command: x\nurl: http://h/mcp\n");
        let err = load_servers(dir.path()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "mcp: server both must define either command or url, not both"
        );
    }

    #[test]
    fn yaml_files_define_http_servers() {
        let dir = workspace("");
        server_file(
            &dir,
            "remote",
            "url: \"{{base}}/mcp\"\nheaders:\n  X-Team: \"{{team}}\"\nauth:\n  type: bearer\n  token: \"{{token}}\"\ntimeout_ms: 5000\n",
        );
        let servers = load_servers(dir.path()).unwrap();
        let remote = &servers["remote"];
        assert_eq!(remote.transport_name(), "http");
        assert_eq!(remote.timeout_ms, Some(5000));
        let TransportDefinition::Http(http) = &remote.transport else {
            panic!("expected http");
        };
        assert_eq!(http.url, "{{base}}/mcp");
        assert_eq!(http.headers["X-Team"], "{{team}}");
        assert!(matches!(&http.auth, Some(Auth::Bearer { token }) if token == "{{token}}"));
        assert_eq!(remote.source, Path::new(SERVERS_DIR).join("remote.yaml"));
    }

    #[test]
    fn toml_auth_is_read() {
        let dir = workspace(
            "[mcp_servers.remote]\nurl = \"http://h/mcp\"\nauth = { type = \"bearer\", token = \"{{t}}\" }\n",
        );
        let servers = load_servers(dir.path()).unwrap();
        let TransportDefinition::Http(http) = &servers["remote"].transport else {
            panic!("expected http");
        };
        assert!(matches!(&http.auth, Some(Auth::Bearer { token }) if token == "{{t}}"));
    }

    #[test]
    fn missing_transport_and_misplaced_keys_fail() {
        let dir = workspace("[mcp_servers.none]\nargs = [\"a\"]\n");
        let err = load_servers(dir.path()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "mcp: server none must define command or url"
        );
        let dir = workspace("[mcp_servers.web]\nurl = \"http://h\"\ncwd = \"x\"\n");
        let err = load_servers(dir.path()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "mcp: server web: args, env and cwd apply to command servers only"
        );
        let dir = workspace("[mcp_servers.cmd]\ncommand = \"x\"\nheaders = { A = \"b\" }\n");
        let err = load_servers(dir.path()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "mcp: server cmd: headers, auth and oauth apply to url servers only"
        );
    }

    #[test]
    fn an_oauth_client_is_read() {
        let dir = workspace(
            "[mcp_servers.web]\nurl = \"http://h\"\n[mcp_servers.web.oauth]\nclient_id = \"app\"\nscopes = [\"read\"]\n",
        );
        let servers = load_servers(dir.path()).unwrap();
        let TransportDefinition::Http(http) = &servers["web"].transport else {
            panic!("an http server");
        };
        let oauth = http.oauth.as_ref().unwrap();
        assert_eq!(oauth.client_id.as_deref(), Some("app"));
        assert_eq!(oauth.scopes, ["read"]);
    }

    #[test]
    fn unknown_keys_and_bad_names_fail() {
        let dir = workspace("[mcp_servers.files]\ncommand = \"x\"\ncomand = \"y\"\n");
        let err = load_servers(dir.path()).unwrap_err().to_string();
        assert!(err.starts_with("rustman.toml:"), "{err}");
        assert!(err.contains("comand"), "{err}");
        let dir = workspace("[mcp_servers.\"a b\"]\ncommand = \"x\"\n");
        let err = load_servers(dir.path()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "mcp: server a b: names may only use letters, digits, '-', '_' and '.'"
        );
    }

    #[test]
    fn duplicate_names_fail() {
        let dir = workspace("[mcp_servers.files]\ncommand = \"x\"\n");
        server_file(&dir, "files", "command: y\n");
        let err = load_servers(dir.path()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "mcp: server files: defined in both rustman.toml and mcp/servers/files.yaml"
        );
    }

    #[test]
    fn find_server_names_the_unknown_one() {
        let dir = workspace("");
        let err = find_server(dir.path(), "nope").unwrap_err();
        assert_eq!(err.to_string(), "mcp: unknown server nope");
    }
}
