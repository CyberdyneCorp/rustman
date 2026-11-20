//! Workspace plugin trust (plugins spec, "Workspace plugin trust",
//! "Headless plugin trust"; design D5).
//!
//! A workspace's plugin set is identified by a hash over the sorted
//! `(name, sha256, grants, limits)` of every declared plugin, plus the
//! hash of a path source's `rustman-plugin.toml` when the manifest sits
//! next to the component (an embedded manifest is covered by the pin), so
//! adding a plugin or changing any pin, grant, limit or manifest asks
//! again. The approval lives in the
//! workspace [`TrustStore`] (`.rustman/trust.yaml`) under [`SUBJECT`],
//! sealed with this machine's key like the stdio-server approvals of the
//! MCP client: an approval that came with the repository matches nothing.
//! Nothing here runs plugin code or reaches the network.

use std::path::Path;
use std::sync::Arc;

use rustman_core::plugins::{Extensions, PluginGate, PluginKind, PluginSummary, PluginTrustPrompt};
use rustman_core::trust::{TrustKey, TrustState, TrustStore};
use sha2::{Digest, Sha256};

use crate::declare::{Declaration, Source, declarations};
use crate::error::LoadError;
use crate::grant::Grant;
use crate::manifest::Manifest;
use crate::runtime::PluginRuntime;
use crate::sandbox::HostContext;
use crate::session::{Session, open, session_secrets};
use crate::source::{package_from_download, read_cache, read_path};
use crate::workspace::LoadOptions;

/// The trust subject of a workspace's plugin set.
pub const SUBJECT: &str = "plugins";

/// The error of a headless command that meets an untrusted plugin set.
pub const UNTRUSTED: &str =
    "workspace plugins are not trusted (run interactively or pass --trust-plugins)";

/// The hash of a plugin set: SHA-256 over the sorted `(name, sha256,
/// sorted grants, limits)` of each plugin, as lowercase hex. Raising a
/// plugin's execution time or memory limit changes it, and so does editing
/// the manifest file of a path source (see [`manifest_digest`]).
pub fn fingerprint(root: &Path, declarations: &[Declaration]) -> String {
    fingerprint_with(declarations, |d| manifest_digest(root, d))
}

