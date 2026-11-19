//! `rustman.toml`: the workspace marker and its settings (design D5).

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::Deserialize;

use crate::agent::AgentSettings;
use crate::error::{ConfigError, LoadError, Position};
use crate::model::Variables;
use crate::safety::SafetySettings;

pub const SETTINGS_FILE: &str = "rustman.toml";

/// The only `rustman.toml` version this build reads.
pub const SETTINGS_VERSION: i64 = 1;

/// Known keys per section. `[variables]` accepts any key.
const KNOWN_KEYS: &[(&str, &[&str])] = &[
    ("workspace", &["name", "version"]),
    (
        "defaults",
        &[
            "timeout_ms",
            "follow_redirects",
            "max_redirects",
            "verify_tls",
            "default_environment",
        ],
    ),
    (
        "http",
        &[
            "ca_bundle",
            "proxy",
            "max_body_bytes",
            "max_download_bytes",
            "client_cert",
            "client_key",
            "client_pkcs12",
            "client_pkcs12_password",
        ],
    ),
    ("history", &["enabled", "max_entries"]),
    (
        "safety",
        &["allowed_hosts", "production_hosts", "rate", "concurrency"],
    ),
    ("explore", &["producers", "disabled_rules"]),
    // Read by rustman-webhook (add-webhook-inspector).
    ("webhook", &["host", "port", "verifiers"]),
    // Read by rustman-mock (add-openapi-mock-server).
    (
        "mock",
        &[
            "host",
            "port",
            "seed",
            "max_body_bytes",
            "max_headers",
            "max_header_bytes",
            "dir",
            "delay_ms",
            "jitter_ms",
            "max_items",
            "max_store_bytes",
            "security",
        ],
    ),
    ("graphql", &["endpoints"]),
    // Read by rustman-jsonrpc (add-jsonrpc-client).
    ("jsonrpc", &["catalogs", "agent_read_only_methods"]),
    (
        "agent",
        &[
            "allow_mutations",
            "allow_destructive",
            "require_confirmation",
            "allowed_hosts",
            "allowed_origins",
            "max_output_bytes",
            "max_jobs",
        ],
    ),
];

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Settings {
    pub workspace: WorkspaceSection,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub http: HttpDefaults,
    #[serde(default)]
    pub history: HistorySettings,
    #[serde(default)]
    pub safety: SafetySettings,
    #[serde(default)]
    pub explore: ExploreSettings,
    #[serde(default)]
    pub graphql: GraphqlSettings,
    #[serde(default)]
    pub jsonrpc: RpcSettings,
    #[serde(default)]
    pub agent: AgentSettings,
    #[serde(default)]
    pub variables: Variables,
}

/// `[jsonrpc]`: settings of the JSON-RPC client. Not `JsonrpcSettings`:
/// that name is the per-request `settings:` block of a `kind: jsonrpc`
/// file ([`crate::model::JsonrpcSettings`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RpcSettings {
    /// `[jsonrpc.catalogs]`: endpoint URL (as a request writes it, or
    /// resolved) to an OpenRPC document, relative to the workspace root.
    pub catalogs: BTreeMap<String, String>,
    /// Method globs agents may call through `rpc_call`; `@name` entries
    /// are presets other capabilities register. Empty: none.
    pub agent_read_only_methods: Vec<String>,
}

/// `[graphql]`: settings of the GraphQL client.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphqlSettings {
    /// `[[graphql.endpoints]]`: per-endpoint settings.
    pub endpoints: Vec<GraphqlEndpoint>,
}

/// One `[[graphql.endpoints]]` entry.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphqlEndpoint {
    /// The endpoint URL, as a request's `endpoint` is written or once resolved.
    pub url: String,
    /// An SDL or introspection JSON file, relative to the workspace root,
    /// used instead of the schema cache for this endpoint.
    pub schema_file: Option<String>,
}

