//! `[secret_providers.<name>]` in `rustman.toml` and in the user config
//! (secret-providers spec "Provider configuration", design D6).
//!
//! The workspace is untrusted input: it may hold no credential, no `cmd`
//! provider and no binary path. Those come from the user config only.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use super::cache::DEFAULT_TTL;
use crate::error::{ConfigError, LoadError};
use crate::settings::SETTINGS_FILE;

/// The file, in the user config directory, that may define providers.
pub const USER_CONFIG_FILE: &str = "config.toml";

/// The default time a fetch may take (`timeout_ms`).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Keys every provider type accepts.
const COMMON_KEYS: &[&str] = &["type", "timeout_ms", "cache_ttl_s"];

/// Keys that look like credentials: never accepted in `rustman.toml`.
pub const CREDENTIAL_KEYS: &[&str] = &[
    "token",
    "password",
    "secret",
    "secret_id",
    "client_secret",
    "private_key",
];

/// Parts of a key name that mark a credential whatever its exact spelling
/// (`secret_access_key`, `api_key`, `auth_token`, `Token`).
const CREDENTIAL_PARTS: &[&str] = &[
    "token",
    "password",
    "passwd",
    "secret",
    "private_key",
    "api_key",
    "apikey",
    "access_key",
    "credential",
];

/// Whether `key` names a credential: one of [`CREDENTIAL_KEYS`], or any key
/// containing a [`CREDENTIAL_PARTS`] word, ignoring case and `-`.
pub fn is_credential_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace('-', "_");
    CREDENTIAL_KEYS.contains(&key.as_str()) || CREDENTIAL_PARTS.iter().any(|p| key.contains(p))
}

/// Keys that make Rustman run a program, or send cloud credentials to
/// another host (`endpoint`, for emulators and tests): user config only.
pub const USER_ONLY_KEYS: &[&str] = &["command", "op_path", "gpg_path", "endpoint"];

/// Every provider type with its own keys, and the cargo feature it needs
/// (`None`: always built).
pub const TYPES: &[(&str, &[&str], Option<&str>)] = &[
    ("keychain", &[], None),
    ("env", &[], None),
    ("sops", &["gpg_path"], None),
    ("onepassword", &["connect_host", "op_path", "account"], None),
    (
        "vault",
        &["address", "namespace", "auth", "mount", "role", "oidc_port"],
        None,
    ),
    ("cmd", &["command"], None),
    (
        "aws-secrets-manager",
        &["region", "profile", "endpoint"],
        Some("secrets-aws"),
    ),
    (
        "aws-ssm",
        &["region", "profile", "endpoint"],
        Some("secrets-aws"),
    ),
    (
        "gcp-secret-manager",
        &["project", "endpoint"],
        Some("secrets-gcp"),
    ),
    (
        "azure-key-vault",
        &["vault", "endpoint"],
        Some("secrets-azure"),
    ),
];

/// Where a provider was defined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Workspace,
    User,
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Workspace => "workspace",
            Self::User => "user",
        })
    }
}

/// One provider's configuration: its type, timeout, cache TTL and its
/// type-specific settings (never credentials).
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderConfig {
    pub name: String,
    pub kind: String,
    pub origin: Origin,
    pub timeout: Duration,
    pub ttl: Duration,
    pub options: toml::Table,
    /// Keys a user table set over a workspace provider (`origin` stays
    /// `Workspace`).
    pub user_keys: Vec<String>,
}

impl ProviderConfig {
    /// The string setting `key`, if set.
    pub fn string(&self, key: &str) -> Option<&str> {
        self.options.get(key).and_then(toml::Value::as_str)
    }

    /// The string-array setting `key` (`command`), if set.
    pub fn strings(&self, key: &str) -> Option<Vec<String>> {
        let items = self.options.get(key)?.as_array()?;
        items
            .iter()
            .map(|v| v.as_str().map(str::to_owned))
            .collect()
    }

    /// Whether `key` comes from the user config, where a host that receives
    /// the user's credentials may be chosen.
    pub fn set_by_user(&self, key: &str) -> bool {
        self.origin == Origin::User || self.user_keys.iter().any(|k| k == key)
    }
}

/// The cargo feature a type needs, `None` when it is always built or unknown.
pub fn feature_of(kind: &str) -> Option<&'static str> {
    TYPES.iter().find(|(t, ..)| *t == kind).and_then(|t| t.2)
}

