//! Loading the workspace's plugins for a headless command (plugins spec,
//! "Headless plugin trust"): an untrusted plugin set stops the command
//! with exit code 2 before anything is sent, unless `--trust-plugins`
//! records the approval. A trusted set is loaded with exactly its grants.

use std::path::Path;
use std::sync::Arc;

use clap::Args;
use rustman_core::plugins::Extensions;
#[cfg(feature = "plugins")]
use rustman_core::safety::Permission;
use rustman_core::{ConfigError, Environment, SecretStore};

/// The request kinds a workspace's files may have: the built-in ones, and
/// (as opaque requests, checked by [`check_kinds`] once the plugins load)
/// any other kind when the workspace declares plugins. Reads
/// `rustman.toml` only; no plugin code runs.
pub fn kind_registry(root: &Path) -> rustman_core::model::KindRegistry {
    let registry = rustman_core::model::KindRegistry::default();
    #[cfg(feature = "plugins")]
    if rustman_plugin::declare::declarations(root).is_ok_and(|d| !d.is_empty()) {
        return registry.with_opaque_fallback();
    }
    let _ = root;
    registry
}

/// The workspace found from `start` (or at `explicit`), loaded with
/// [`kind_registry`] so request files of plugin kinds do not stop commands
/// that never run them (`env`, `secret`, `report render`, ...).
pub fn discover(
    start: &Path,
    explicit: Option<&Path>,
) -> Result<rustman_core::Workspace, ConfigError> {
    let root = rustman_core::discovery::find_root(start, explicit)?;
    rustman_core::Workspace::load_with(&root, &kind_registry(&root))
}

/// Refuse the request files of `ws` whose kind no loaded plugin registers,
/// and those whose fields do not match their plugin kind's schema (exit
/// code 2, before any request is sent).
pub fn check_kinds(
    ws: &rustman_core::Workspace,
    plugins: Option<&Arc<dyn Extensions>>,
) -> Result<(), ConfigError> {
    rustman_core::plugins::check_kinds(ws, plugins.map(|p| &**p as &dyn Extensions))
}

/// `--trust-plugins`, shared by the commands that run requests.
#[derive(Debug, Args)]
pub struct PluginFlags {
    /// Load the workspace plugins even when this machine has not trusted
    /// them yet, and record the approval (for CI)
    // Hidden without plugin support, where it does nothing.
    #[arg(long)]
    #[cfg_attr(not(feature = "plugins"), arg(hide = true))]
    pub trust_plugins: bool,
}

/// The plugins of the workspace at `root`, or `None` when it declares
/// none (or this build has no plugin support).
#[cfg(feature = "plugins")]
pub fn headless(
    root: &Path,
    environment: Option<&Environment>,
    store: &SecretStore,
    flags: &PluginFlags,
) -> Result<Option<Arc<dyn Extensions>>, ConfigError> {
    use rustman_core::trust::TrustKey;

    rustman_plugin::verify_pins(root).map_err(usage)?;
    let key = TrustKey::machine();
    if !rustman_plugin::trust::require(root, key, flags.trust_plugins).map_err(usage)? {
        return Ok(None);
    }
    load(root, environment, store, Permission::Mutations, None).map(Some)
}

#[cfg(not(feature = "plugins"))]
pub fn headless(
    _root: &Path,
    _environment: Option<&Environment>,
    _store: &SecretStore,
    _flags: &PluginFlags,
) -> Result<Option<Arc<dyn Extensions>>, ConfigError> {
    Ok(None)
}

/// The plugins of an agent session (`rustman mcp serve`): loaded only when
/// their set is already trusted. Agents can never trust a set, so an
/// untrusted one is left unloaded and the server starts without it.
/// Their network requests pass the agent's allowlist on top of their
/// grants, and are audited.
#[cfg(feature = "plugins")]
pub fn agent(
    workspace: &rustman_core::Workspace,
    environment: Option<&Environment>,
    store: &SecretStore,
) -> Result<Option<Arc<dyn Extensions>>, ConfigError> {
    use rustman_core::trust::TrustKey;
    use rustman_mcp::server::plugin_net::AgentNetGuard;
    use rustman_plugin::trust::{Trust, check};

    let root = &workspace.root;

    rustman_plugin::verify_pins(root).map_err(usage)?;
    match check(root, TrustKey::machine()).map_err(usage)? {
        Trust::NoPlugins => Ok(None),
        Trust::Untrusted(_) => {
            eprintln!("rustman mcp: plugins not loaded: workspace not trusted");
            Ok(None)
        }
        // Agent mutations need the human's confirmation, which a plugin
        // call cannot ask for: plugins of agent sessions only read.
        Trust::Trusted => {
            let guard = AgentNetGuard::shared(workspace, environment);
            load(root, environment, store, Permission::ReadOnly, Some(guard)).map(Some)
        }
    }
}

#[cfg(not(feature = "plugins"))]
pub fn agent(
    _workspace: &rustman_core::Workspace,
    _environment: Option<&Environment>,
    _store: &SecretStore,
) -> Result<Option<Arc<dyn Extensions>>, ConfigError> {
    Ok(None)
}

