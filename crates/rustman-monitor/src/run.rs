//! One monitor run (monitors "One-shot run", design D1): the monitor's
//! targets run once on the `cli-runner` engine (the same loader, variable
//! resolution, assertions and captures as `rustman run`), with every
//! request sent through the [`MonitorGate`], under a deadline for the whole
//! run. The run becomes a [`RunResult`] for the store.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use rustman_core::assertions::Outcome as AssertionOutcome;
use rustman_core::model::Request;
use rustman_core::run::{Plan, RequestRecord, RequestStatus, RunRecord, plan};
use rustman_core::safety::PolicyError;
use rustman_core::{ConfigError, Environment, SecretStore, Workspace};
use rustman_http::RequestError;
use rustman_http::runner::{RunContext, RunOptions, Runner};
use tokio_util::sync::CancellationToken;

use crate::definition::Monitor;
use crate::duration;
use crate::policy::{MonitorGate, allowed_origins, policy};
use crate::store::{Counts, Outcome, RequestResult, RunResult};

/// Exit codes of a run (ROADMAP exit codes).
pub mod exit {
    pub const PASS: u8 = 0;
    pub const FAIL: u8 = 1;
    pub const ERROR: u8 = 3;
    pub const BLOCKED: u8 = 4;
    pub const INTERRUPTED: u8 = 130;
}

/// Why a monitor could not run at all.
#[derive(Debug, thiserror::Error)]
pub enum MonitorError {
    /// A missing environment or target, or an unsupported request kind.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// No allowlist could be built.
    #[error(transparent)]
    Policy(#[from] PolicyError),
    /// The runner could not start (cookie jar, persisted captures).
    #[error(transparent)]
    Request(#[from] RequestError),
    /// The result or the alert state could not be stored.
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),
}

/// A finished run.
#[derive(Debug)]
pub struct MonitorRun {
    /// What the store keeps.
    pub result: RunResult,
    /// The whole run, redacted, for reports.
    pub record: RunRecord,
    /// A request was refused by the monitor's policy.
    pub blocked: bool,
    /// Stopped by Ctrl+C or a daemon shutdown (not by the deadline).
    pub interrupted: bool,
}

impl MonitorRun {
    /// 0 passed, 1 an assertion failed, 3 a request errored, 4 a request
    /// was blocked, 130 interrupted.
    pub fn exit_code(&self) -> u8 {
        if self.interrupted {
            return exit::INTERRUPTED;
        }
        match self.result.outcome {
            Outcome::Error if self.blocked => exit::BLOCKED,
            Outcome::Pass | Outcome::Skipped => exit::PASS,
            Outcome::Fail => exit::FAIL,
            Outcome::Error => exit::ERROR,
        }
    }
}

/// Run `monitor` of `ws` once, started at `at`. `cancel` stops it early
/// (the result is then an `error`).
pub async fn run(
    ws: &Workspace,
    monitor: &Monitor,
    secrets: &SecretStore,
    at: Timestamp,
    cancel: CancellationToken,
) -> Result<MonitorRun, MonitorError> {
    let env = environment(ws, &monitor.environment)?;
    let scopes = scopes(&ws.root, monitor)?;
    let plan = plan(ws, &scopes, &[]);
    check_plan(monitor, &plan)?;
    let ctx = RunContext {
        root: &ws.root,
        settings: &ws.settings,
        environment: Some(env),
        secrets,
        in_workspace: true,
    };
    let origins = allowed_origins(&plan, &ctx);
    let policy = policy(&ws.settings.safety, &origins, monitor.allow_mutations).map_err(|_| {
        ConfigError::Usage(format!(
            "monitor '{}': no target host found in environment '{}' or its requests",
            monitor.name, monitor.environment
        ))
    })?;
    let gate = MonitorGate::new(policy, &ws.settings.safety, monitor.allow_mutations);
    let deadline = CancellationToken::new();
    let options = RunOptions {
        gate: Some(&gate),
        cancel: deadline.clone(),
        ..RunOptions::default()
    };
    let runner = Runner::new(ctx, options)?;
    let (record, timed_out) =
        with_deadline(runner.run(&plan), monitor.timeout, &deadline, &cancel).await;
    // The cookie jar is saved as after `rustman run`; failing to is not
    // the monitored API's fault.
    let _ = runner.finish();
    let interrupted = record.summary.interrupted && !timed_out;
    let result = summarize(
        &record,
        at,
        timed_out.then_some(monitor.timeout),
        interrupted,
    );
    Ok(MonitorRun {
        blocked: record.requests.iter().any(is_blocked),
        interrupted,
        result,
        record,
    })
}

pub(crate) fn environment<'a>(
    ws: &'a Workspace,
    name: &str,
) -> Result<&'a Environment, ConfigError> {
    ws.environments
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| ConfigError::EnvironmentNotFound {
            name: name.to_owned(),
            available: ws.environments.iter().map(|e| e.name.clone()).collect(),
        })
}

/// The targets relative to the workspace root; each must exist inside it.
fn scopes(root: &Path, monitor: &Monitor) -> Result<Vec<PathBuf>, ConfigError> {
    let canonical_root = std::fs::canonicalize(root).map_err(|e| ConfigError::Io {
        path: root.to_path_buf(),
        source: e,
    })?;
    monitor
        .targets
        .iter()
        .map(|target| {
            let problem = |what: &str| {
                ConfigError::Usage(format!(
                    "{}: target '{target}' {what}",
                    monitor.file.display()
                ))
            };
            let full =
                std::fs::canonicalize(root.join(target)).map_err(|_| problem("does not exist"))?;
            full.strip_prefix(&canonical_root)
                .map(Path::to_path_buf)
                .map_err(|_| problem("is outside the workspace"))
        })
        .collect()
}

