//! Managing a workspace's plugins (plugins spec, "Plugin CLI"): adding a
//! plugin from a path or an `https` URL with no grants, removing one,
//! listing them with their trust state, and re-verifying every pin. None
//! of these runs plugin code: adding one compiles it to check that it is a
//! component of the plugin API, and nothing more.

use std::path::Path;
use std::sync::Arc;

use rustman_core::settings::Settings;
use rustman_core::trust::TrustKey;

use crate::declare::{Declaration, Source, declarations};
use crate::edit::{add_table, remove_table};
use crate::error::LoadError;
use crate::fetch::{FetchOptions, fetch};
use crate::limits::Limits;
use crate::lock::Lockfile;
use crate::manifest::Manifest;
use crate::pin::sha256_hex;
use crate::plugin::Plugin;
use crate::sandbox::HostContext;
use crate::source::{Package, package_from_download, read_path, write_cache};
use crate::trust::{Trust, check as check_trust, local_manifest};
use crate::workspace::{LoadOptions, PluginSet, check, load, verify_pins};

/// A plugin `plugin add` declared.
#[derive(Clone, Debug)]
pub struct Added {
    pub declaration: Declaration,
    pub manifest: Manifest,
}

/// How downloads of the workspace at `root` connect: TLS verification is
/// always on, and the workspace's `[http] ca_bundle` is trusted too.
pub fn fetch_options(root: &Path) -> Result<FetchOptions, LoadError> {
    let (settings, _) = Settings::load(root).map_err(|e| LoadError::Workspace(e.to_string()))?;
    let ca_bundle = match settings.http.ca_bundle.as_deref() {
        Some(path) => {
            let file = root.join(path);
            let bytes = std::fs::read(&file)
                .map_err(|e| LoadError::Workspace(format!("{}: {e}", file.display())))?;
            Some(bytes)
        }
        None => None,
    };
    Ok(FetchOptions {
        ca_bundle,
        ..FetchOptions::default()
    })
}

/// The `source` of a plugin at `path` (relative to `cwd`) for the
/// workspace at `root`: a URL as given, a path relative to the workspace
/// with `/` separators. A path outside the workspace is refused.
pub fn source_text(root: &Path, cwd: &Path, path: &str) -> Result<String, LoadError> {
    if path.contains("://") {
        return Ok(path.to_owned());
    }
    let invalid =
        |message: String| LoadError::Workspace(format!("plugin source {path}: {message}"));
    let full = dunce::canonicalize(cwd.join(path)).map_err(|e| invalid(e.to_string()))?;
    let root = dunce::canonicalize(root).map_err(|e| invalid(e.to_string()))?;
    let relative = full
        .strip_prefix(&root)
        .map_err(|_| invalid(format!("outside the workspace {}", root.display())))?;
    let parts: Vec<String> = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.is_empty() {
        return Err(invalid("is the workspace root".to_owned()));
    }
    Ok(parts.join("/"))
}

/// Declare the plugin at `source` (a workspace-relative path or an `https`
/// URL) as `name` (default: its manifest's name), with no grants: fetch
/// it, pin its sha256, check its manifest and component, then write
/// `rustman.toml` and `rustman.lock`. A URL download is cached.
pub async fn add(
    root: &Path,
    source: &str,
    name: Option<&str>,
    options: &FetchOptions,
) -> Result<Added, LoadError> {
    let context = name.unwrap_or(source);
    let source = Source::parse(context, source.trim())?;
    refuse_declared(root, name, &source)?;
    let package = match &source {
        Source::Path(path) => read_path(root, context, path)?,
        Source::Url(url) => package_from_download(context, fetch(context, url, options).await?)?,
    };
    let manifest = Manifest::parse(context, &package.manifest)?;
    let declaration = Declaration {
        name: name.unwrap_or(&manifest.name).to_owned(),
        sha256: sha256_hex(&package.wasm),
        source,
        grants: Vec::new(),
        limits: Limits::default(),
    };
    let verified = verify_component(root, declaration.clone(), package)?;
    let settings = root.join(rustman_core::settings::SETTINGS_FILE);
    let text = read_text(&settings)?;
    let text = add_table(&text, &declaration)?;
    if matches!(declaration.source, Source::Url(_)) {
        write_cache(root, &declaration.name, &declaration.sha256, &verified.wasm)?;
    }
    write_text(&settings, &text)?;
    let mut lock = Lockfile::read(root)?;
    lock.upsert(verified.locked());
    lock.write(root)?;
    Ok(Added {
        declaration,
        manifest: verified.manifest,
    })
}