/// `[explore]`: settings of OpenAPI exploration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExploreSettings {
    /// `[explore.producers]`: path parameter name to the operation that
    /// produces its value (`operationId` or `METHOD /path`), optionally
    /// followed by `#<json-pointer>` into its response body.
    pub producers: BTreeMap<String, String>,
    /// Finding rule ids that are never reported.
    pub disabled_rules: Vec<String>,
}

/// `[history]`: whether exchanges are recorded, and how many are kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct HistorySettings {
    pub enabled: bool,
    pub max_entries: usize,
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_entries: 500,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct WorkspaceSection {
    pub name: String,
    pub version: i64,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct Defaults {
    pub timeout_ms: u64,
    pub follow_redirects: bool,
    pub max_redirects: u32,
    pub verify_tls: bool,
    pub default_environment: Option<String>,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            timeout_ms: 30_000,
            follow_redirects: true,
            max_redirects: 10,
            verify_tls: true,
            default_environment: None,
        }
    }
}

/// `[http]`. The file paths are relative to the workspace root. The client
/// certificate (`client_cert` + `client_key`, or `client_pkcs12`) applies to
/// every request that sets none of its own.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct HttpDefaults {
    pub ca_bundle: Option<String>,
    pub proxy: Option<String>,
    pub max_body_bytes: u64,
    /// A response body kept as received (not decompressed) larger than this
    /// fails, so an endless stream cannot fill the disk.
    pub max_download_bytes: u64,
    pub client_cert: Option<String>,
    pub client_key: Option<String>,
    pub client_pkcs12: Option<String>,
    /// A `{{variable}}` template, resolved with the request's variables (the
    /// one `rustman.toml` value that is interpolated).
    pub client_pkcs12_password: Option<String>,
    /// Where each file path key was written in `rustman.toml`, for messages.
    #[serde(skip)]
    pub positions: BTreeMap<&'static str, Position>,
}

/// The `[http]` keys that name files.
pub const HTTP_PATH_KEYS: [&str; 4] = ["ca_bundle", "client_cert", "client_key", "client_pkcs12"];

impl HttpDefaults {
    /// The file path set under `key` (one of [`HTTP_PATH_KEYS`]).
    pub fn path(&self, key: &str) -> Option<&str> {
        match key {
            "ca_bundle" => self.ca_bundle.as_deref(),
            "client_cert" => self.client_cert.as_deref(),
            "client_key" => self.client_key.as_deref(),
            "client_pkcs12" => self.client_pkcs12.as_deref(),
            _ => None,
        }
    }
}

impl Default for HttpDefaults {
    fn default() -> Self {
        Self {
            ca_bundle: None,
            proxy: None,
            max_body_bytes: 10 * 1024 * 1024,
            max_download_bytes: 1024 * 1024 * 1024,
            client_cert: None,
            client_key: None,
            client_pkcs12: None,
            client_pkcs12_password: None,
            positions: BTreeMap::new(),
        }
    }
}

/// Hides `client_pkcs12_password`.
impl fmt::Debug for HttpDefaults {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let password = self.client_pkcs12_password.as_ref().map(|_| "<hidden>");
        f.debug_struct("HttpDefaults")
            .field("ca_bundle", &self.ca_bundle)
            .field("proxy", &self.proxy)
            .field("max_body_bytes", &self.max_body_bytes)
            .field("max_download_bytes", &self.max_download_bytes)
            .field("client_cert", &self.client_cert)
            .field("client_key", &self.client_key)
            .field("client_pkcs12", &self.client_pkcs12)
            .field("client_pkcs12_password", &password)
            .finish_non_exhaustive()
    }
}