/// [`fingerprint`], with the manifest digest of each declaration from
/// `digest`: [`crate::load`] passes the digests of the bytes it read.
pub fn fingerprint_with(
    declarations: &[Declaration],
    digest: impl Fn(&Declaration) -> String,
) -> String {
    let mut entries: Vec<(&str, &str, Vec<&str>, String)> = declarations
        .iter()
        .map(|d| {
            let mut grants: Vec<&str> = d.grants.iter().map(|g| g.trim()).collect();
            grants.sort_unstable();
            let limits = format!(
                "cpu_ms={} memory_mib={} manifest={}",
                d.limits.cpu_ms,
                d.limits.memory_mib,
                digest(d)
            );
            (d.name.as_str(), d.sha256.as_str(), grants, limits)
        })
        .collect();
    entries.sort_unstable();
    let mut hash = Sha256::new();
    for (name, sha256, grants, limits) in entries {
        hash.update(name.as_bytes());
        hash.update(b"\0");
        hash.update(sha256.as_bytes());
        for grant in grants {
            hash.update(b"\0");
            hash.update(grant.as_bytes());
        }
        hash.update(b"\0");
        hash.update(limits.as_bytes());
        hash.update(b"\n");
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The SHA-256 of the `rustman-plugin.toml` file a path source reads its
/// manifest from (in its folder, or beside a `.wasm` file that embeds
/// none); empty for a URL source, an embedded manifest, or a source that
/// cannot be read. The sha256 pin covers only the `.wasm` file, so this
/// makes a manifest edit ask for trust again.
fn manifest_digest(root: &Path, declaration: &Declaration) -> String {
    let Source::Path(path) = &declaration.source else {
        return String::new();
    };
    read_path(root, &declaration.name, path)
        .map(|package| package.manifest_digest())
        .unwrap_or_default()
}

/// A grant in plain language, for the trust prompt.
pub fn describe(grant: &Grant) -> String {
    match grant {
        Grant::Clock => "read the clock".to_owned(),
        Grant::Random => "generate random bytes".to_owned(),
        Grant::Env(name) => format!("read the environment variable {name}"),
        Grant::FsRead(dir) if dir.is_empty() => {
            "read files anywhere in the workspace (except .rustman/)".to_owned()
        }
        Grant::FsRead(dir) => format!("read files under {dir}/"),
        Grant::Net(target) if crate::sandbox::net::is_local_host(&target.host) => {
            format!("send HTTP requests to {target} (this machine or a private network)")
        }
        Grant::Net(target) => format!("send HTTP requests to {target}"),
        Grant::SecretsRead(name) => format!("read the secret {name}"),
    }
}

/// A grant as written, in plain language when it parses.
fn describe_text(text: &str) -> String {
    match Grant::parse(text) {
        Ok(grant) => describe(&grant),
        Err(_) => format!("{} (invalid grant)", text.trim()),
    }
}

/// Whether the plugin set declared at `root` is trusted on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trust {
    /// The workspace declares no plugin.
    NoPlugins,
    Trusted,
    /// What to show the user before anything loads.
    Untrusted(PluginTrustPrompt),
}

/// Check the plugin set declared at `root` against the trust store.
pub fn check(root: &Path, key: TrustKey) -> Result<Trust, LoadError> {
    let declared = declarations(root)?;
    if declared.is_empty() {
        return Ok(Trust::NoPlugins);
    }
    let store = TrustStore::load_with_key(root, key).map_err(workspace)?;
    let fingerprint = fingerprint(root, &declared);
    let changed = match store.state(SUBJECT, &fingerprint) {
        TrustState::Trusted => return Ok(Trust::Trusted),
        TrustState::Changed => true,
        TrustState::Unknown => false,
    };
    let plugins = declared.iter().map(|d| summary(root, d)).collect();
    Ok(Trust::Untrusted(PluginTrustPrompt {
        plugins,
        changed,
        fingerprint,
    }))
}

/// Record the user's trust in the set with `fingerprint`, which must still
/// be the set declared at `root`.
pub fn approve(root: &Path, key: TrustKey, fingerprint_shown: &str) -> Result<(), LoadError> {
    let current = fingerprint(root, &declarations(root)?);
    if current != fingerprint_shown {
        return Err(LoadError::Workspace(
            "the workspace plugins changed while the trust prompt was shown; review them again"
                .to_owned(),
        ));
    }
    let mut store = TrustStore::load_with_key(root, key).map_err(workspace)?;
    store.approve(SUBJECT, &current).map_err(workspace)?;
    store.save().map_err(workspace)
}

/// Refuse a loaded `set` whose fingerprint (computed from what the load
/// read) is not the approved one: the plugins changed between the trust
/// check and the load.
pub fn verify_loaded(root: &Path, key: TrustKey, set: &crate::PluginSet) -> Result<(), LoadError> {
    let store = TrustStore::load_with_key(root, key).map_err(workspace)?;
    match store.state(SUBJECT, &set.fingerprint) {
        TrustState::Trusted => Ok(()),
        _ => Err(LoadError::Workspace(CHANGED_WHILE_LOADING.to_owned())),
    }
}

/// The error of a load whose plugins changed after the trust check.
pub const CHANGED_WHILE_LOADING: &str =
    "the workspace plugins changed while they were loading; review them again";

/// Load the plugin set of `root`, refusing it unless what was read is
/// exactly the set trusted on this machine.
pub async fn load_trusted(
    root: &Path,
    key: TrustKey,
    options: &LoadOptions,
) -> Result<crate::PluginSet, LoadError> {
    let set = crate::load(root, options).await?;
    verify_loaded(root, key, &set)?;
    Ok(set)
}

/// The headless rule: an untrusted set is refused unless `trust_plugins`
/// (`--trust-plugins`) is set, which records the approval. Returns whether
/// the workspace declares any plugin.
pub fn require(root: &Path, key: TrustKey, trust_plugins: bool) -> Result<bool, LoadError> {
    match check(root, key.clone())? {
        Trust::NoPlugins => Ok(false),
        Trust::Trusted => Ok(true),
        Trust::Untrusted(prompt) if trust_plugins => {
            approve(root, key, &prompt.fingerprint)?;
            Ok(true)
        }
        Trust::Untrusted(_) => Err(LoadError::Workspace(UNTRUSTED.to_owned())),
    }
}

/// How the prompt shows one plugin: its version comes from a manifest read
/// from disk (never downloaded), else `unknown`.
fn summary(root: &Path, declaration: &Declaration) -> PluginSummary {
    let version = local_manifest(root, declaration)
        .map_or_else(|| "unknown".to_owned(), |m| m.version.to_string());
    PluginSummary {
        name: declaration.name.clone(),
        version,
        source: declaration.source.as_str().to_owned(),
        grants: declaration
            .grants
            .iter()
            .map(|g| describe_text(g))
            .collect(),
        limits: format!(
            "execution time {} ms per call, memory {} MiB",
            declaration.limits.cpu_ms, declaration.limits.memory_mib
        ),
    }
}

pub(crate) fn local_manifest(root: &Path, declaration: &Declaration) -> Option<Manifest> {
    let name = &declaration.name;
    let package = match &declaration.source {
        Source::Path(path) => read_path(root, name, path).ok()?,
        Source::Url(_) => {
            let wasm = read_cache(root, name, &declaration.sha256).ok()??;
            package_from_download(name, wasm).ok()?
        }
    };
    Manifest::parse(name, &package.manifest).ok()
}

/// The [`PluginGate`] of an interactive front-end: [`check`] and
/// [`approve`] with this machine's key.
#[derive(Clone, Debug, Default)]
pub struct TrustGate {
    pub key: TrustKey,
}

impl PluginGate for TrustGate {
    fn check(&self, root: &Path) -> Result<Option<PluginTrustPrompt>, String> {
        match check(root, self.key.clone()).map_err(|e| e.to_string())? {
            Trust::NoPlugins | Trust::Trusted => Ok(None),
            Trust::Untrusted(prompt) => Ok(Some(prompt)),
        }
    }

    fn approve(&self, root: &Path, prompt: &PluginTrustPrompt) -> Result<(), String> {
        approve(root, self.key.clone(), &prompt.fingerprint).map_err(|e| e.to_string())
    }

    /// Load the trusted set from disk (no download) and list its kinds.
    fn kinds(&self, root: &Path) -> Result<Vec<PluginKind>, String> {
        if check(root, self.key.clone()).map_err(|e| e.to_string())? != Trust::Trusted {
            return Ok(Vec::new());
        }
        let root = root.to_path_buf();
        let key = self.key.clone();
        // Its own thread and runtime: the caller may run inside one.
        let loaded = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .map_err(|e| e.to_string())?;
            let options = LoadOptions {
                fetch: crate::manage::fetch_options(&root).map_err(|e| e.to_string())?,
                allow_fetch: false,
            };
            let set = runtime
                .block_on(load_trusted(&root, key, &options))
                .map_err(|e| e.to_string())?;
            let plugins = PluginRuntime::new(&set, HostContext::new(&root));
            plugins.map(|p| p.kinds()).map_err(|e| e.to_string())
        });
        loaded
            .join()
            .map_err(|_| "loading the workspace plugins failed".to_owned())?
    }

    /// Open the trusted set for one send, with the environment's secrets
    /// and the network of its grants, like a headless command. Nothing is
    /// downloaded: a URL source must be cached already.
    fn extensions(
        &self,
        root: &Path,
        environment: Option<&rustman_core::Environment>,
        store: &rustman_core::SecretStore,
    ) -> Result<Option<Arc<dyn Extensions>>, String> {
        if check(root, self.key.clone()).map_err(|e| e.to_string())? != Trust::Trusted {
            return Ok(None);
        }
        let session = Session {
            root,
            environment,
            secrets: session_secrets(environment, store),
            permission: rustman_core::safety::Permission::Mutations,
            load: LoadOptions {
                fetch: crate::manage::fetch_options(root).map_err(|e| e.to_string())?,
                allow_fetch: false,
            },
            key: self.key.clone(),
        };
        // Its own thread and runtime: the caller may run inside one.
        let opened = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .build()
                        .map_err(|e| e.to_string())?;
                    runtime.block_on(open(session)).map_err(|e| e.to_string())
                })
                .join()
                .map_err(|_| "loading the workspace plugins failed".to_owned())?
        })?;
        Ok(Some(Arc::new(opened.runtime)))
    }
}