/// Refuse a plugin whose name or source the workspace already declares,
/// before anything is downloaded.
fn refuse_declared(root: &Path, name: Option<&str>, source: &Source) -> Result<(), LoadError> {
    let declared = declarations(root)?;
    let same = declared
        .iter()
        .find(|d| name == Some(d.name.as_str()) || &d.source == source);
    match same {
        Some(d) => Err(LoadError::invalid(
            &d.name,
            format!(
                "already declared in rustman.toml (run `rustman plugin remove {}` first)",
                d.name
            ),
        )),
        None => Ok(()),
    }
}

/// Check the manifest and grants, and compile the component: it must
/// import only the host interfaces and export its extension points.
fn verify_component(
    root: &Path,
    declaration: Declaration,
    package: Package,
) -> Result<crate::VerifiedPlugin, LoadError> {
    if !crate::manifest::is_kebab_case(&declaration.name) {
        let message = format!("[plugins.{}]: the name is not kebab-case", declaration.name);
        return Err(LoadError::invalid(&declaration.name, message));
    }
    let verified = check(declaration, package)?;
    Plugin::new(&verified, Arc::new(HostContext::new(root)))?;
    Ok(verified)
}

/// Remove the plugin `name` from `rustman.toml` and `rustman.lock`. The
/// cached download, if any, stays in `.rustman/plugins/`.
pub fn remove(root: &Path, name: &str) -> Result<(), LoadError> {
    let settings = root.join(rustman_core::settings::SETTINGS_FILE);
    let text = remove_table(&read_text(&settings)?, name)?;
    write_text(&settings, &text)?;
    let mut lock = Lockfile::read(root)?;
    if lock.remove(name) {
        lock.write(root)?;
    }
    Ok(())
}

/// Whether this machine trusts the workspace's plugin set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustState {
    Trusted,
    Untrusted,
    /// Trusted before, with other plugins, pins or grants.
    Changed,
}

impl TrustState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::Untrusted => "not trusted",
            Self::Changed => "changed since trusted",
        }
    }
}

/// One declared plugin, as `plugin list` shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub name: String,
    /// From the manifest on disk, else from `rustman.lock`; `unknown`
    /// when neither has it.
    pub version: String,
    pub api: String,
    pub source: String,
    pub grants: Vec<String>,
}

/// The declared plugins and the trust state of their set; `None` when the
/// workspace declares none. Reads files only.
pub fn list(root: &Path, key: TrustKey) -> Result<Option<(Vec<Listed>, TrustState)>, LoadError> {
    let trust = match check_trust(root, key)? {
        Trust::NoPlugins => return Ok(None),
        Trust::Trusted => TrustState::Trusted,
        Trust::Untrusted(prompt) if prompt.changed => TrustState::Changed,
        Trust::Untrusted(_) => TrustState::Untrusted,
    };
    let lock = Lockfile::read(root)?;
    let listed = declarations(root)?
        .iter()
        .map(|declaration| listed(root, declaration, &lock))
        .collect();
    Ok(Some((listed, trust)))
}

fn listed(root: &Path, declaration: &Declaration, lock: &Lockfile) -> Listed {
    let locked = lock
        .get(&declaration.name)
        .filter(|l| l.sha256 == declaration.sha256);
    let (version, api) = match (local_manifest(root, declaration), locked) {
        (Some(manifest), _) => (manifest.version.to_string(), manifest.api.to_string()),
        (None, Some(locked)) => (locked.version.clone(), locked.api.to_string()),
        (None, None) => ("unknown".to_owned(), "unknown".to_owned()),
    };
    Listed {
        name: declaration.name.clone(),
        version,
        api,
        source: declaration.source.as_str().to_owned(),
        grants: declaration.grants.clone(),
    }
}

/// Re-hash every declared plugin against its pin, downloading the URL
/// sources that are not cached yet, check their manifests and grants, and
/// bring `rustman.lock` in line. Returns the set and whether the lockfile
/// changed.
pub async fn verify(root: &Path, fetch: FetchOptions) -> Result<(PluginSet, bool), LoadError> {
    let options = LoadOptions {
        fetch,
        allow_fetch: true,
    };
    verify_pins(root)?;
    let set = load(root, &options).await?;
    let declared: Vec<String> = declarations(root)?.into_iter().map(|d| d.name).collect();
    let before = Lockfile::read(root)?;
    let mut lock = before.clone();
    lock.plugins.retain(|p| declared.contains(&p.name));
    for plugin in &set.plugins {
        lock.upsert(plugin.locked());
    }
    let changed = lock != before;
    if changed {
        lock.write(root)?;
    }
    Ok((set, changed))
}

fn read_text(path: &Path) -> Result<String, LoadError> {
    std::fs::read_to_string(path)
        .map_err(|e| LoadError::Workspace(format!("{}: {e}", path.display())))
}

fn write_text(path: &Path, text: &str) -> Result<(), LoadError> {
    std::fs::write(path, text).map_err(|e| LoadError::Workspace(format!("{}: {e}", path.display())))
}