impl Settings {
    /// Parse the contents of `rustman.toml`. Returns the settings and a warning
    /// for every unknown key.
    pub fn parse(file: &Path, text: &str) -> Result<(Self, Vec<String>), ConfigError> {
        let table: toml::Table = toml::from_str(text).map_err(|e| toml_error(file, text, &e))?;
        check_version(file, &table)?;
        let mut settings: Self = toml::from_str(text).map_err(|e| toml_error(file, text, &e))?;
        settings.http.positions = http_path_positions(text);
        Ok((settings, unknown_keys(&table)))
    }

    /// The settings of a request sent outside any workspace: every default,
    /// no variables.
    pub fn ad_hoc() -> Self {
        let text = "[workspace]\nname = \"ad-hoc\"\nversion = 1\n";
        Self::parse(Path::new(SETTINGS_FILE), text)
            .expect("the built-in settings parse")
            .0
    }

    /// Read and parse `rustman.toml` from the workspace root.
    pub fn load(root: &Path) -> Result<(Self, Vec<String>), ConfigError> {
        let file = root.join(SETTINGS_FILE);
        let text = std::fs::read_to_string(&file).map_err(|e| ConfigError::io(&file, e))?;
        Self::parse(Path::new(SETTINGS_FILE), &text)
    }
}

/// The positions of the `[http]` file paths, read in a second, span-keeping
/// pass.
fn http_path_positions(text: &str) -> BTreeMap<&'static str, Position> {
    #[derive(Deserialize)]
    struct Spans {
        http: Option<BTreeMap<String, toml::Spanned<toml::Value>>>,
    }
    let Ok(Spans { http: Some(http) }) = toml::from_str(text) else {
        return BTreeMap::new();
    };
    HTTP_PATH_KEYS
        .into_iter()
        .filter_map(|key| {
            let span = http.get(key)?.span();
            Some((key, position_at(text, span.start)))
        })
        .collect()
}

/// Check `[workspace] version`. A value of the wrong type is left to the
/// typed pass, which reports it with its line.
fn check_version(file: &Path, table: &toml::Table) -> Result<(), ConfigError> {
    let Some(version) = table.get("workspace").and_then(|w| w.get("version")) else {
        return Err(invalid(
            file,
            format!("missing [workspace] version = {SETTINGS_VERSION}"),
        ));
    };
    match version.as_integer() {
        None | Some(SETTINGS_VERSION) => Ok(()),
        Some(v) if v > SETTINGS_VERSION => Err(ConfigError::NewerSettingsVersion(v)),
        Some(v) => Err(invalid(
            file,
            format!("unsupported version {v} in [workspace] (expected {SETTINGS_VERSION})"),
        )),
    }
}

fn invalid(file: &Path, message: String) -> ConfigError {
    LoadError::new(file, None, message).into()
}

fn unknown_keys(table: &toml::Table) -> Vec<String> {
    let mut warnings = Vec::new();
    for (section, value) in table {
        if section == "variables" {
            continue;
        }
        let Some((_, known)) = KNOWN_KEYS.iter().find(|(name, _)| name == section) else {
            warnings.push(unknown_key_warning(section));
            continue;
        };
        let keys = value.as_table().into_iter().flat_map(|t| t.keys());
        for key in keys.filter(|key| !known.contains(&key.as_str())) {
            warnings.push(unknown_key_warning(&format!("{section}.{key}")));
        }
    }
    warnings
}

fn unknown_key_warning(key: &str) -> String {
    format!("{SETTINGS_FILE}: unknown key '{key}' ignored")
}

fn toml_error(file: &Path, text: &str, err: &toml::de::Error) -> ConfigError {
    let position = err.span().map(|span| position_at(text, span.start));
    LoadError::new(file, position, err.message().trim_end()).into()
}

