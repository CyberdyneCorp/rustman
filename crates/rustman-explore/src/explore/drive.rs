//! Driving one exploration's execute and report phases on its
//! [`Session`]: run the probes the journal does not hold yet, keep
//! `state.json` current for `explore status`, and write `report.json` when
//! every probe has run. `explore run` and `explore resume` both end here.

use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::exec::{Executor, Outcome};
use super::journal::{self, Journal};
use super::plan::Plan;
use super::report::{ExplorationReport, build};
use super::runner::{DRAIN, ProbeObserver, RunOptions, execute};
use super::session::{Phase, Progress, REPORT_FILE, RunState, Session, SessionError};
use crate::openapi::model::ApiModel;

/// How often `state.json` is rewritten while probes run.
const SAVE_EVERY: Duration = Duration::from_millis(500);

/// What the phases need besides the session and the executor.
pub struct Drive<'a> {
    pub plan: &'a Plan,
    pub model: &'a ApiModel,
    /// Probes in flight at once.
    pub concurrency: usize,
    pub cancel: CancellationToken,
    /// Called with the progress and the execution time so far.
    pub progress: &'a mut dyn FnMut(&Progress, Duration),
    /// Called with each probe's outcome and duration as it finishes.
    pub probe: Option<&'a mut ProbeObserver<'a>>,
}

/// How the exploration ended.
#[derive(Debug)]
pub enum Finished {
    /// Stopped by a cancellation; `explore resume` continues it.
    Interrupted(Progress),
    Completed(Box<ExplorationReport>),
}

/// Run the execute phase, then (unless interrupted) the report phase.
pub async fn drive(
    session: &mut Session,
    executor: &Executor<'_>,
    drive: Drive<'_>,
) -> Result<Finished, SessionError> {
    session.state.state = RunState::Running;
    session.enter(Phase::Execute)?;
    let path = session.journal_path();
    let io = |source| SessionError::Io {
        path: path.clone(),
        source,
    };
    let entries = journal::read(&path).map_err(io)?;
    let mut journal = Journal::open(&path).map_err(io)?;
    let started = Instant::now();
    let before = Duration::from_millis(session.state.executed_ms);
    let mut last_save = Instant::now();
    let mut save_error = None;
    let progress = drive.progress;
    let mut on_progress = |p: &Progress| {
        progress(p, before + started.elapsed());
        session.state.progress = *p;
        if last_save.elapsed() >= SAVE_EVERY {
            last_save = Instant::now();
            session.touch();
            if let Err(e) = session.save() {
                save_error.get_or_insert(e);
            }
        }
    };
    let options = RunOptions {
        concurrency: drive.concurrency,
        cancel: drive.cancel,
        drain: DRAIN,
        progress: &mut on_progress,
        // Shortened to this call: the trait object behind `&mut` keeps
        // its own lifetime otherwise.
        probe: drive
            .probe
            .map(|p| &mut *p as &mut dyn FnMut(&Outcome, Duration)),
    };
    let result = execute(executor, drive.plan, &mut journal, entries, options).await;
    let executed = result.map_err(io)?;
    if let Some(e) = save_error {
        return Err(e);
    }
    session.state.executed_ms = millis(before + started.elapsed());
    if executed.interrupted {
        session.state.state = RunState::Interrupted;
        session.touch();
        session.save()?;
        return Ok(Finished::Interrupted(session.state.progress));
    }
    session.enter(Phase::Report)?;
    let report = build(&session.state, drive.plan, drive.model, &executed.entries);
    session.write_json(REPORT_FILE, &report)?;
    session.state.state = RunState::Completed;
    session.state.progress.findings = report.summary.findings;
    session.touch();
    session.save()?;
    Ok(Finished::Completed(Box::new(report)))
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