/// The targets hold requests, all of them HTTP: the monitor's policy
/// governs HTTP sends.
fn check_plan(monitor: &Monitor, plan: &Plan<'_>) -> Result<(), ConfigError> {
    if plan.is_empty() {
        return Err(ConfigError::Usage(format!(
            "{}: the targets hold no requests",
            monitor.file.display()
        )));
    }
    for request in &plan.requests {
        let kind = request.request();
        if !matches!(kind, Request::Http(_)) {
            return Err(ConfigError::Usage(format!(
                "{}: monitors run http requests only; {} is a {} request",
                monitor.file.display(),
                request.entry.path.to_string_lossy().replace('\\', "/"),
                kind.kind()
            )));
        }
    }
    Ok(())
}

/// Await `run`; once `timeout` passes, or `cancel` fires, cancel the run
/// through `deadline` and await what it recorded. True when the deadline
/// stopped it.
async fn with_deadline(
    run: impl Future<Output = RunRecord>,
    timeout: Duration,
    deadline: &CancellationToken,
    cancel: &CancellationToken,
) -> (RunRecord, bool) {
    tokio::pin!(run);
    tokio::select! {
        record = &mut run => (record, false),
        () = tokio::time::sleep(timeout) => {
            deadline.cancel();
            (run.await, true)
        }
        () = cancel.cancelled() => {
            deadline.cancel();
            (run.await, false)
        }
    }
}

/// The stored form of `record`.
fn summarize(
    record: &RunRecord,
    at: Timestamp,
    timed_out: Option<Duration>,
    interrupted: bool,
) -> RunResult {
    let summary = &record.summary;
    let outcome = if timed_out.is_some() || summary.errored > 0 && !interrupted {
        Outcome::Error
    } else if interrupted {
        // A shutdown, not a verdict on the target: like an overlap skip,
        // it counts toward neither uptime nor the alert rules.
        Outcome::Skipped
    } else if summary.total == 0 || summary.errored > 0 {
        Outcome::Error
    } else if summary.failed > 0 {
        Outcome::Fail
    } else {
        Outcome::Pass
    };
    let message = match timed_out {
        Some(t) => Some(format!(
            "run exceeded its timeout of {}",
            duration::format(t)
        )),
        None => interrupted.then(|| "interrupted".to_owned()),
    };
    RunResult {
        at,
        outcome,
        duration_ms: summary.duration_ms,
        summary: Counts {
            total: summary.total,
            passed: summary.passed,
            failed: summary.failed,
            errored: summary.errored,
            skipped: summary.skipped,
        },
        requests: record.requests.iter().map(request_result).collect(),
        failures: record.requests.iter().flat_map(failures).collect(),
        message,
    }
}

fn request_result(record: &RequestRecord) -> RequestResult {
    RequestResult {
        id: record.id.clone(),
        status: status_name(record.status).to_owned(),
        code: record.exchange.as_ref().map(|e| e.response.status),
        duration_ms: record.duration_ms,
    }
}

fn status_name(status: RequestStatus) -> &'static str {
    match status {
        RequestStatus::Passed => "passed",
        RequestStatus::Failed => "failed",
        RequestStatus::Errored => "errored",
        RequestStatus::Skipped => "skipped",
    }
}

/// The request's error and failed assertions, as `<id>: <message>`
/// (already redacted by the runner).
fn failures(record: &RequestRecord) -> Vec<String> {
    let error = record.error.iter().map(String::as_str);
    let assertions = record
        .assertions
        .iter()
        .filter(|a| a.outcome != AssertionOutcome::Pass)
        .map(|a| a.message.as_str());
    error
        .chain(assertions)
        .map(|message| format!("{}: {message}", record.id))
        .collect()
}

fn is_blocked(record: &RequestRecord) -> bool {
    record
        .error
        .as_deref()
        .is_some_and(|e| e.starts_with("blocked: "))
}

#[cfg(test)]
mod tests {
    use rustman_core::run::Summary;

    use super::*;

    fn record(errored: usize, failed: usize, interrupted: bool) -> RunRecord {
        RunRecord {
            started_at: "2026-03-01T10:00:00Z".into(),
            environment: None,
            requests: Vec::new(),
            summary: Summary {
                total: 2,
                passed: 2 - errored - failed,
                failed,
                errored,
                interrupted,
                ..Summary::default()
            },
        }
    }

    fn outcome(record: &RunRecord, timed_out: Option<Duration>) -> Outcome {
        let at = "2026-03-01T10:00:00Z".parse().unwrap();
        summarize(record, at, timed_out, record.summary.interrupted).outcome
    }

    /// A run cut short by a shutdown (SIGTERM during a deploy) is stored
    /// as skipped: it must not count toward uptime or fire an alert.
    #[test]
    fn an_interrupted_run_is_skipped_not_an_error() {
        let interrupted = record(1, 0, true);
        assert_eq!(outcome(&interrupted, None), Outcome::Skipped);
        let message = summarize(&interrupted, Timestamp::UNIX_EPOCH, None, true).message;
        assert_eq!(message.as_deref(), Some("interrupted"));
        assert_eq!(outcome(&record(0, 1, true), None), Outcome::Skipped);
        // A timeout is still a verdict on the target.
        let timeout = Some(Duration::from_secs(60));
        assert_eq!(outcome(&record(1, 0, false), timeout), Outcome::Error);
        assert_eq!(outcome(&record(1, 0, false), None), Outcome::Error);
        assert_eq!(outcome(&record(0, 1, false), None), Outcome::Fail);
        assert_eq!(outcome(&record(0, 0, false), None), Outcome::Pass);
    }
}
