//! What every front-end does before an exploration executes: the safety
//! policy of its settings, the planner's options, the state of a new
//! exploration and the budgets a resume has left. The CLI and the MCP server
//! share these, so an exploration started by an agent resumes from a
//! terminal.

use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use rustman_core::Environment;
use rustman_core::safety::{
    EnvironmentInfo, Limits, Origin, Permission, PolicyError, PolicyOptions, SafetyPolicy,
};
use rustman_core::settings::Settings as WorkspaceSettings;

use super::graph::Graph;
use super::plan::{Disposition, Plan, PlanOptions};
use super::session::{Phase, Progress, RunState, Settings, State, new_id, unix_ms};
use crate::Imported;
use crate::store::sha256;

/// Why an exploration's safety policy cannot be built.
#[derive(Debug, thiserror::Error)]
pub enum PolicySetupError {
    /// An extra origin that is not `scheme://host[:port]`.
    #[error("{0}")]
    InvalidHost(String),
    #[error(transparent)]
    Policy(#[from] PolicyError),
}

/// The safety policy of an exploration with `settings` against `target`,
/// under `[safety]` and the active environment.
pub fn policy(
    target: &str,
    settings: &Settings,
    workspace: &WorkspaceSettings,
    environment: Option<&Environment>,
) -> Result<SafetyPolicy, PolicySetupError> {
    let allow_hosts = settings
        .allow_hosts
        .iter()
        .map(|h| Origin::parse(h))
        .collect::<Result<Vec<_>, _>>()
        .map_err(PolicySetupError::InvalidHost)?;
    let options = PolicyOptions {
        target,
        permission: Permission::from_flags(settings.allow_mutations, settings.allow_destructive),
        allow_hosts: &allow_hosts,
        production: settings.production.as_deref(),
        environment: environment.map(EnvironmentInfo::from).unwrap_or_default(),
    };
    Ok(SafetyPolicy::new(&workspace.safety, options)?)
}

/// The planner's options for `settings`.
pub fn plan_options<'a>(
    settings: &Settings,
    policy: &'a SafetyPolicy,
    workspace: &WorkspaceSettings,
    environment: Option<&Environment>,
    identity_b: bool,
) -> PlanOptions<'a> {
    PlanOptions {
        families: settings.probes.iter().copied().collect(),
        operations: settings.operations.clone(),
        seed: settings.seed,
        fuzz_cases: settings.fuzz_cases,
        identity_b,
        known: known_variables(workspace, environment),
        policy,
    }
}

/// Variables that have a value: the environment's (secrets included) and
/// `rustman.toml [variables]`.
pub fn known_variables(
    settings: &WorkspaceSettings,
    environment: Option<&Environment>,
) -> BTreeSet<String> {
    let mut known: BTreeSet<String> = settings
        .variables
        .iter()
        .map(|(n, _)| n.to_owned())
        .collect();
    if let Some(env) = environment {
        known.extend(env.variables.iter().map(|(n, _)| n.to_owned()));
        known.extend(env.secrets.iter().cloned());
    }
    known
}

/// What a new exploration starts from.
pub struct NewExploration<'a> {
    pub spec: &'a str,
    pub imported: &'a Imported,
    pub target: &'a str,
    pub environment: Option<&'a Environment>,
    pub identity_b: Option<String>,
    pub settings: Settings,
    pub graph: Graph,
}

/// The state of a new exploration, in the plan phase.
pub fn new_state(new: NewExploration<'_>) -> State {
    let now = rustman_core::run::now_rfc3339();
    State {
        id: new_id(&now),
        phase: Phase::Plan,
        state: RunState::Running,
        started_at: now.clone(),
        started_unix_ms: unix_ms(),
        updated_at: now,
        executed_ms: 0,
        spec: spec_location(new.spec),
        document_sha256: sha256(&new.imported.text),
        target: new.target.to_owned(),
        environment: new.environment.map(|e| e.name.clone()),
        identity_b: new.identity_b,
        settings: new.settings,
        progress: Progress::default(),
        graph: new.graph,
    }
}

/// `spec` as a resume finds it again: a file as an absolute path.
pub fn spec_location(spec: &str) -> String {
    let path = PathBuf::from(spec);
    if spec.contains("://") || !path.is_file() {
        return spec.to_owned();
    }
    dunce::canonicalize(&path).map_or_else(|_| spec.to_owned(), |p| p.display().to_string())
}

/// The budgets of `state` minus what earlier runs used (`sent` requests).
pub fn remaining(state: &State, sent: u64) -> Limits {
    let settings = &state.settings;
    let total = Duration::from_millis(settings.max_duration_ms);
    let used = Duration::from_millis(state.executed_ms);
    Limits {
        rate: NonZeroU32::new(settings.rate).unwrap_or(NonZeroU32::MIN),
        concurrency: NonZeroU32::new(settings.concurrency).unwrap_or(NonZeroU32::MIN),
        max_probes: settings.max_probes.saturating_sub(sent),
        max_duration: total.saturating_sub(used),
    }
}

/// How many probes of `plan` are allowed, blocked and skipped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanCounts {
    pub allowed: usize,
    pub blocked: usize,
    pub skipped: usize,
}

impl PlanCounts {
    pub fn of(plan: &Plan) -> Self {
        let mut counts = Self::default();
        for probe in &plan.probes {
            match probe.disposition {
                Disposition::Allowed => counts.allowed += 1,
                Disposition::Blocked(_) => counts.blocked += 1,
                Disposition::Skipped(_) => counts.skipped += 1,
            }
        }
        counts
    }
}

#[cfg(test)]
mod tests {
    use rustman_core::model::{VarValue, Variables};

    use super::*;

    fn env() -> Environment {
        Environment {
            name: "local".into(),
            variables: Variables::new(vec![("base_url".into(), VarValue::String("x".into()))]),
            secrets: vec!["token".into()],
            production: false,
            trace: Default::default(),
        }
    }

    #[test]
    fn known_variables_include_environment_secrets() {
        let known = known_variables(&WorkspaceSettings::ad_hoc(), Some(&env()));
        assert!(known.contains("base_url") && known.contains("token"));
        assert!(known_variables(&WorkspaceSettings::ad_hoc(), None).is_empty());
    }

    #[test]
    fn a_url_spec_is_kept_and_a_missing_file_too() {
        assert_eq!(
            spec_location("http://x/openapi.yaml"),
            "http://x/openapi.yaml"
        );
        assert_eq!(spec_location("no/such/file.yaml"), "no/such/file.yaml");
    }

    #[test]
    fn invalid_extra_origins_are_refused() {
        let settings = Settings {
            probes: Vec::new(),
            operations: Vec::new(),
            seed: 1,
            fuzz_cases: 1,
            allow_mutations: false,
            allow_destructive: false,
            allow_hosts: vec!["not an origin".into()],
            production: None,
            allow_remote_refs: false,
            rate: 1,
            concurrency: 1,
            max_probes: 1,
            max_duration_ms: 1,
            fail_on: crate::explore::rules::Severity::High,
        };
        let ws = WorkspaceSettings::ad_hoc();
        assert!(policy("http://127.0.0.1:1", &settings, &ws, None).is_err());
    }
}