/// A provider table of `rustman.toml` that is refused, by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    pub name: String,
    pub message: String,
}

/// Check the `[secret_providers]` tables of `rustman.toml`; returns a
/// warning per unknown key. Errors are configuration errors (exit code 2).
pub fn check_workspace(tables: &BTreeMap<String, toml::Table>) -> Result<Vec<String>, Refused> {
    let mut warnings = Vec::new();
    for (name, table) in tables {
        let refuse = |message: String| Refused {
            name: name.clone(),
            message,
        };
        if let Some(found) = credential_in(table) {
            return Err(refuse(format!(
                "credentials must not be stored in {SETTINGS_FILE} ({found})"
            )));
        }
        if table.get("type").and_then(toml::Value::as_str) == Some("cmd") {
            return Err(refuse(
                "cmd secret providers can only be defined in user config".to_owned(),
            ));
        }
        if let Some(key) = USER_ONLY_KEYS.iter().find(|k| table.contains_key(**k)) {
            return Err(refuse(format!(
                "secret provider '{name}': '{key}' can only be set in user config"
            )));
        }
        warnings.extend(check_table(name, table, true).map_err(refuse)?);
    }
    Ok(warnings)
}

/// The first credential of a workspace table, at any depth: a key that
/// names one (its dotted path), or a URL with a `user:password@` part
/// (`<key>: URL with credentials`). Never the value itself.
fn credential_in(table: &toml::Table) -> Option<String> {
    table.iter().find_map(|(key, value)| {
        if is_credential_key(key) {
            return Some(key.clone());
        }
        credential_in_value(value).map(|found| match found {
            Found::Key(inner) => format!("{key}.{inner}"),
            Found::Userinfo => format!("{key}: URL with credentials"),
        })
    })
}

enum Found {
    Key(String),
    Userinfo,
}

fn credential_in_value(value: &toml::Value) -> Option<Found> {
    match value {
        toml::Value::String(text) if has_userinfo(text) => Some(Found::Userinfo),
        toml::Value::Table(table) => credential_in(table).map(Found::Key),
        toml::Value::Array(items) => items.iter().find_map(credential_in_value),
        _ => None,
    }
}

/// Whether `text` is a URL whose authority has a `user[:password]@` part.
fn has_userinfo(text: &str) -> bool {
    let Some((_, rest)) = text.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    authority.contains('@')
}

/// Check one provider table: its type and the types of the shared keys.
/// `typed` is false for a user-config table that only overrides keys of a
/// workspace provider.
fn check_table(name: &str, table: &toml::Table, typed: bool) -> Result<Vec<String>, String> {
    let kind = match table.get("type") {
        Some(value) => value
            .as_str()
            .ok_or_else(|| format!("secret provider '{name}': 'type' must be a string"))?,
        None if typed => return Err(format!("secret provider '{name}': missing 'type'")),
        None => return Ok(Vec::new()),
    };
    let Some((_, keys, _)) = TYPES.iter().find(|(t, ..)| *t == kind) else {
        let known: Vec<&str> = TYPES.iter().map(|t| t.0).collect();
        return Err(format!(
            "secret provider '{name}': unknown type '{kind}' (expected one of: {})",
            known.join(", ")
        ));
    };
    // No setting is a table: a nested one (`[secret_providers.x.auth]`)
    // would otherwise be silently ignored.
    if let Some(key) = table.iter().find_map(|(k, v)| v.is_table().then_some(k)) {
        return Err(format!(
            "secret provider '{name}': '{key}' must not be a table"
        ));
    }
    for key in ["timeout_ms", "cache_ttl_s"] {
        if table
            .get(key)
            .is_some_and(|v| !matches!(v.as_integer(), Some(n) if n >= 0))
        {
            return Err(format!(
                "secret provider '{name}': '{key}' must be a non-negative integer"
            ));
        }
    }
    Ok(table
        .keys()
        .filter(|k| !COMMON_KEYS.contains(&k.as_str()) && !keys.contains(&k.as_str()))
        .map(|k| format!("{SETTINGS_FILE}: unknown key 'secret_providers.{name}.{k}' ignored"))
        .collect())
}

/// The user config: only `[secret_providers]` is read here.
#[derive(Debug, Default, Deserialize)]
struct UserConfig {
    #[serde(default)]
    secret_providers: BTreeMap<String, toml::Table>,
}

