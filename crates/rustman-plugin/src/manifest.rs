//! `rustman-plugin.toml`: what a plugin is and what it asks for (plugins
//! spec, "Plugin package and manifest" and "Plugin API versioning").
//!
//! The manifest ships next to the `.wasm` file, or inside it as a custom
//! section named [`MANIFEST_SECTION`] (required for URL sources, whose
//! single download must carry it; embedded, the manifest is covered by the
//! sha256 pin).

use serde::Deserialize;

use crate::error::LoadError;
use crate::extension::ExtensionPoint;
use crate::grant::Capability;

/// The manifest file name.
pub const MANIFEST_FILE: &str = "rustman-plugin.toml";

/// The custom section of a component that holds its manifest.
pub const MANIFEST_SECTION: &str = "rustman-plugin.toml";

/// The WIT package of the plugin API.
pub const PACKAGE: &str = rustman_plugin_sdk::PACKAGE;

/// The newest plugin API version this build implements.
pub const API_VERSION: &str = rustman_plugin_sdk::API_VERSION;

/// The plugin API majors this build supports, oldest first.
pub const SUPPORTED_API: [u64; 1] = [rustman_plugin_sdk::API_MAJOR as u64];

/// A validated manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    /// Kebab-case.
    pub name: String,
    pub version: semver::Version,
    /// The plugin API major the component was built against.
    pub api: u64,
    pub extension_points: Vec<ExtensionPoint>,
    /// What the plugin may be granted.
    pub capabilities: Vec<Capability>,
    pub description: Option<String>,
    /// For a directory source: the component, relative to the manifest.
    pub component: Option<String>,
}

#[derive(Deserialize)]
struct Raw {
    name: Option<String>,
    version: Option<String>,
    api: Option<i64>,
    extension_points: Option<Vec<String>>,
    capabilities: Option<Vec<String>>,
    description: Option<String>,
    component: Option<String>,
}

impl Manifest {
    /// Parse and validate `text`. `context` names the plugin in messages
    /// until the manifest's own name is known (the workspace's name for it).
    pub fn parse(context: &str, text: &str) -> Result<Self, LoadError> {
        let raw: Raw = toml::from_str(text).map_err(|e| {
            let message = format!("{MANIFEST_FILE}: {}", e.message().trim_end());
            LoadError::invalid(context, message)
        })?;
        let name = required(context, raw.name, "name")?;
        if !is_kebab_case(&name) {
            let message = format!("{MANIFEST_FILE}: name '{name}' is not kebab-case");
            return Err(LoadError::invalid(context, message));
        }
        let version = required(&name, raw.version, "version")?;
        let version = semver::Version::parse(&version).map_err(|e| {
            let message = format!("{MANIFEST_FILE}: version '{version}' is not semver: {e}");
            LoadError::invalid(&name, message)
        })?;
        let api = required(&name, raw.api, "api")?;
        let api = u64::try_from(api)
            .ok()
            .filter(|api| *api > 0)
            .ok_or_else(|| LoadError::invalid(&name, format!("{MANIFEST_FILE}: invalid api")))?;
        let points = required(&name, raw.extension_points, "extension_points")?;
        let capabilities = required(&name, raw.capabilities, "capabilities")?;
        Ok(Self {
            extension_points: extension_points(&name, &points)?,
            capabilities: capabilities_of(&name, &capabilities)?,
            name,
            version,
            api,
            description: raw.description,
            component: raw.component,
        })
    }

    /// Refuse a manifest for an API major this build does not support.
    pub fn check_api(&self) -> Result<(), LoadError> {
        if SUPPORTED_API.contains(&self.api) {
            return Ok(());
        }
        Err(LoadError::UnsupportedApi {
            plugin: self.name.clone(),
            api: self.api,
            supported: supported_api(),
        })
    }
}

/// The supported majors, as `rustman plugin api` lists them: `1`, `1, 2`.
pub fn supported_api() -> String {
    let majors: Vec<String> = SUPPORTED_API.iter().map(u64::to_string).collect();
    majors.join(", ")
}

fn required<T>(plugin: &str, value: Option<T>, field: &str) -> Result<T, LoadError> {
    value.ok_or_else(|| {
        LoadError::invalid(plugin, format!("{MANIFEST_FILE}: missing field '{field}'"))
    })
}

