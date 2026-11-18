//! The `[agent]` table of `rustman.toml`: what AI agents may do through the
//! MCP server (agent-mcp-interface, design D4). Permissions come only from
//! this table; no command-line flag raises them.

use std::num::NonZeroU32;

use serde::Deserialize;

use crate::safety::{Origin, Permission};

/// Default cap on one serialized tool result, in bytes.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 256 * 1024;

/// Default number of background jobs that may run at once.
pub const DEFAULT_MAX_JOBS: NonZeroU32 = NonZeroU32::new(2).unwrap();

/// `[agent]`: the permissions and limits of agent sessions.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentSettings {
    /// Allow POST, PUT and PATCH.
    pub allow_mutations: bool,
    /// Allow DELETE and unknown methods; implies `allow_mutations`.
    pub allow_destructive: bool,
    /// Without a client that can ask the human (MCP elicitation), refuse
    /// mutating calls. `false` accepts unattended mutations.
    pub require_confirmation: bool,
    /// The origins agents may reach. Empty: the hosts of the session's
    /// active environment.
    pub allowed_hosts: Vec<Origin>,
    /// Browser origins accepted by the HTTP transport besides loopback ones.
    pub allowed_origins: Vec<Origin>,
    /// Cap on one serialized tool result.
    pub max_output_bytes: usize,
    /// Background jobs that may run at once.
    pub max_jobs: NonZeroU32,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            allow_mutations: false,
            allow_destructive: false,
            require_confirmation: true,
            allowed_hosts: Vec::new(),
            allowed_origins: Vec::new(),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            max_jobs: DEFAULT_MAX_JOBS,
        }
    }
}

impl AgentSettings {
    /// The methods agents may send.
    pub fn permission(&self) -> Permission {
        Permission::from_flags(self.allow_mutations, self.allow_destructive)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::error::ConfigError;
    use crate::settings::Settings;

    const HEAD: &str = "[workspace]\nname = \"shop\"\nversion = 1\n";

    fn parse(agent: &str) -> Result<AgentSettings, ConfigError> {
        let text = format!("{HEAD}{agent}");
        let (settings, warnings) = Settings::parse(Path::new("rustman.toml"), &text)?;
        assert!(warnings.is_empty(), "{warnings:?}");
        Ok(settings.agent)
    }

    #[test]
    fn a_missing_table_is_read_only_with_confirmation() {
        let agent = parse("").unwrap();
        assert_eq!(agent, AgentSettings::default());
        assert!(!agent.allow_mutations);
        assert!(!agent.allow_destructive);
        assert!(agent.require_confirmation);
        assert!(agent.allowed_hosts.is_empty());
        assert!(agent.allowed_origins.is_empty());
        assert_eq!(agent.max_output_bytes, 262_144);
        assert_eq!(agent.max_jobs.get(), 2);
        assert_eq!(agent.permission(), Permission::ReadOnly);
        assert_eq!(parse("[agent]\n").unwrap(), AgentSettings::default());
    }

    #[test]
    fn every_key_is_read() {
        let agent = parse(
            r#"
[agent]
allow_mutations = true
allow_destructive = false
require_confirmation = false
allowed_hosts = ["http://127.0.0.1:9000"]
allowed_origins = ["https://inspector.example.com"]
max_output_bytes = 4096
max_jobs = 1
"#,
        )
        .unwrap();
        assert!(agent.allow_mutations);
        assert!(!agent.require_confirmation);
        assert_eq!(agent.allowed_hosts[0].to_string(), "http://127.0.0.1:9000");
        assert_eq!(
            agent.allowed_origins[0].to_string(),
            "https://inspector.example.com:443"
        );
        assert_eq!(agent.max_output_bytes, 4096);
        assert_eq!(agent.max_jobs.get(), 1);
        assert_eq!(agent.permission(), Permission::Mutations);
    }

    #[test]
    fn destructive_implies_mutations() {
        let agent = parse("[agent]\nallow_destructive = true\n").unwrap();
        assert_eq!(agent.permission(), Permission::Destructive);
    }

    #[test]
    fn invalid_or_unknown_keys_are_errors() {
        // A misspelled permission must never be ignored silently.
        for (text, message) in [
            (
                "[agent]\nallow_mutation = true\n",
                "unknown field `allow_mutation`",
            ),
            (
                "[agent]\nallowed_hosts = [\"example.com\"]\n",
                "invalid origin",
            ),
            ("[agent]\nmax_jobs = 0\n", "nonzero"),
            ("[agent]\nallow_mutations = \"yes\"\n", "expected a boolean"),
        ] {
            let err = parse(text).unwrap_err().to_string();
            assert!(err.contains(message), "{text}: {err}");
        }
    }
}