fn workspace(error: impl std::fmt::Display) -> LoadError {
    LoadError::Workspace(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::Limits;

    fn declaration(name: &str, sha256: &str, grants: &[&str]) -> Declaration {
        Declaration {
            name: name.to_owned(),
            source: Source::Path(format!("plugins/{name}.wasm")),
            sha256: sha256.to_owned(),
            grants: grants.iter().map(|g| (*g).to_owned()).collect(),
            limits: Limits::default(),
        }
    }

    #[test]
    fn the_fingerprint_ignores_order_but_not_pins_or_grants() {
        let a = declaration("a", "11", &["clock", "net:x.example"]);
        let b = declaration("b", "22", &[]);
        let base = fingerprint(Path::new("."), &[a.clone(), b.clone()]);
        let reordered = declaration("a", "11", &["net:x.example", "clock"]);
        assert_eq!(base, fingerprint(Path::new("."), &[b.clone(), reordered]));
        let regranted = declaration("a", "11", &["clock", "net:y.example"]);
        assert_ne!(base, fingerprint(Path::new("."), &[regranted, b.clone()]));
        let repinned = declaration("a", "12", &["clock", "net:x.example"]);
        assert_ne!(base, fingerprint(Path::new("."), &[repinned, b.clone()]));
        assert_ne!(base, fingerprint(Path::new("."), &[a]));
        assert_eq!(base.len(), 64);
    }

    #[test]
    fn grants_read_in_plain_language() {
        let cases = [
            ("clock", "read the clock"),
            ("random", "generate random bytes"),
            ("env:HOME", "read the environment variable HOME"),
            ("fs:read:data", "read files under data/"),
            (
                "net:api.example.com",
                "send HTTP requests to api.example.com",
            ),
            (
                "net:api.example.com:8443",
                "send HTTP requests to api.example.com:8443",
            ),
            ("secrets:read:hmac_key", "read the secret hmac_key"),
            ("net:*", "net:* (invalid grant)"),
            (
                "net:169.254.169.254",
                "send HTTP requests to 169.254.169.254 (this machine or a private network)",
            ),
            (
                "net:127.0.0.1:8080",
                "send HTTP requests to 127.0.0.1:8080 (this machine or a private network)",
            ),
            (
                "net:localhost",
                "send HTTP requests to localhost (this machine or a private network)",
            ),
            (
                "net:10.1.2.3",
                "send HTTP requests to 10.1.2.3 (this machine or a private network)",
            ),
            ("net:93.184.216.34", "send HTTP requests to 93.184.216.34"),
        ];
        for (grant, text) in cases {
            assert_eq!(describe_text(grant), text, "{grant}");
        }
    }
}
