//! OpenAPI and exploration tools (spec "OpenAPI and exploration tools"):
//! `import_openapi`, `explore_plan`, `explore_run`, `explore_status` and
//! `explore_cancel`, with the semantics of `rustman import openapi` and
//! `rustman explore`. Explorations started here are journaled like the
//! CLI's, so `rustman explore resume <id>` continues them.

use std::sync::Arc;
use std::time::Duration;

use rustman_core::safety::{Governor, Limits, PolicyError, SafetyPolicy};
use rustman_explore::collection::collection_dir;
use rustman_explore::explore::drive::{Drive, Finished, drive};
use rustman_explore::explore::exec::{
    Context, Executor, Identity, exploration_export, exploration_send_options,
};
use rustman_explore::explore::graph::Graph;
use rustman_explore::explore::plan::{self, DEFAULT_FUZZ_CASES, Family, Plan};
use rustman_explore::explore::rules::Severity;
use rustman_explore::explore::session::{RunState, Session, SessionError, Settings};
use rustman_explore::explore::setup::{self, NewExploration, PlanCounts, PolicySetupError};
use rustman_explore::store::{Target as Collection, generate_for, write_import};
use rustman_explore::{ImportError, ImportOptions, Imported, import, sync};
use rustman_http::GovernedSender;
use rustman_report::exploration::SeverityCounts;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::execute::contained;
use super::{Call, output};
use crate::server::context::{AgentContext, ToolOutput};
use crate::server::error::{ErrorCode, ToolError};
use crate::server::jobs::{CANCEL_WAIT, Job, JobKind, JobState};
use crate::server::limits::{paginate, snapshot};
use crate::server::policy::agent_safety;

/// The seed of agent explorations without one: plans stay the same between
/// `explore_plan` pages and the `explore_run` that follows.
const DEFAULT_SEED: u64 = 0;