/// The `[secret_providers]` tables of `<dir>/config.toml`; a missing
/// directory or file defines none.
pub fn load_user(dir: Option<&Path>) -> Result<BTreeMap<String, toml::Table>, ConfigError> {
    let Some(path) = dir.map(|d| d.join(USER_CONFIG_FILE)) else {
        return Ok(BTreeMap::new());
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(ConfigError::io(&path, e)),
    };
    let invalid = |message: String| ConfigError::from(LoadError::new(&path, None, message));
    let config: UserConfig = toml::from_str(&text).map_err(|e| invalid(e.message().to_owned()))?;
    for (name, table) in &config.secret_providers {
        check_table(name, table, false).map_err(invalid)?;
    }
    Ok(config.secret_providers)
}

/// The providers of the workspace (already checked by settings loading)
/// overlaid with the user's: a user table with a `type` replaces the
/// workspace one, a user table without one overrides its keys (such as
/// `op_path`).
pub fn merge(
    workspace: &BTreeMap<String, toml::Table>,
    user: &BTreeMap<String, toml::Table>,
) -> Result<Vec<ProviderConfig>, String> {
    let mut configs = Vec::new();
    for (name, table) in workspace {
        let (table, origin) = match user.get(name) {
            Some(own) if own.contains_key("type") => (own.clone(), Origin::User),
            Some(own) => (overlay(table, own), Origin::Workspace),
            None => (table.clone(), Origin::Workspace),
        };
        let mut config = config(name, table, origin)?;
        if let Some(own) = user.get(name) {
            config.user_keys = own.keys().cloned().collect();
        }
        configs.push(config);
    }
    for (name, table) in user.iter().filter(|(n, _)| !workspace.contains_key(*n)) {
        if !table.contains_key("type") {
            return Err(format!("secret provider '{name}': missing 'type'"));
        }
        configs.push(config(name, table.clone(), Origin::User)?);
    }
    Ok(configs)
}

fn overlay(base: &toml::Table, over: &toml::Table) -> toml::Table {
    let mut merged = base.clone();
    merged.extend(over.iter().map(|(k, v)| (k.clone(), v.clone())));
    merged
}

fn config(name: &str, mut options: toml::Table, origin: Origin) -> Result<ProviderConfig, String> {
    check_table(name, &options, true)?;
    let integer = |options: &toml::Table, key: &str| {
        options
            .get(key)
            .and_then(toml::Value::as_integer)
            .and_then(|n| u64::try_from(n).ok())
    };
    let timeout = integer(&options, "timeout_ms").map_or(DEFAULT_TIMEOUT, Duration::from_millis);
    let ttl = integer(&options, "cache_ttl_s").map_or(DEFAULT_TTL, Duration::from_secs);
    let kind = options
        .remove("type")
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    Ok(ProviderConfig {
        name: name.to_owned(),
        kind,
        origin,
        timeout,
        ttl,
        options,
        user_keys: Vec::new(),
    })
}