fn extension_points(plugin: &str, names: &[String]) -> Result<Vec<ExtensionPoint>, LoadError> {
    if names.is_empty() {
        let message = format!("{MANIFEST_FILE}: extension_points is empty");
        return Err(LoadError::invalid(plugin, message));
    }
    let mut points = Vec::new();
    for name in names {
        let point = ExtensionPoint::parse(name).ok_or_else(|| {
            LoadError::invalid(plugin, format!("unknown extension point '{name}'"))
        })?;
        if !points.contains(&point) {
            points.push(point);
        }
    }
    Ok(points)
}

fn capabilities_of(plugin: &str, texts: &[String]) -> Result<Vec<Capability>, LoadError> {
    texts
        .iter()
        .map(|text| {
            Capability::parse(text).map_err(|e| {
                let error = e.for_plugin(plugin, text);
                let LoadError::Invalid { message, .. } = error else {
                    return error;
                };
                LoadError::invalid(plugin, format!("{MANIFEST_FILE}: capability {message}"))
            })
        })
        .collect()
}

/// Lowercase ASCII words of letters and digits, joined by single `-`.
pub fn is_kebab_case(name: &str) -> bool {
    let word = |w: &str| {
        !w.is_empty()
            && w.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    };
    name.starts_with(|c: char| c.is_ascii_lowercase()) && name.split('-').all(word)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grant::Grant;

    const VALID: &str = r#"
name = "aws-sigv4"
version = "0.3.1"
api = 1
extension_points = ["auth"]
capabilities = ["clock"]
"#;

    fn error(text: &str) -> String {
        Manifest::parse("ctx", text).unwrap_err().to_string()
    }

    #[test]
    fn valid_manifest() {
        let manifest = Manifest::parse("ctx", VALID).unwrap();
        assert_eq!(manifest.name, "aws-sigv4");
        assert_eq!(manifest.version.to_string(), "0.3.1");
        assert_eq!(manifest.extension_points, [ExtensionPoint::Auth]);
        assert_eq!(manifest.capabilities, [Capability::Class("clock")]);
        assert!(manifest.check_api().is_ok());
        assert!(Grant::Clock.requested_by(&manifest.capabilities));
        assert!(!Grant::Random.requested_by(&manifest.capabilities));
    }

    #[test]
    fn unknown_extension_point() {
        let text = VALID.replace(r#"["auth"]"#, r#"["telemetry"]"#);
        assert_eq!(
            error(&text),
            "plugin aws-sigv4: unknown extension point 'telemetry'"
        );
    }

    #[test]
    fn newer_api_major() {
        let manifest = Manifest::parse("ctx", &VALID.replace("api = 1", "api = 2")).unwrap();
        let err = manifest.check_api().unwrap_err();
        assert_eq!(
            err.to_string(),
            "plugin aws-sigv4 targets plugin API 2; this rustman supports 1"
        );
        assert!(err.is_skippable());
    }

    #[test]
    fn missing_fields_are_named() {
        for field in ["version", "api", "extension_points", "capabilities"] {
            let text: String = VALID
                .lines()
                .filter(|line| !line.starts_with(field))
                .map(|line| format!("{line}\n"))
                .collect();
            assert_eq!(
                error(&text),
                format!("plugin aws-sigv4: rustman-plugin.toml: missing field '{field}'")
            );
        }
        let nameless = VALID.replace("name = \"aws-sigv4\"", "");
        assert_eq!(
            error(&nameless),
            "plugin ctx: rustman-plugin.toml: missing field 'name'"
        );
    }

    #[test]
    fn invalid_values_are_refused() {
        assert!(error(&VALID.replace("aws-sigv4", "AWS_SigV4")).contains("not kebab-case"));
        assert!(error(&VALID.replace("0.3.1", "1.0")).contains("is not semver"));
        assert!(error(&VALID.replace("api = 1", "api = 0")).contains("invalid api"));
        assert!(error(&VALID.replace(r#"["auth"]"#, "[]")).contains("extension_points is empty"));
        assert_eq!(
            error(&VALID.replace(r#"["clock"]"#, r#"["net:*"]"#)),
            "plugin aws-sigv4: rustman-plugin.toml: capability wildcard grants are not allowed"
        );
        assert!(error("name = 1").starts_with("plugin ctx: rustman-plugin.toml: "));
    }

    #[test]
    fn kebab_case() {
        assert!(is_kebab_case("aws-sigv4"));
        assert!(is_kebab_case("faker"));
        for bad in ["", "-a", "a-", "a--b", "A", "1a", "a_b"] {
            assert!(!is_kebab_case(bad), "{bad}");
        }
    }
}