/// Load and compile the trusted plugin set of `root`, downloading a
/// missing cached URL source (verified against its pin). What is loaded
/// must be exactly the set trusted on this machine.
#[cfg(feature = "plugins")]
fn load(
    root: &Path,
    environment: Option<&Environment>,
    store: &SecretStore,
    permission: Permission,
    guard: Option<Arc<dyn rustman_core::plugins::PluginNetGuard>>,
) -> Result<Arc<dyn Extensions>, ConfigError> {
    use rustman_core::trust::TrustKey;
    use rustman_plugin::LoadOptions;
    use rustman_plugin::session::{Session, open, session_secrets};

    let session = Session {
        root,
        environment,
        secrets: session_secrets(environment, store),
        permission,
        load: LoadOptions {
            allow_fetch: true,
            fetch: rustman_plugin::manage::fetch_options(root).map_err(usage)?,
        },
        key: TrustKey::machine(),
        guard,
    };
    let opened = super::exec::runtime()
        .map_err(|e| ConfigError::Usage(e.to_string()))?
        .block_on(open(session))
        .map_err(usage)?;
    let skipped: Vec<String> = opened.skipped.iter().map(ToString::to_string).collect();
    super::print_warnings(&skipped);
    Ok(Arc::new(opened.runtime))
}

#[cfg(feature = "plugins")]
fn usage(error: rustman_plugin::LoadError) -> ConfigError {
    ConfigError::Usage(error.to_string())
}

/// A `--reporter` value: a built-in format, or `<plugin>/<format>:<path>`
/// rendered by a report plugin (plugins spec, "Report renderer extension
/// point").
#[derive(Clone, Debug)]
pub enum ReporterChoice {
    Builtin(rustman_report::Reporter),
    Plugin(PluginReport),
}

/// A report a plugin renders into a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginReport {
    /// `<plugin>/<format>`.
    pub format: String,
    pub path: std::path::PathBuf,
}

impl std::str::FromStr for ReporterChoice {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, String> {
        let name = value.split_once(':').map_or(value, |(name, _)| name);
        if rustman_core::plugins::split_qualified(name).is_none() {
            return value
                .parse()
                .map(Self::Builtin)
                .map_err(|e: rustman_report::ReporterError| e.to_string());
        }
        match value.split_once(':') {
            Some((format, path)) if !path.is_empty() => Ok(Self::Plugin(PluginReport {
                format: format.to_owned(),
                path: path.into(),
            })),
            _ => Err(format!(
                "plugin reporter '{value}' needs a file: {name}:<path>"
            )),
        }
    }
}

/// The built-in reporters and the plugin reports of `choices`.
pub fn split_reporters(
    choices: &[ReporterChoice],
) -> (Vec<rustman_report::Reporter>, Vec<PluginReport>) {
    let mut builtin = Vec::new();
    let mut plugin = Vec::new();
    for choice in choices {
        match choice {
            ReporterChoice::Builtin(reporter) => builtin.push(reporter.clone()),
            ReporterChoice::Plugin(report) => plugin.push(report.clone()),
        }
    }
    (builtin, plugin)
}

/// Write each plugin report of the redacted report document `report`: the
/// file holds exactly the bytes the plugin returned.
pub fn write_plugin_reports(
    plugins: Option<&Arc<dyn Extensions>>,
    reports: &[PluginReport],
    report: &str,
) -> Result<(), ConfigError> {
    for target in reports {
        let Some(plugins) = plugins else {
            return Err(ConfigError::Usage(format!(
                "report format {}: workspace plugins are not loaded",
                target.format
            )));
        };
        let bytes = plugins
            .render(&target.format, report)
            .map_err(ConfigError::Usage)?;
        write_file(&target.path, &bytes)?;
    }
    Ok(())
}

/// Write a plugin report like the built-in reporters: a regular file is
/// replaced atomically, so an interrupted write never leaves half of it.
fn write_file(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    rustman_report::write_file_bytes(path, bytes).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// What `rustman import <format> <src>` converts with a plugin, and where
/// it goes (paths relative to the workspace root, with `/`).
pub struct ImportTarget<'a> {
    pub format: &'a str,
    pub source: &'a str,
    pub into: &'a str,
    pub dry_run: bool,
    pub on_conflict: rustman_core::plugin_import::OnConflict,
}

/// Convert `target.source` with the importer plugin of `target.format`,
/// check every file it returned, then write them (plugins spec, "Importer
/// extension point"). Nothing is written when one is invalid.
pub fn import(
    root: &Path,
    plugins: Option<&Arc<dyn Extensions>>,
    target: &ImportTarget<'_>,
) -> Result<(), ConfigError> {
    use rustman_core::plugin_import::{Written, plan, write};

    let Some(plugins) = plugins else {
        return Err(ConfigError::Usage(format!(
            "unknown import format '{}' (no workspace plugin registers it)",
            target.format
        )));
    };
    let files = plugins
        .import(target.format, target.source)
        .map_err(ConfigError::Usage)?;
    let planned = plan(target.into, &files, &**plugins).map_err(ConfigError::Usage)?;
    for file in &planned {
        for reference in &file.neutralized {
            println!(
                "review: {}: secret reference {reference} from the source neutralized: \
                 it is kept as literal text and never resolved",
                file.path
            );
        }
    }
    if target.dry_run {
        for file in &planned {
            println!("would write {}", file.path);
        }
        return Ok(());
    }
    for written in write(root, &planned, target.on_conflict)? {
        match written {
            Written::Created(path) => println!("created {path}"),
            Written::Replaced(path) => println!("replaced {path}"),
            Written::Renamed { from, to } => println!("created {to} ({from} exists)"),
            Written::Skipped(path) => println!("skipped {path} (exists)"),
        }
    }
    println!(
        "imported {} request(s) and {} environment(s) with plugin {}",
        files.requests.len(),
        files.environments.len(),
        files.plugin
    );
    Ok(())
}
