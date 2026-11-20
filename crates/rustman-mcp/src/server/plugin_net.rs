//! The network of plugins in agent sessions (plugins spec, "Plugin network
//! access"): a plugin of `rustman mcp serve` sends under its `net:` grants,
//! and on top of them under the agent's own allowlist (`[agent]
//! allowed_hosts`, or the hosts of the server's environment). Every
//! request it sends, or has refused, gets an audit line with its method
//! and redacted URL, and a request that cannot be audited is not sent.

use std::sync::Arc;

use rustman_core::plugins::PluginNetGuard;
use rustman_core::run::now_rfc3339;
use rustman_core::{Environment, Workspace};
use serde_json::json;

use super::audit::{AuditEntry, AuditLog, Outcome};
use super::policy::session_sender;

/// The session name of plugin requests in the audit log.
pub const PLUGIN_SESSION: &str = "plugins";

/// [`PluginNetGuard`] of agent sessions.
pub struct AgentNetGuard {
    /// The agent's sender policy, or why every request is refused.
    policy: Result<rustman_core::safety::SafetyPolicy, String>,
    audit: AuditLog,
}

impl std::fmt::Debug for AgentNetGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentNetGuard")
            .field("audit", &self.audit)
            .finish_non_exhaustive()
    }
}

impl AgentNetGuard {
    /// The guard of the plugins of a server over `workspace` started in
    /// `environment`: the allowlist agents of that environment get.
    pub fn new(workspace: &Workspace, environment: Option<&Environment>) -> Self {
        let settings = &workspace.settings;
        let policy = session_sender(&settings.safety, &settings.agent, environment)
            .map(|sender| sender.policy().clone());
        Self {
            policy,
            audit: AuditLog::for_workspace(&workspace.root),
        }
    }

    /// The guard as the plugin host takes it.
    pub fn shared(
        workspace: &Workspace,
        environment: Option<&Environment>,
    ) -> Arc<dyn PluginNetGuard> {
        Arc::new(Self::new(workspace, environment))
    }

    fn check(&self, url: &str) -> Result<(), String> {
        let policy = self.policy.as_ref().map_err(Clone::clone)?;
        policy
            .check_origin(url)
            .map_err(|blocked| blocked.reason().to_owned())
    }
}

impl PluginNetGuard for AgentNetGuard {
    fn admit(&self, plugin: &str, method: &str, url: &str, shown: &str) -> Result<(), String> {
        self.audit.open().map_err(|e| e.message)?;
        let decision = self.check(url);
        let entry = AuditEntry {
            timestamp: now_rfc3339(),
            session: PLUGIN_SESSION.to_owned(),
            client: "plugin".to_owned(),
            tool: format!("plugin:{plugin}"),
            arguments: json!({}),
            outcome: if decision.is_ok() {
                Outcome::Ok
            } else {
                Outcome::Blocked
            },
            reason: decision.as_ref().err().cloned(),
            method: Some(method.to_owned()),
            url: Some(shown.to_owned()),
        };
        self.audit.append(&entry).map_err(|e| e.message)?;
        decision
    }
}