/// The URI prefix of reports.
pub const REPORT_URI_PREFIX: &str = "rustman://reports/";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportArgs {
    /// The OpenAPI document: a file relative to the workspace root, or an
    /// http(s) URL on an allowed host.
    pub spec: String,
    /// The collection folder, relative to the workspace root (default:
    /// named after the API's title).
    pub into: Option<String>,
    /// Only report what would change (default true). Writing needs the
    /// human's confirmation.
    pub dry_run: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ImportResult {
    pub dry_run: bool,
    /// The collection folder, relative to the workspace root.
    pub collection: String,
    /// `create` for a first import, `sync` to merge into an existing one.
    pub mode: String,
    /// Request files generated, one per operation.
    pub requests: usize,
    /// Environments created (or, in a dry run, that would be).
    pub environments: Vec<String>,
    /// What a sync changes, per request file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<Value>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ExploreArgs {
    /// The OpenAPI document: a file relative to the workspace root, or an
    /// http(s) URL on an allowed host.
    pub spec: String,
    /// The base URL probes go to; its origin must be allowed for agents.
    pub target: String,
    /// Probe families (default: all but bola): conformance, auth,
    /// negative-input, method-not-allowed, lifecycle, bola, fuzz,
    /// undocumented-status, error-leakage, security-headers.
    pub probes: Option<Vec<String>>,
    /// Only operations matching these globs over operationId, path or
    /// `METHOD /path`.
    #[serde(default)]
    pub operations: Vec<String>,
    /// The fuzz seed (default 0, so plans are reproducible).
    pub seed: Option<u64>,
    /// At most this many fuzz cases per operation.
    pub fuzz_cases: Option<usize>,
    /// Also plan POST, PUT and PATCH probes; needs `[agent]
    /// allow_mutations` and the human's confirmation.
    #[serde(default)]
    pub allow_mutations: bool,
    /// Also plan DELETE probes; needs `[agent] allow_destructive`.
    #[serde(default)]
    pub allow_destructive: bool,
    /// The `next_cursor` of the previous page of probes (`explore_plan`).
    pub cursor: Option<String>,
    /// Probes per page (default 50; `explore_plan`).
    pub limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ExploreRunArgs {
    #[serde(flatten)]
    pub explore: ExploreArgs,
    /// Probe budget (default: 1000).
    pub max_probes: Option<u64>,
    /// Time budget in milliseconds (default: 15 minutes).
    pub max_duration_ms: Option<u64>,
    /// The severity the report counts as failing: critical, high (default),
    /// medium, low or info.
    pub fail_on: Option<String>,
}

/// One planned probe.
#[derive(Debug, Hash, Serialize, JsonSchema)]
pub struct ProbeView {
    pub id: String,
    pub family: String,
    /// `METHOD /path` of the operation probed.
    pub operation: String,
    pub method: String,
    /// What the probe varies.
    pub variant: String,
    /// `allowed`, `blocked: <reason>` or `skipped: <reason>`.
    pub disposition: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PlanResult {
    pub seed: u64,
    pub total: usize,
    pub allowed: usize,
    pub blocked: usize,
    pub skipped: usize,
    pub probes: Vec<ProbeView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExplorationStarted {
    pub job_id: String,
    /// Resumable with `rustman explore resume <exploration_id>`.
    pub exploration_id: String,
    pub seed: u64,
    pub probes: usize,
    pub allowed: usize,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatusArgs {
    /// A job id (`job-1`) or exploration id (`exp-...`); default: the
    /// latest exploration.
    pub id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelArgs {
    /// The job id `explore_run` returned (or its exploration id).
    pub job_id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExplorationStatus {
    pub exploration_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    /// `running`, `interrupted`, `cancelled`, `failed` or `completed`.
    pub state: String,
    /// `discover`, `plan`, `execute` or `report`.
    pub phase: String,
    pub done: usize,
    pub total: usize,
    pub findings: SeverityCounts,
    pub target: String,
    pub started_at: String,
    pub updated_at: String,
    /// `rustman://reports/<id>` once the report exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `spec` as the importer reads it: an http(s) URL whose origin agents may
/// reach, or a file inside the workspace.
pub fn spec_source(ctx: &AgentContext, spec: &str) -> Result<String, ToolError> {
    if spec.contains("://") {
        ctx.sender()?
            .policy()
            .check_origin(spec)
            .map_err(|blocked| ToolError::blocked(blocked.reason()))?;
        return Ok(spec.to_owned());
    }
    let file = ctx.workspace().root.join(contained(spec)?);
    if !file.is_file() {
        return Err(ToolError::new(
            ErrorCode::NotFound,
            format!("no file '{spec}' in the workspace"),
        ));
    }
    Ok(file.to_string_lossy().into_owned())
}

/// Read `spec` for `call`. A URL is fetched through the session's sender,
/// so the policy checks it and every redirect hop, and the audit line
/// names it.
async fn load(call: &Call, spec: &str) -> Result<Imported, ToolError> {
    let ctx = &call.ctx;
    let source = spec_source(ctx, spec)?;
    let mut options = ImportOptions::default();
    if spec.contains("://") {
        call.target.set("GET", &source);
        options.fetch.sender = Some(ctx.sender()?);
    }
    import(&source, &options).await.map_err(|e| match e {
        ImportError::Blocked { reason, .. } => ToolError::blocked(reason),
        other => ToolError::new(ErrorCode::InvalidInput, other.to_string()),
    })
}

fn relative(ctx: &AgentContext, path: &std::path::Path) -> String {
    path.strip_prefix(&ctx.workspace().root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub async fn import_openapi(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: ImportArgs = call.args()?;
    let ctx = &call.ctx;
    let imported = load(call, &args.spec).await?;
    let root = ctx.workspace().root.clone();
    let dir = match &args.into {
        Some(into) => root.join(contained(into)?),
        None => root.join(collection_dir(&imported.model)),
    };
    let collection = Collection::new(root, dir);
    let shown = relative(ctx, &collection.dir);
    let dry_run = args.dry_run.unwrap_or(true);
    if !dry_run {
        let message = format!(
            "An agent wants to import {} into the collection '{shown}', writing workspace files. Allow it?",
            args.spec
        );
        let what = format!("writing the collection '{shown}'");
        ctx.ask(&message, &what, &call.peer).await?;
    }
    let mut result = if is_non_empty_dir(&collection.dir) {
        sync_import(&imported, &collection, dry_run)?
    } else {
        first_import(&imported, &collection, dry_run)?
    };
    result.collection = shown;
    let verb = if dry_run { "would import" } else { "imported" };
    let line = format!(
        "{verb} {} operations into {} ({})",
        result.requests, result.collection, result.mode
    );
    output(&result, line)
}

fn is_non_empty_dir(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some())
}

fn first_import(
    imported: &Imported,
    collection: &Collection,
    dry_run: bool,
) -> Result<ImportResult, ToolError> {
    let generated = generate_for(imported, collection);
    let mut environments: Vec<String> = generated
        .environments
        .iter()
        .filter(|e| !collection.environment_path(&e.name).exists())
        .map(|e| e.name.clone())
        .collect();
    if !dry_run {
        let outcome = write_import(imported, &generated, collection)
            .map_err(|e| ToolError::internal(e.to_string()))?;
        environments = outcome.created;
    }
    Ok(ImportResult {
        dry_run,
        collection: String::new(),
        mode: "create".into(),
        requests: generated.requests,
        environments,
        sync: None,
        warnings: diagnostics(imported, generated.warnings.iter()),
    })
}

fn sync_import(
    imported: &Imported,
    collection: &Collection,
    dry_run: bool,
) -> Result<ImportResult, ToolError> {
    let mut planned =
        sync::plan(imported, collection).map_err(|e| ToolError::internal(e.to_string()))?;
    planned.summary.dry_run = dry_run;
    if !dry_run {
        sync::apply(&planned).map_err(|e| ToolError::internal(e.to_string()))?;
    }
    let mut warnings = diagnostics(imported, planned.generated.warnings.iter());
    warnings.extend(planned.warnings.iter().cloned());
    Ok(ImportResult {
        dry_run,
        collection: String::new(),
        mode: "sync".into(),
        requests: planned.generated.requests,
        environments: Vec::new(),
        sync: serde_json::to_value(&planned.summary).ok(),
        warnings,
    })
}

fn diagnostics<'a, D: ToString + 'a>(
    imported: &Imported,
    more: impl Iterator<Item = &'a D>,
) -> Vec<String> {
    let mut lines: Vec<String> = imported.warnings.iter().map(ToString::to_string).collect();
    lines.extend(more.map(ToString::to_string));
    lines
}

/// Refuse probe methods the agent permissions do not allow.
fn check_permissions(ctx: &AgentContext, args: &ExploreArgs) -> Result<(), ToolError> {
    let agent = ctx.agent_settings();
    if args.allow_destructive && !agent.allow_destructive {
        return Err(ToolError::blocked("destructive not allowed for agents"));
    }
    let mutations = args.allow_mutations || args.allow_destructive;
    if mutations && !(agent.allow_mutations || agent.allow_destructive) {
        return Err(ToolError::blocked("mutations not allowed for agents"));
    }
    Ok(())
}

/// The exploration settings of `args` under the session's limits. Agents
/// never confirm production hosts nor add origins.
fn settings(
    ctx: &AgentContext,
    args: &ExploreArgs,
    limits: Limits,
    fail_on: Severity,
) -> Result<Settings, ToolError> {
    check_permissions(ctx, args)?;
    let probes = match &args.probes {
        Some(list) => Family::parse_list(&list.join(","))
            .map_err(|e| ToolError::new(ErrorCode::InvalidInput, e))?,
        None => Family::defaults(),
    };
    Ok(Settings {
        probes: probes.into_iter().collect(),
        operations: args.operations.clone(),
        seed: args.seed.unwrap_or(DEFAULT_SEED),
        fuzz_cases: args.fuzz_cases.unwrap_or(DEFAULT_FUZZ_CASES),
        allow_mutations: args.allow_mutations || args.allow_destructive,
        allow_destructive: args.allow_destructive,
        allow_hosts: Vec::new(),
        production: None,
        allow_remote_refs: false,
        rate: limits.rate.get(),
        concurrency: limits.concurrency.get(),
        max_probes: limits.max_probes,
        max_duration_ms: u64::try_from(limits.max_duration.as_millis()).unwrap_or(u64::MAX),
        fail_on,
    })
}

/// The exploration's safety policy: its target must be an origin agents
/// may reach, the other allowed origins are the agent's allowlist (never
/// `[safety].allowed_hosts`), and production is refused.
fn policy(
    ctx: &AgentContext,
    target: &str,
    settings: &Settings,
) -> Result<SafetyPolicy, ToolError> {
    let sender = ctx.sender()?;
    let agent = sender.policy();
    agent
        .check_origin(target)
        .map_err(|blocked| ToolError::blocked(blocked.reason()))?;
    // Probes and their redirects may reach the agent's allowlist only.
    let mut workspace = ctx.workspace().settings.clone();
    workspace.safety = agent_safety(&workspace.safety, agent.allowed_origins());
    let environment = ctx.environment();
    setup::policy(target, settings, &workspace, environment.as_ref()).map_err(|e| match e {
        PolicySetupError::Policy(PolicyError::InvalidTarget(m))
        | PolicySetupError::InvalidHost(m) => ToolError::new(ErrorCode::InvalidInput, m),
        PolicySetupError::Policy(other) => ToolError::blocked(other.to_string()),
    })
}

/// An exploration ready to execute: discovered and planned.
struct Planned {
    imported: Imported,
    graph: Graph,
    plan: Plan,
    policy: SafetyPolicy,
    settings: Settings,
}

async fn discover_and_plan(
    call: &Call,
    args: &ExploreArgs,
    limits: Limits,
    fail_on: Severity,
) -> Result<Planned, ToolError> {
    let ctx = &call.ctx;
    let settings = settings(ctx, args, limits, fail_on)?;
    let policy = policy(ctx, &args.target, &settings)?;
    let imported = load(call, &args.spec).await?;
    let ws = &ctx.workspace().settings;
    let graph = Graph::build(&imported.model, &ws.explore.producers);
    let environment = ctx.environment();
    let options = setup::plan_options(&settings, &policy, ws, environment.as_ref(), false);
    let plan = plan::plan(&imported.model, &graph, &options);
    Ok(Planned {
        imported,
        graph,
        plan,
        policy,
        settings,
    })
}

fn probe_view(probe: &plan::Probe) -> ProbeView {
    ProbeView {
        id: probe.id.clone(),
        family: probe.family.as_str().to_owned(),
        operation: probe.operation.clone(),
        method: probe.method.clone(),
        variant: probe.variant.clone(),
        disposition: probe.disposition.to_string(),
    }
}

pub async fn explore_plan(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: ExploreArgs = call.args()?;
    let ctx = &call.ctx;
    let limits = Limits::from_settings(&ctx.workspace().settings.safety);
    let planned = discover_and_plan(call, &args, limits, Severity::High).await?;
    let counts = PlanCounts::of(&planned.plan);
    let probes: Vec<ProbeView> = planned.plan.probes.iter().map(probe_view).collect();
    let total = probes.len();
    let page = paginate(
        probes,
        snapshot([planned.plan.seed, total as u64]),
        args.cursor.as_deref(),
        args.limit,
    )?;
    let result = PlanResult {
        seed: planned.plan.seed,
        total,
        allowed: counts.allowed,
        blocked: counts.blocked,
        skipped: counts.skipped,
        probes: page.items,
        next_cursor: page.next_cursor,
    };
    let line = format!(
        "plan: {total} probes ({} allowed, {} blocked, {} skipped), seed {}; nothing was sent",
        counts.allowed, counts.blocked, counts.skipped, result.seed
    );
    output(&result, line)
}

/// The limits of an agent exploration: `[safety]` rate and concurrency
/// with the budgets of `args`.
fn run_limits(ctx: &AgentContext, args: &ExploreRunArgs) -> Limits {
    let defaults = Limits::from_settings(&ctx.workspace().settings.safety);
    Limits {
        max_probes: args
            .max_probes
            .unwrap_or(rustman_core::safety::DEFAULT_MAX_PROBES),
        max_duration: args.max_duration_ms.map_or(
            rustman_core::safety::DEFAULT_MAX_DURATION,
            Duration::from_millis,
        ),
        ..defaults
    }
}

pub async fn explore_run(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: ExploreRunArgs = call.args()?;
    let ctx = &call.ctx;
    let fail_on = match &args.fail_on {
        Some(text) => text
            .parse::<Severity>()
            .map_err(|e| ToolError::new(ErrorCode::InvalidInput, e))?,
        None => Severity::High,
    };
    let limits = run_limits(ctx, &args);
    let explore = &args.explore;
    let planned = discover_and_plan(call, explore, limits, fail_on).await?;
    if planned.settings.allow_mutations {
        let environment = ctx.environment().map_or_else(
            || "no environment".into(),
            |e| format!("environment '{}'", e.name),
        );
        let target = ctx.redactor().text(&explore.target);
        let message = format!(
            "An agent wants to run an exploration with mutating probes against {target} ({environment}). Allow it?"
        );
        ctx.ask(
            &message,
            &format!("a mutating exploration of {target}"),
            &call.peer,
        )
        .await?;
    }
    let max = usize::try_from(ctx.agent_settings().max_jobs.get()).unwrap_or(usize::MAX);
    let environment = ctx.environment();
    let counts = PlanCounts::of(&planned.plan);
    let state = setup::new_state(NewExploration {
        spec: &spec_source(ctx, &explore.spec)?,
        imported: &planned.imported,
        target: &explore.target,
        environment: environment.as_ref(),
        identity_b: None,
        settings: planned.settings.clone(),
        graph: planned.graph.clone(),
    });
    let job = ctx
        .jobs()
        .start(JobKind::Exploration, Some(state.id.clone()), max)?;
    let session = match Session::create(&ctx.workspace().root, state, &planned.plan) {
        Ok(session) => session,
        Err(err) => {
            job.finish(JobState::Failed, Some(err.to_string()), None);
            return Err(ToolError::internal(err.to_string()));
        }
    };
    let started = ExplorationStarted {
        job_id: job.id.clone(),
        exploration_id: session.state.id.clone(),
        seed: planned.plan.seed,
        probes: planned.plan.probes.len(),
        allowed: counts.allowed,
    };
    job.set_progress(0, planned.plan.probes.len());
    let owned = (call.ctx.clone(), job.clone());
    ctx.jobs().spawn(&job, move || {
        exploration_job(owned.0, owned.1, session, planned, limits)
    });
    call.watch(&job);
    let line = format!(
        "{} started exploration {}: {} probes ({} allowed)",
        started.job_id, started.exploration_id, started.probes, started.allowed
    );
    output(&started, line)
}

async fn exploration_job(
    ctx: Arc<AgentContext>,
    job: Arc<Job>,
    mut session: Session,
    planned: Planned,
    limits: Limits,
) {
    let ws = ctx.workspace();
    let environment = ctx.environment();
    let sender = GovernedSender::new(planned.policy, Governor::new(limits));
    let graph = session.state.graph.clone();
    let target = session.state.target.clone();
    let model = &planned.imported.model;
    let send = match exploration_send_options(&ws.settings, environment.as_ref(), ctx.secrets()) {
        Ok(send) => send,
        Err(e) => return job.finish(JobState::Failed, Some(e), None),
    };
    let context = Context {
        model,
        graph: &graph,
        target: &target,
        variables: &ws.settings.variables,
        identity: Identity {
            environment: environment.as_ref(),
            secrets: ctx.secrets(),
        },
        identity_b: None,
        scans: &planned.plan.scans,
        disabled_rules: &ws.settings.explore.disabled_rules,
        send,
    };
    // The probes' client spans go to the process's exporter, whose worker
    // sends them within its batch delay: the server outlives the job.
    let export = exploration_export(&ws.settings, environment.as_ref());
    let executor = Executor::new(&sender, context).exporting(export);
    let options = Drive {
        plan: &planned.plan,
        model,
        concurrency: usize::try_from(limits.concurrency.get()).unwrap_or(1),
        cancel: job.cancel_token(),
        progress: &mut |p, _| job.set_progress(p.done, p.total),
        probe: None,
    };
    match drive(&mut session, &executor, options).await {
        Ok(Finished::Completed(_)) => job.finish(JobState::Completed, None, None),
        Ok(Finished::Interrupted(_)) => job.finish(JobState::Cancelled, None, None),
        Err(err) => job.finish(JobState::Failed, Some(err.to_string()), None),
    }
}

/// The exploration `id` names (a job or exploration id; the latest
/// without one), with the job of this session that drives it.
fn find_exploration(
    ctx: &AgentContext,
    id: Option<&str>,
) -> Result<(Session, Option<Arc<Job>>), ToolError> {
    let root = &ctx.workspace().root;
    let jobs = ctx.jobs();
    let exploration = match id {
        Some(id) if id.starts_with("job-") => {
            let job = jobs.get(id).ok_or_else(|| not_found(id))?;
            job.exploration.clone().ok_or_else(|| {
                ToolError::new(
                    ErrorCode::InvalidInput,
                    format!("{id} is not an exploration"),
                )
            })?
        }
        Some(id) => id.to_owned(),
        None => Session::latest(root).map_err(session_error)?.state.id,
    };
    let session = Session::open(root, &exploration).map_err(session_error)?;
    Ok((session, jobs.for_exploration(&exploration)))
}

fn not_found(id: &str) -> ToolError {
    ToolError::new(ErrorCode::NotFound, format!("no job or exploration '{id}'"))
}

pub fn session_error(err: SessionError) -> ToolError {
    match err {
        SessionError::NotFound(id) => not_found(&id),
        SessionError::None => ToolError::new(ErrorCode::NotFound, "no exploration yet"),
        other => ToolError::internal(other.to_string()),
    }
}

fn status(session: &Session, job: Option<&Job>) -> ExplorationStatus {
    let state = &session.state;
    let job_state = job.map(Job::state);
    let shown = match job_state {
        Some(JobState::Running) => "running".to_owned(),
        Some(JobState::Cancelled) => "cancelled".to_owned(),
        Some(JobState::Failed) => "failed".to_owned(),
        _ => state.state.to_string(),
    };
    let report = session
        .report_path()
        .is_file()
        .then(|| format!("{REPORT_URI_PREFIX}{}", state.id));
    // A running job knows its progress before the journaled state does, and
    // so does one cancelled before the journal recorded any.
    let journaled = state.progress.total > 0;
    let live = job
        .filter(|j| j.state() == JobState::Running || !journaled)
        .map(Job::progress);
    let (done, total) = live.map_or((state.progress.done, state.progress.total), |p| {
        (p.done, p.total)
    });
    ExplorationStatus {
        exploration_id: state.id.clone(),
        job_id: job.map(|j| j.id.clone()),
        state: shown,
        phase: state.phase.to_string(),
        done,
        total,
        findings: state.progress.findings,
        target: state.target.clone(),
        started_at: state.started_at.clone(),
        updated_at: state.updated_at.clone(),
        report,
        error: job.and_then(Job::error),
    }
}

fn status_output(status: &ExplorationStatus) -> Result<ToolOutput, ToolError> {
    let line = format!(
        "exploration {} {} ({}): {}/{} probes, {} findings",
        status.exploration_id,
        status.state,
        status.phase,
        status.done,
        status.total,
        status.findings.total()
    );
    output(status, line)
}

pub fn explore_status(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: StatusArgs = call.args()?;
    let (session, job) = find_exploration(&call.ctx, args.id.as_deref())?;
    status_output(&status(&session, job.as_deref()))
}

pub async fn explore_cancel(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: CancelArgs = call.args()?;
    let (_, job) = find_exploration(&call.ctx, Some(&args.job_id))?;
    let job = job.ok_or_else(|| {
        ToolError::new(
            ErrorCode::NotFound,
            format!("no job of this session drives '{}'", args.job_id),
        )
    })?;
    job.cancel(CANCEL_WAIT).await;
    let (session, job) = find_exploration(&call.ctx, Some(&job.id))?;
    let status = status(&session, job.as_deref());
    if session.state.state == RunState::Interrupted {
        tracing::info!("exploration {} cancelled; resumable", status.exploration_id);
    }
    status_output(&status)
}
