//! `[plugins.<name>]` in `rustman.toml` (plugins spec, "Declaring plugins
//! in the workspace"). Only declared plugins are ever loaded.

use std::collections::BTreeMap;
use std::path::Path;

use rustman_core::settings::SETTINGS_FILE;
use serde::Deserialize;

use crate::error::LoadError;
use crate::grant::{GrantError, normalize_relative};
use crate::limits::{Limits, LimitsTable};
use crate::manifest::is_kebab_case;

/// Where a plugin comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A `.wasm` file, or a folder holding `rustman-plugin.toml`, relative
    /// to the workspace root (normalized, `/`-separated).
    Path(String),
    /// An `https` URL; the download is cached by its sha256.
    Url(String),
}

impl Source {
    /// Parse a `source` value: `https` URLs, or workspace-relative paths.
    pub fn parse(plugin: &str, text: &str) -> Result<Self, LoadError> {
        let lower = text.to_ascii_lowercase();
        if lower.starts_with("https://") {
            url::Url::parse(text)
                .map_err(|e| LoadError::invalid(plugin, format!("source '{text}': {e}")))?;
            return Ok(Self::Url(text.to_owned()));
        }
        if lower.starts_with("http://") {
            return Err(LoadError::PlainHttp);
        }
        if text.contains("://") {
            return Err(LoadError::PlainHttp);
        }
        match normalize_relative(text) {
            Ok(parts) if !parts.is_empty() => Ok(Self::Path(parts.join("/"))),
            Ok(_) => Err(LoadError::invalid(plugin, "source is empty")),
            Err(GrantError::Escapes) => {
                let message = format!("source '{text}' escapes the workspace");
                Err(LoadError::invalid(plugin, message))
            }
            Err(_) => Err(LoadError::invalid(
                plugin,
                format!("invalid source '{text}'"),
            )),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Path(path) | Self::Url(path) => path,
        }
    }
}

/// One `[plugins.<name>]` table, validated except for its grants, which
/// need the manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub name: String,
    pub source: Source,
    /// Lowercase hex.
    pub sha256: String,
    /// As written; parsed against the manifest when the plugin loads.
    pub grants: Vec<String>,
    pub limits: Limits,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDeclaration {
    source: String,
    sha256: String,
    #[serde(default)]
    grants: Vec<String>,
    #[serde(default)]
    limits: LimitsTable,
}

#[derive(Deserialize)]
struct RawSettings {
    #[serde(default)]
    plugins: BTreeMap<String, toml::Value>,
}

/// The plugins `rustman.toml` (its text) declares, sorted by name.
pub fn parse_declarations(text: &str) -> Result<Vec<Declaration>, LoadError> {
    let raw: RawSettings = toml::from_str(text).map_err(|e| {
        LoadError::Workspace(format!("{SETTINGS_FILE}: {}", e.message().trim_end()))
    })?;
    raw.plugins
        .into_iter()
        .map(|(name, value)| declaration(name, value))
        .collect()
}

/// The plugins the workspace at `root` declares.
pub fn declarations(root: &Path) -> Result<Vec<Declaration>, LoadError> {
    let file = root.join(SETTINGS_FILE);
    let text = std::fs::read_to_string(&file)
        .map_err(|e| LoadError::Workspace(format!("{}: {e}", file.display())))?;
    parse_declarations(&text)
}

fn declaration(name: String, value: toml::Value) -> Result<Declaration, LoadError> {
    if !is_kebab_case(&name) {
        let message = format!("[plugins.{name}]: the name is not kebab-case");
        return Err(LoadError::invalid(&name, message));
    }
    let raw: RawDeclaration = value.try_into().map_err(|e: toml::de::Error| {
        let message = format!("[plugins.{name}]: {}", e.message().trim_end());
        LoadError::invalid(&name, message)
    })?;
    let sha256 = raw.sha256.trim().to_ascii_lowercase();
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        let message = format!("[plugins.{name}]: sha256 must be 64 hex digits");
        return Err(LoadError::invalid(&name, message));
    }
    Ok(Declaration {
        source: Source::parse(&name, raw.source.trim())?,
        limits: raw.limits.validate(&name)?,
        sha256,
        grants: raw.grants,
        name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIN: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";

    fn parse(tables: &str) -> Result<Vec<Declaration>, String> {
        let text = format!("[workspace]\nname = \"w\"\nversion = 1\n{tables}");
        parse_declarations(&text).map_err(|e| e.to_string())
    }

    #[test]
    fn declarations_are_read_with_defaults() {
        let tables = format!(
            "[plugins.hmac]\nsource = \"plugins/hmac\"\nsha256 = \"{}\"\n\n[plugins.faker]\nsource = \"https://example.com/faker.wasm\"\nsha256 = \"{PIN}\"\ngrants = [\"random\"]\nlimits = {{ cpu_ms = 250 }}\n",
            PIN.to_uppercase()
        );
        let declared = parse(&tables).unwrap();
        assert_eq!(declared.len(), 2);
        let (faker, hmac) = (&declared[0], &declared[1]);
        assert_eq!(hmac.name, "hmac");
        assert_eq!(hmac.source, Source::Path("plugins/hmac".into()));
        assert_eq!(hmac.sha256, PIN);
        assert!(hmac.grants.is_empty());
        assert_eq!(hmac.limits, Limits::default());
        assert_eq!(
            faker.source,
            Source::Url("https://example.com/faker.wasm".into())
        );
        assert_eq!(faker.grants, ["random"]);
        assert_eq!(faker.limits.cpu_ms, 250);
        assert!(parse("").unwrap().is_empty());
    }

    #[test]
    fn invalid_declarations_are_refused() {
        let table = |body: &str| format!("[plugins.hmac]\nsha256 = \"{PIN}\"\n{body}\n");
        assert_eq!(
            parse(&table("source = \"http://example.com/p.wasm\"")).unwrap_err(),
            "plugin sources must use https"
        );
        assert_eq!(
            parse(&table("source = \"../elsewhere/p.wasm\"")).unwrap_err(),
            "plugin hmac: source '../elsewhere/p.wasm' escapes the workspace"
        );
        assert!(
            parse(&table("source = \"p.wasm\"\nhash = 1"))
                .unwrap_err()
                .contains("unknown field `hash`")
        );
        assert_eq!(
            parse(&table(
                "source = \"p.wasm\"\nlimits = { memory_mib = 4096 }"
            ))
            .unwrap_err(),
            "plugin hmac: memory limit exceeds maximum (1024 MiB)"
        );
        let short = "[plugins.hmac]\nsource = \"p.wasm\"\nsha256 = \"ab12\"\n";
        assert_eq!(
            parse(short).unwrap_err(),
            "plugin hmac: [plugins.hmac]: sha256 must be 64 hex digits"
        );
        let upper = format!("[plugins.Hmac]\nsource = \"p.wasm\"\nsha256 = \"{PIN}\"\n");
        assert!(parse(&upper).unwrap_err().contains("not kebab-case"));
    }
}
