//! A workspace's plugin set: every declared plugin, read from its source,
//! checked against its pin, its manifest and its grants. Nothing here runs
//! plugin code; [`crate::plugin::Plugin`] does, once the set is trusted.

use std::path::Path;
use std::sync::Arc;

use crate::declare::{Declaration, Source, declarations};
use crate::error::LoadError;
use crate::fetch::{FetchOptions, fetch};
use crate::grant::{Grant, parse_grants};
use crate::lock::LockedPlugin;
use crate::manifest::Manifest;
use crate::pin::verify;
use crate::source::{
    Package, package_from_download, read_cache, read_path, read_path_wasm, write_cache,
};

/// A plugin whose bytes match the pin and whose manifest and grants are
/// valid.
#[derive(Clone, Debug)]
pub struct VerifiedPlugin {
    pub declaration: Declaration,
    pub manifest: Manifest,
    pub grants: Vec<Grant>,
    pub wasm: Arc<[u8]>,
}

impl VerifiedPlugin {
    /// The workspace's name for the plugin.
    pub fn name(&self) -> &str {
        &self.declaration.name
    }

    /// The plugin's `rustman.lock` entry.
    pub fn locked(&self) -> LockedPlugin {
        LockedPlugin {
            name: self.declaration.name.clone(),
            version: self.manifest.version.to_string(),
            source: self.declaration.source.as_str().to_owned(),
            sha256: self.declaration.sha256.clone(),
            api: self.manifest.api,
        }
    }
}

/// The verified plugins, and those skipped because this build does not
/// support their API major (shown, but the workspace still runs).
#[derive(Clone, Debug, Default)]
pub struct PluginSet {
    pub plugins: Vec<VerifiedPlugin>,
    pub skipped: Vec<LoadError>,
    /// The trust fingerprint of what [`load`] actually read: the
    /// declarations and the bytes of each manifest file. A trusted load
    /// compares it with the approved one ([`crate::trust::verify_loaded`]),
    /// so a change between the trust check and the load is refused.
    pub fingerprint: String,
}

impl PluginSet {
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty() && self.skipped.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&VerifiedPlugin> {
        self.plugins.iter().find(|p| p.name() == name)
    }
}

/// How URL sources are handled when their download is not cached.
#[derive(Clone, Debug, Default)]
pub struct LoadOptions {
    pub fetch: FetchOptions,
    /// Download a missing cache entry (and verify it against the pin).
    /// Off: a missing cache entry is an error.
    pub allow_fetch: bool,
}

/// Load the plugin set of the workspace at `root`. Only plugins declared
/// in `rustman.toml` are read. Any error but an unsupported API major stops
/// the load.
pub async fn load(root: &Path, options: &LoadOptions) -> Result<PluginSet, LoadError> {
    let mut set = PluginSet::default();
    let declared = declarations(root)?;
    // Plugin names are unique: they are the `[plugins.<name>]` keys.
    let mut digests = std::collections::HashMap::new();
    for declaration in declared.iter().cloned() {
        let package = package(root, &declaration, options).await?;
        digests.insert(declaration.name.clone(), package.manifest_digest());
        match check(declaration, package) {
            Ok(plugin) => set.plugins.push(plugin),
            Err(e) if e.is_skippable() => set.skipped.push(e),
            Err(e) => return Err(e),
        }
    }
    set.fingerprint = crate::trust::fingerprint_with(&declared, |d| {
        digests.get(&d.name).cloned().unwrap_or_default()
    });
    Ok(set)
}

/// Re-hash every declared plugin that is on disk (a path source, or a
/// cached download) against its pin, without parsing or running anything.
/// A URL source that is not cached yet is verified when it is downloaded.
pub fn verify_pins(root: &Path) -> Result<(), LoadError> {
    for declaration in declarations(root)? {
        let name = &declaration.name;
        let wasm = match &declaration.source {
            Source::Path(path) => Some(read_path_wasm(root, name, path)?),
            Source::Url(_) => read_cache(root, name, &declaration.sha256)?,
        };
        if let Some(wasm) = wasm {
            verify(name, &declaration.sha256, &wasm)?;
        }
    }
    Ok(())
}

async fn package(
    root: &Path,
    declaration: &Declaration,
    options: &LoadOptions,
) -> Result<Package, LoadError> {
    let (name, pin) = (&declaration.name, &declaration.sha256);
    let url = match &declaration.source {
        Source::Path(path) => {
            let package = read_path(root, name, path)?;
            verify(name, pin, &package.wasm)?;
            return Ok(package);
        }
        Source::Url(url) => url,
    };
    if let Some(wasm) = read_cache(root, name, pin)? {
        verify(name, pin, &wasm)?;
        return package_from_download(name, wasm);
    }
    if !options.allow_fetch {
        let message = format!("{url} is not downloaded yet (run `rustman plugin verify`)");
        return Err(LoadError::invalid(name, message));
    }
    let wasm = fetch(name, url, &options.fetch).await?;
    verify(name, pin, &wasm)?;
    let package = package_from_download(name, wasm)?;
    write_cache(root, name, pin, &package.wasm)?;
    Ok(package)
}

/// Check a package against its declaration.
pub fn check(declaration: Declaration, package: Package) -> Result<VerifiedPlugin, LoadError> {
    let manifest = Manifest::parse(&declaration.name, &package.manifest)?;
    manifest.check_api()?;
    let grants = parse_grants(
        &declaration.name,
        &declaration.grants,
        &manifest.capabilities,
    )?;
    Ok(VerifiedPlugin {
        declaration,
        manifest,
        grants,
        wasm: package.wasm.into(),
    })
}