/// The user config directory: `%APPDATA%\rustman` on Windows, else
/// `$XDG_CONFIG_HOME/rustman` or `$HOME/.config/rustman`, read through `var`.
pub fn user_config_dir_from(var: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let non_empty = |name: &str| var(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(windows) {
        non_empty("APPDATA")
    } else {
        non_empty("XDG_CONFIG_HOME").or_else(|| non_empty("HOME").map(|h| h.join(".config")))
    };
    base.map(|b| b.join("rustman"))
}

/// [`user_config_dir_from`] over this process's environment.
pub fn user_config_dir() -> Option<PathBuf> {
    user_config_dir_from(|name| std::env::var(name).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables(text: &str) -> BTreeMap<String, toml::Table> {
        #[derive(Deserialize)]
        struct File {
            secret_providers: BTreeMap<String, toml::Table>,
        }
        toml::from_str::<File>(text).unwrap().secret_providers
    }

    #[test]
    fn a_valid_vault_config_is_accepted() {
        let t = tables(
            "[secret_providers.vault]\ntype = \"vault\"\naddress = \"https://vault.example.com\"\nauth = \"oidc\"\n",
        );
        assert_eq!(check_workspace(&t).unwrap(), Vec::<String>::new());
        let configs = merge(&t, &BTreeMap::new()).unwrap();
        assert_eq!(configs[0].name, "vault");
        assert_eq!(configs[0].kind, "vault");
        assert_eq!(configs[0].string("auth"), Some("oidc"));
        assert_eq!(configs[0].origin, Origin::Workspace);
        assert_eq!(configs[0].timeout, DEFAULT_TIMEOUT);
        assert_eq!(configs[0].ttl, DEFAULT_TTL);
    }

    #[test]
    fn credentials_in_the_workspace_are_rejected() {
        for key in CREDENTIAL_KEYS {
            let t = tables(&format!(
                "[secret_providers.vault]\ntype = \"vault\"\n{key} = \"hvs.abc\"\n"
            ));
            let err = check_workspace(&t).unwrap_err().message;
            assert_eq!(
                err,
                format!("credentials must not be stored in rustman.toml ({key})")
            );
            assert!(!err.contains("hvs.abc"));
        }
    }

    #[test]
    fn keys_that_look_like_credentials_are_rejected_too() {
        for key in [
            "secret_access_key",
            "api_key",
            "access_key_id",
            "auth_token",
            "Token",
            "client-secret",
            "credentials",
        ] {
            let t = tables(&format!(
                "[secret_providers.x]\ntype = \"aws-ssm\"\n\"{key}\" = \"AKIA-canary\"\n"
            ));
            let err = check_workspace(&t).unwrap_err().message;
            assert_eq!(
                err,
                format!("credentials must not be stored in rustman.toml ({key})")
            );
        }
        // Every documented setting is still accepted.
        for (_, keys, _) in TYPES {
            for key in keys.iter().chain(COMMON_KEYS) {
                assert!(!is_credential_key(key), "{key}");
            }
        }
    }

    #[test]
    fn nested_credentials_and_url_userinfo_are_rejected() {
        let cases = [
            (
                "[secret_providers.v]\ntype = \"vault\"\n[secret_providers.v.auth]\ntoken = \"hvs.abc\"\n",
                "auth.token",
            ),
            (
                "[secret_providers.v]\ntype = \"vault\"\nmount = [{ password = \"hvs.abc\" }]\n",
                "mount.password",
            ),
            (
                "[secret_providers.v]\ntype = \"vault\"\naddress = \"https://me:hvs.abc@vault.example\"\n",
                "address: URL with credentials",
            ),
            (
                "[secret_providers.o]\ntype = \"onepassword\"\nconnect_host = \"http://hvs.abc@op.example/v1\"\n",
                "connect_host: URL with credentials",
            ),
        ];
        for (text, found) in cases {
            let err = check_workspace(&tables(text)).unwrap_err().message;
            assert_eq!(
                err,
                format!("credentials must not be stored in rustman.toml ({found})")
            );
            assert!(!err.contains("hvs.abc"), "{err}");
        }
        // An `@` outside the authority is not userinfo.
        let t = tables(
            "[secret_providers.v]\ntype = \"vault\"\naddress = \"https://vault.example/a@b\"\n",
        );
        assert!(check_workspace(&t).is_ok());
    }

    #[test]
    fn a_nested_table_is_an_error_not_ignored() {
        let text = "[secret_providers.v]\ntype = \"vault\"\n[secret_providers.v.auth]\nmethod = \"approle\"\n";
        let t = tables(text);
        assert_eq!(
            check_workspace(&t).unwrap_err().message,
            "secret provider 'v': 'auth' must not be a table"
        );
        assert_eq!(
            merge(&BTreeMap::new(), &t).unwrap_err(),
            "secret provider 'v': 'auth' must not be a table"
        );
    }

    #[test]
    fn commands_and_binary_paths_are_user_config_only() {
        let t = tables("[secret_providers.x]\ntype = \"cmd\"\ncommand = [\"pass\", \"show\"]\n");
        assert_eq!(
            check_workspace(&t).unwrap_err().message,
            "cmd secret providers can only be defined in user config"
        );
        let t = tables("[secret_providers.op]\ntype = \"onepassword\"\nop_path = \"/tmp/evil\"\n");
        assert_eq!(
            check_workspace(&t).unwrap_err().message,
            "secret provider 'op': 'op_path' can only be set in user config"
        );
        // A workspace cannot send cloud credentials to a host of its own.
        let t = tables(
            "[secret_providers.az]\ntype = \"azure-key-vault\"\nendpoint = \"https://evil.example\"\n",
        );
        assert_eq!(
            check_workspace(&t).unwrap_err().message,
            "secret provider 'az': 'endpoint' can only be set in user config"
        );
    }

    #[test]
    fn types_and_numbers_are_checked() {
        let t = tables("[secret_providers.x]\ntype = \"lastpass\"\n");
        assert!(
            check_workspace(&t)
                .unwrap_err()
                .message
                .contains("unknown type 'lastpass'")
        );
        let t = tables("[secret_providers.x]\naddress = \"a\"\n");
        assert_eq!(
            check_workspace(&t).unwrap_err().message,
            "secret provider 'x': missing 'type'"
        );
        let t = tables("[secret_providers.x]\ntype = \"env\"\ntimeout_ms = -1\n");
        assert!(
            check_workspace(&t)
                .unwrap_err()
                .message
                .contains("'timeout_ms'")
        );
        let t = tables("[secret_providers.x]\ntype = \"env\"\nregion = \"eu\"\n");
        assert_eq!(
            check_workspace(&t).unwrap(),
            ["rustman.toml: unknown key 'secret_providers.x.region' ignored"]
        );
    }

    #[test]
    fn user_config_overlays_the_workspace() {
        let ws = tables(
            "[secret_providers.op]\ntype = \"onepassword\"\n[secret_providers.ci]\ntype = \"env\"\ntimeout_ms = 50\ncache_ttl_s = 0\n",
        );
        let user = tables(
            "[secret_providers.op]\nop_path = \"/opt/op\"\n[secret_providers.pass]\ntype = \"cmd\"\ncommand = [\"pass\", \"show\"]\n",
        );
        let configs = merge(&ws, &user).unwrap();
        let by = |n: &str| configs.iter().find(|c| c.name == n).unwrap();
        assert_eq!(by("op").string("op_path"), Some("/opt/op"));
        assert_eq!(by("op").origin, Origin::Workspace);
        assert!(by("op").set_by_user("op_path"));
        assert!(!by("op").set_by_user("connect_host"));
        assert!(!by("ci").set_by_user("timeout_ms"));
        assert!(by("pass").set_by_user("command"));
        assert_eq!(by("ci").timeout, Duration::from_millis(50));
        assert_eq!(by("ci").ttl, Duration::ZERO);
        assert_eq!(by("pass").origin, Origin::User);
        assert_eq!(
            by("pass").strings("command"),
            Some(vec!["pass".to_owned(), "show".to_owned()])
        );
        let orphan = tables("[secret_providers.y]\nop_path = \"/x\"\n");
        assert!(merge(&BTreeMap::new(), &orphan).is_err());
    }

    #[test]
    fn the_user_config_file_is_read_from_the_config_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_user(Some(dir.path())).unwrap().is_empty());
        assert!(load_user(None).unwrap().is_empty());
        std::fs::write(
            dir.path().join(USER_CONFIG_FILE),
            "theme = \"x\"\n[secret_providers.pass]\ntype = \"cmd\"\ncommand = [\"pass\"]\n",
        )
        .unwrap();
        let user = load_user(Some(dir.path())).unwrap();
        assert!(user.contains_key("pass"));
        std::fs::write(
            dir.path().join(USER_CONFIG_FILE),
            "[secret_providers.p]\ntype = 3\n",
        )
        .unwrap();
        assert!(load_user(Some(dir.path())).is_err());
    }

    #[test]
    fn the_config_dir_follows_the_platform() {
        let vars = |pairs: &'static [(&str, &str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(user_config_dir_from(vars(&[])), None);
        if cfg!(windows) {
            let dir = user_config_dir_from(vars(&[("APPDATA", r"C:\Users\a\AppData\Roaming")]));
            assert_eq!(
                dir,
                Some(PathBuf::from(r"C:\Users\a\AppData\Roaming\rustman"))
            );
        } else {
            let home = user_config_dir_from(vars(&[("HOME", "/home/a")]));
            assert_eq!(home, Some(PathBuf::from("/home/a/.config/rustman")));
            let xdg = user_config_dir_from(vars(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "/cfg")]));
            assert_eq!(xdg, Some(PathBuf::from("/cfg/rustman")));
        }
        assert_eq!(feature_of("aws-ssm"), Some("secrets-aws"));
        assert_eq!(feature_of("vault"), None);
    }
}