fn position_at(text: &str, offset: usize) -> Position {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    Position { line, column }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<(Settings, Vec<String>), ConfigError> {
        Settings::parse(Path::new("rustman.toml"), text)
    }

    #[test]
    fn the_explore_table_is_read() {
        let text = "[workspace]\nname = \"shop\"\nversion = 1\n\n[explore]\ndisabled_rules = [\"error-leakage\"]\n\n[explore.producers]\norderId = \"createOrder#/id\"\n";
        let (settings, warnings) = parse(text).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(settings.explore.disabled_rules, ["error-leakage"]);
        assert_eq!(settings.explore.producers["orderId"], "createOrder#/id");
        let (settings, _) = parse("[workspace]\nname = \"shop\"\nversion = 1").unwrap();
        assert_eq!(settings.explore, ExploreSettings::default());
        assert!(
            parse("[workspace]\nname = \"a\"\nversion = 1\n[explore]\nproducer = {}\n").is_err()
        );
    }

    #[test]
    fn the_jsonrpc_table_is_read() {
        let text = "[workspace]\nname = \"a\"\nversion = 1\n\n[jsonrpc]\nagent_read_only_methods = [\"getblock*\"]\n\n[jsonrpc.catalogs]\n\"{{rpc_url}}\" = \"openrpc/node.json\"\n";
        let (settings, warnings) = parse(text).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(settings.jsonrpc.agent_read_only_methods, ["getblock*"]);
        assert_eq!(
            settings.jsonrpc.catalogs["{{rpc_url}}"],
            "openrpc/node.json"
        );
        let unknown = "[workspace]\nname = \"a\"\nversion = 1\n[jsonrpc]\ncatalog = \"x\"\n";
        assert!(parse(unknown).is_err());
    }

    #[test]
    fn the_graphql_table_is_read() {
        let text = "[workspace]\nname = \"a\"\nversion = 1\n\n[[graphql.endpoints]]\nurl = \"https://api.example.com/graphql\"\nschema_file = \"schemas/api.graphql\"\n";
        let (settings, warnings) = parse(text).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            settings.graphql.endpoints,
            [GraphqlEndpoint {
                url: "https://api.example.com/graphql".into(),
                schema_file: Some("schemas/api.graphql".into()),
            }]
        );
        let unknown = "[workspace]\nname = \"a\"\nversion = 1\n[[graphql.endpoints]]\nurl = \"x\"\nschema = \"y\"\n";
        assert!(parse(unknown).is_err());
    }

    #[test]
    fn minimal_settings_apply_every_default() {
        let (settings, warnings) = parse("[workspace]\nname = \"shop\"\nversion = 1").unwrap();
        assert!(warnings.is_empty());
        assert_eq!(settings.workspace.name, "shop");
        assert_eq!(settings.defaults, Defaults::default());
        assert_eq!(settings.defaults.timeout_ms, 30_000);
        assert!(settings.defaults.follow_redirects);
        assert_eq!(settings.defaults.max_redirects, 10);
        assert!(settings.defaults.verify_tls);
        assert_eq!(settings.http.max_body_bytes, 10 * 1024 * 1024);
        assert_eq!(settings.http.max_download_bytes, 1024 * 1024 * 1024);
        assert!(settings.variables.is_empty());
        assert_eq!(settings.history, HistorySettings::default());
        assert!(settings.history.enabled);
        assert_eq!(settings.history.max_entries, 500);
    }

    #[test]
    fn every_documented_key_is_read() {
        let text = r#"
[workspace]
name = "shop"
version = 1

[defaults]
timeout_ms = 5000
follow_redirects = false
max_redirects = 3
verify_tls = false
default_environment = "local"

[http]
ca_bundle = "certs/ca.pem"
proxy = "http://127.0.0.1:3128"
max_body_bytes = 1024
client_cert = "certs/client.pem"
client_key = "certs/client.key"
client_pkcs12 = "certs/client.p12"
client_pkcs12_password = "{{p12_password}}"
max_download_bytes = 4096

[history]
enabled = false
max_entries = 20

[variables]
base_url = "http://localhost"
"#;
        let (s, warnings) = parse(text).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(s.defaults.timeout_ms, 5000);
        assert!(!s.defaults.follow_redirects);
        assert_eq!(s.defaults.default_environment.as_deref(), Some("local"));
        assert_eq!(s.http.ca_bundle.as_deref(), Some("certs/ca.pem"));
        assert_eq!(s.http.max_body_bytes, 1024);
        assert_eq!(s.http.max_download_bytes, 4096);
        assert_eq!(s.http.client_cert.as_deref(), Some("certs/client.pem"));
        assert_eq!(s.http.path("client_key"), Some("certs/client.key"));
        assert_eq!(s.http.path("client_pkcs12"), Some("certs/client.p12"));
        assert_eq!(s.http.positions["ca_bundle"].line, 14);
        assert_eq!(s.http.positions["client_pkcs12"].line, 19);
        assert!(s.variables.get("base_url").is_some());
        assert_eq!(
            s.history,
            HistorySettings {
                enabled: false,
                max_entries: 20
            }
        );
        let debug = format!("{:?}", s.http);
        assert!(!debug.contains("p12_password"), "{debug}");
    }

    #[test]
    fn unknown_keys_warn_without_failing() {
        let text = "[workspace]\nname = \"a\"\nversion = 1\ncolour = 1\n[defaults]\nretries = 2\n[stress]\nx = 1\n";
        let (_, warnings) = parse(text).unwrap();
        assert_eq!(
            warnings,
            [
                "rustman.toml: unknown key 'defaults.retries' ignored",
                "rustman.toml: unknown key 'stress' ignored",
                "rustman.toml: unknown key 'workspace.colour' ignored",
            ]
        );
    }

    #[test]
    fn mock_keys_are_known() {
        let text = "[workspace]\nname = \"a\"\nversion = 1\n[mock]\nport = 4011\nseed = 7\nmax_body_bytes = 10\ndir = \"stubs\"\ndelay_ms = 5\njitter_ms = 1\nmax_items = 9\nmax_store_bytes = 99\n[mock.security]\nbearer = \"{{t}}\"\n";
        let (_, warnings) = parse(text).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn webhook_verifiers_are_a_known_key() {
        let text = "[workspace]\nname = \"a\"\nversion = 1\n[[webhook.verifiers]]\npath = \"/h\"\n";
        let (_, warnings) = parse(text).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn newer_version_fails() {
        let err = parse("[workspace]\nname = \"a\"\nversion = 2\n[defaults]\ntimeout_ms = \"x\"")
            .unwrap_err();
        assert!(matches!(err, ConfigError::NewerSettingsVersion(2)));
        assert_eq!(
            err.to_string(),
            "rustman.toml version 2 is newer than this rustman supports (1)"
        );
    }

    #[test]
    fn missing_or_old_version_fails() {
        let err = parse("[workspace]\nname = \"a\"\n").unwrap_err();
        assert_eq!(
            err.to_string(),
            "rustman.toml: missing [workspace] version = 1"
        );
        let err = parse("[workspace]\nname = \"a\"\nversion = 0\n").unwrap_err();
        assert_eq!(
            err.to_string(),
            "rustman.toml: unsupported version 0 in [workspace] (expected 1)"
        );
    }

    #[test]
    fn type_errors_report_the_line() {
        let err =
            parse("[workspace]\nname = \"a\"\nversion = 1\n[defaults]\ntimeout_ms = \"slow\"\n")
                .unwrap_err();
        let ConfigError::Invalid(errors) = &err else {
            panic!("{err}")
        };
        assert_eq!(errors[0].line(), Some(5), "{err}");
    }

    #[test]
    fn a_non_integer_version_is_a_type_error_with_its_line() {
        let err = parse("[workspace]\nname = \"a\"\nversion = \"2\"\n").unwrap_err();
        let ConfigError::Invalid(errors) = &err else {
            panic!("{err}")
        };
        assert_eq!(errors[0].line(), Some(3), "{err}");
        assert!(!err.to_string().contains("missing"), "{err}");
    }
}
