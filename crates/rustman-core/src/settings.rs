//! `rustman.toml`: the workspace marker and its settings (design D5).

use std::path::Path;

use serde::Deserialize;

use crate::error::{ConfigError, LoadError, Position};
use crate::model::Variables;

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
    ("http", &["ca_bundle", "proxy", "max_body_bytes"]),
];

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Settings {
    pub workspace: WorkspaceSection,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub http: HttpDefaults,
    #[serde(default)]
    pub variables: Variables,
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

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct HttpDefaults {
    pub ca_bundle: Option<String>,
    /// Where `ca_bundle` was written in `rustman.toml`, for error messages.
    #[serde(skip)]
    pub ca_bundle_position: Option<Position>,
    pub proxy: Option<String>,
    pub max_body_bytes: u64,
}

impl Default for HttpDefaults {
    fn default() -> Self {
        Self {
            ca_bundle: None,
            ca_bundle_position: None,
            proxy: None,
            max_body_bytes: 10 * 1024 * 1024,
        }
    }
}

impl Settings {
    /// Parse the contents of `rustman.toml`. Returns the settings and a warning
    /// for every unknown key.
    pub fn parse(file: &Path, text: &str) -> Result<(Self, Vec<String>), ConfigError> {
        let table: toml::Table = toml::from_str(text).map_err(|e| toml_error(file, text, &e))?;
        check_version(file, &table)?;
        let mut settings: Self = toml::from_str(text).map_err(|e| toml_error(file, text, &e))?;
        settings.http.ca_bundle_position = ca_bundle_position(text);
        Ok((settings, unknown_keys(&table)))
    }

    /// Read and parse `rustman.toml` from the workspace root.
    pub fn load(root: &Path) -> Result<(Self, Vec<String>), ConfigError> {
        let file = root.join(SETTINGS_FILE);
        let text = std::fs::read_to_string(&file).map_err(|e| ConfigError::io(&file, e))?;
        Self::parse(Path::new(SETTINGS_FILE), &text)
    }
}

/// The position of `[http] ca_bundle`, read in a second, span-keeping pass.
fn ca_bundle_position(text: &str) -> Option<Position> {
    #[derive(Deserialize)]
    struct Spans {
        http: Option<HttpSpans>,
    }
    #[derive(Deserialize)]
    struct HttpSpans {
        ca_bundle: Option<toml::Spanned<String>>,
    }
    let spans: Spans = toml::from_str(text).ok()?;
    let span = spans.http?.ca_bundle?.span();
    Some(position_at(text, span.start))
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
        assert!(settings.variables.is_empty());
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
        assert!(s.variables.get("base_url").is_some());
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
