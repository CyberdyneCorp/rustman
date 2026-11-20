//! A trusted plugin set opened for a session (plugins spec, "Headless
//! plugin trust", "Plugin network access"): loaded and checked against the
//! approved fingerprint, given the session's secrets, and a network
//! limited to its `net:` grants through the workspace safety policy. The
//! CLI commands and the TUI send path open plugins this way.

use std::path::Path;
use std::sync::Arc;

use rustman_core::safety::Permission;
use rustman_core::trust::TrustKey;
use rustman_core::{Environment, Secret, SecretStore};

use crate::error::LoadError;
use crate::runtime::PluginRuntime;
use crate::sandbox::net::GovernedNetwork;
use crate::sandbox::{HostContext, SystemServices};
use crate::workspace::{LoadOptions, PluginSet};

/// What a session opens its plugins with.
pub struct Session<'a> {
    pub root: &'a Path,
    pub environment: Option<&'a Environment>,
    /// The session's secrets ([`session_secrets`]).
    pub secrets: Vec<Secret<String>>,
    /// The methods plugin requests may use.
    pub permission: Permission,
    pub load: LoadOptions,
    /// The machine key the trust approval is sealed with.
    pub key: TrustKey,
}

/// A session's compiled plugins, and those skipped because this build
/// does not support their API major.
pub struct Opened {
    pub runtime: PluginRuntime,
    pub skipped: Vec<LoadError>,
}

/// Load the trusted plugin set of `session.root` and compile it for the
/// session. Fails when what was read is not the trusted set.
pub async fn open(session: Session<'_>) -> Result<Opened, LoadError> {
    let set = crate::trust::load_trusted(session.root, session.key, &session.load).await?;
    let context = HostContext::new(session.root).with_secrets(session.secrets);
    let context = with_network(
        session.root,
        session.environment,
        &set,
        context,
        session.permission,
    )?;
    let runtime = PluginRuntime::new(&set, context)?;
    Ok(Opened {
        runtime,
        skipped: set.skipped,
    })
}

/// `context` with the network of the set's `net:` grants: plugin requests
/// go through the safety policy of the workspace, limited to the methods
/// `permission` allows, and are recorded in its history.
fn with_network(
    root: &Path,
    environment: Option<&Environment>,
    set: &PluginSet,
    context: HostContext,
    permission: Permission,
) -> Result<HostContext, LoadError> {
    let (settings, _) = rustman_core::settings::Settings::load(root)
        .map_err(|e| LoadError::Workspace(e.to_string()))?;
    let history = settings
        .history
        .enabled
        .then(|| rustman_core::history::HistoryStore::for_workspace(root, &settings.history));
    let grants = set.plugins.iter().flat_map(|plugin| &plugin.grants);
    let network = GovernedNetwork::for_grants(
        &settings.safety,
        permission,
        environment,
        grants,
        history,
        context.redactor(),
    )
    .map_err(LoadError::Workspace)?;
    let services = SystemServices { network };
    Ok(context.with_services(Arc::new(services)))
}

/// The secrets of the active environment that are set: what plugins may
/// reveal with a grant, and what everything they return is redacted of.
pub fn session_secrets(
    environment: Option<&Environment>,
    store: &SecretStore,
) -> Vec<Secret<String>> {
    let Some(environment) = environment else {
        return Vec::new();
    };
    environment
        .secrets
        .iter()
        .filter_map(|name| store.get(&environment.name, name).ok().flatten())
        .collect()
}
