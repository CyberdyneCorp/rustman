//! `rustman explore status [<id>]`: the phase, progress and findings by
//! severity of a running or finished exploration (the latest by default).

use clap::Args;
use rustman_explore::explore::rules::SeverityCounts;
use rustman_explore::explore::session::{Phase, RunState, Session, unix_ms};
use serde::Serialize;

use super::RootArgs;
use crate::exit::CliError;

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// The exploration id (default: the most recent one)
    id: Option<String>,
    /// Print the status as JSON
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    root: RootArgs,
}

/// What `explore status` shows.
#[derive(Debug, Serialize)]
struct Status<'a> {
    id: &'a str,
    state: RunState,
    phase: Phase,
    done: usize,
    total: usize,
    findings: SeverityCounts,
    elapsed_ms: u64,
    target: &'a str,
    started_at: &'a str,
    updated_at: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    report: Option<String>,
}

pub fn status_command(args: &StatusArgs) -> Result<(), CliError> {
    let root = args.root.root()?;
    let session = match &args.id {
        Some(id) => Session::open(&root, id)?,
        None => Session::latest(&root)?,
    };
    let state = &session.state;
    let elapsed_ms = match state.state {
        RunState::Running => unix_ms().saturating_sub(state.started_unix_ms),
        RunState::Interrupted | RunState::Completed => state.executed_ms,
    };
    let report = session.report_path();
    let status = Status {
        id: &state.id,
        state: state.state,
        phase: state.phase,
        done: state.progress.done,
        total: state.progress.total,
        findings: state.progress.findings,
        elapsed_ms,
        target: &state.target,
        started_at: &state.started_at,
        updated_at: &state.updated_at,
        report: report.is_file().then(|| report.display().to_string()),
    };
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&status).unwrap_or_default()
        );
        return Ok(());
    }
    print!("{}", render(&status));
    Ok(())
}

fn render(s: &Status<'_>) -> String {
    let mut out = format!(
        "exploration {} ({})\nphase: {}\nprobes: {}/{} done\nfindings: {}\nelapsed: {:.1}s\ntarget: {}\nstarted: {}\n",
        s.id,
        s.state,
        s.phase,
        s.done,
        s.total,
        s.findings,
        std::time::Duration::from_millis(s.elapsed_ms).as_secs_f64(),
        s.target,
        s.started_at,
    );
    if let Some(report) = &s.report {
        out.push_str(&format!("report: {report}\n"));
    }
    out
}
