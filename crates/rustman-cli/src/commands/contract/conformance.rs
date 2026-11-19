//! `rustman contract verify` (contract-testing "Passive conformance of
//! recorded traffic", "Conformance coverage and unmatched traffic"):
//! recorded exchanges checked against an OpenAPI document. It is built
//! with `rustman_contract::sender::Offline` and reads the document with
//! `Spec::load_local`: nothing is sent, and no connection is opened.

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Args;
use rustman_contract::conformance::exchange::from_history;
use rustman_contract::conformance::{Checker, RecordedExchange};
use rustman_contract::input::{MAX_INPUT_BYTES, read_text};
use rustman_contract::report::{ConformanceSources, conformance_report};
use rustman_contract::sender::Offline;
use rustman_contract::{Spec, output};
use rustman_core::history::{HistoryStore, Query, parse_age};
use rustman_core::settings::HistorySettings;
use rustman_core::{ConfigError, Workspace};
use rustman_report::{Report, ReportTarget};

use super::{print_diagnostics, print_summary, redactor, workspace};
use crate::commands::{EnvSelection, exec, print_warnings};
use crate::exit::{ASSERTION, CliError, SUCCESS};

/// Where the exchanges come from by default.
const HISTORY: &str = "history";

#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// The OpenAPI document, a local file (URLs are refused: this command
    /// opens no network connection)
    #[arg(long, value_name = "OPENAPI")]
    spec: String,
    /// `history` (the default) or a run report written by
    /// `rustman run --report json=...`
    #[arg(long, value_name = "SOURCE", default_value = HISTORY)]
    from: String,
    /// Only history entries at most this old, as in 30m, 2h or 7d
    #[arg(long, value_name = "AGE", value_parser = parse_age)]
    since: Option<Duration>,
    /// Only history entries with this text in the URL, the request body or
    /// the response body (case-insensitive)
    #[arg(long, value_name = "TEXT")]
    grep: Option<String>,
    /// At most this many history entries, newest first
    #[arg(long, value_name = "N")]
    limit: Option<usize>,
    /// Exit 1 when an exchange matches no documented operation
    #[arg(long)]
    fail_on_undocumented: bool,
    /// Also write the report as json, junit, markdown, html or sarif, to a
    /// file or stdout, as in markdown=out/conformance.md (repeatable)
    #[arg(long = "report", value_name = "FORMAT[=PATH]")]
    report: Vec<ReportTarget>,
    /// Environment whose secret values are redacted from the output
    /// (default: RUSTMAN_ENV, then default_environment)
    #[arg(long, value_name = "NAME")]
    env: Option<String>,
    /// Workspace directory, whose history is read (default: search from the
    /// current directory upwards; outside one, the ad-hoc history)
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
}

impl VerifyArgs {
    fn selection(&self) -> EnvSelection {
        EnvSelection {
            env: self.env.clone(),
            workspace: self.workspace.clone(),
        }
    }

    fn query(&self) -> Query {
        Query {
            since: self.since,
            grep: self.grep.clone(),
            limit: self.limit,
            ..Query::default()
        }
    }
}

pub fn run(args: &VerifyArgs) -> Result<u8, CliError> {
    rustman_report::render::one_stdout(&args.report).map_err(ConfigError::Usage)?;
    let selection = args.selection();
    let ws = workspace(&selection)?;
    let redactor = redactor(&selection, ws.as_ref())?;
    let runtime = exec::runtime()?;
    let spec = runtime
        .block_on(Spec::load_local(&args.spec))
        .inspect_err(|e| print_diagnostics(e.warnings()))?;
    let exchanges = exchanges(args, ws.as_ref())?;
    let result = Checker::new(&spec, &Offline).check(exchanges);
    print_summary(
        &redactor.text(&output::conformance_human(&result)),
        &args.report,
    );
    let sources = ConformanceSources {
        spec: &spec.name,
        exchanges: &args.from,
        fail_on_undocumented: args.fail_on_undocumented,
    };
    let report = conformance_report(&result, &spec, sources, &redactor);
    let failing = report.summary.failing;
    crate::commands::report::write_all(&args.report, &Report::Contract(Box::new(report)))?;
    Ok(if failing > 0 { ASSERTION } else { SUCCESS })
}

/// The exchanges of `--from`: the history `--since`/`--grep`/`--limit`
/// select, or every exchange of a run report.
fn exchanges(args: &VerifyArgs, ws: Option<&Workspace>) -> Result<Vec<RecordedExchange>, CliError> {
    if args.from != HISTORY {
        return run_report(&args.from);
    }
    let store = match ws {
        Some(ws) => HistoryStore::for_workspace(&ws.root, &ws.settings.history),
        None => HistoryStore::global(&HistorySettings::default())?,
    };
    let (exchanges, warnings) = from_history(&store, &args.query())?;
    print_warnings(&warnings);
    Ok(exchanges)
}

fn run_report(path: &str) -> Result<Vec<RecordedExchange>, CliError> {
    let text = read_text(Path::new(path), MAX_INPUT_BYTES).map_err(|source| ConfigError::Io {
        path: PathBuf::from(path),
        source,
    })?;
    let usage = |message: String| CliError::from(ConfigError::Usage(message));
    match Report::parse(&text).map_err(|e| usage(format!("{path}: {e}")))? {
        Report::Run(run) => Ok(RecordedExchange::from_run_report(&run, path)),
        other => Err(usage(format!(
            "{path}: a run report is needed, not a {} report",
            other.kind()
        ))),
    }
}
