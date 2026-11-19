//! `rustman run <path>...`: execute the requests under files and folders, in
//! collection order, and write the reports.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Args, ValueEnum};
use rustman_core::data_rows::DataRows;
use rustman_core::run::{
    DataRun, Filter, Plan, RpcRefusal, RunRecord, collection_data, plan, secret_columns,
};
use rustman_core::{ConfigError, Workspace};
use rustman_graphql::GraphqlRunner;
use rustman_grpc::GrpcRunner;
use rustman_http::runner::{BailScope, RunContext, Runner};
use rustman_jsonrpc::JsonrpcRunner;
use rustman_mcp::client::McpRunner;
use rustman_report::{Report, ReportTarget, Reporter, RunReport};

use super::exec::{
    SendFlags, cancel_on_ctrl_c, history_store, parse_duration, runtime, secret_store,
};
use super::{EnvSelection, current_dir, print_warnings};
use crate::exit::{CliError, REQUEST, SAFETY, run_code};

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Request files or folders to run (default: the current directory)
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,
    #[command(flatten)]
    selection: EnvSelection,
    #[command(flatten)]
    flags: SendFlags,
    /// Skip the remaining requests after the first failed or errored one;
    /// --bail=iteration skips only the rest of the failing iteration
    #[arg(long, value_name = "SCOPE", value_enum, num_args = 0..=1,
          require_equals = true, default_missing_value = "run")]
    bail: Option<Bail>,
    /// Run only matching requests: tag:<name>, or a glob over request ids
    /// such as 'users/*' (repeatable; a request matching any runs)
    #[arg(long, value_name = "FILTER")]
    filter: Vec<String>,
    /// Run the selected requests this many times (default 1)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    iterations: Option<u32>,
    /// Run the selected requests once per row of a CSV or JSON file, with
    /// the row's values as variables (default: the folder's
    /// _collection.yaml data)
    #[arg(long, value_name = "FILE")]
    data: Option<PathBuf>,
    /// Ignore the data file a _collection.yaml names
    #[arg(long, conflicts_with = "data")]
    no_data: bool,
    /// Label each iteration with this column's value in reports (default:
    /// row <n>)
    #[arg(long, value_name = "COLUMN")]
    data_label: Option<String>,
    /// Data columns whose values are secrets and are redacted in every
    /// output, besides those named like password, token, secret or api_key
    #[arg(long, value_name = "A,B", value_delimiter = ',')]
    secret_columns: Vec<String>,
    /// Run up to N folders at once; requests in one folder stay in order
    /// unless its _collection.yaml sets parallel: true
    #[arg(long, value_name = "N", default_value_t = 1,
          value_parser = clap::value_parser!(u32).range(1..))]
    parallel: u32,
    /// Report format, optionally written to a file: pretty, json, junit or
    /// tap, as in json or junit:out/junit.xml (repeatable; default: pretty
    /// on stdout)
    #[arg(long, value_name = "FORMAT[:PATH]")]
    reporter: Vec<Reporter>,
    /// Also render the report as json, junit, markdown, html or sarif,
    /// written to a file or stdout, as in html=out/report.html (repeatable)
    #[arg(long = "report", value_name = "FORMAT[=PATH]")]
    report: Vec<ReportTarget>,
    /// GraphQL and JSON-RPC subscriptions stop after N events (default:
    /// each request's max_events, else 10)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    max_events: Option<u32>,
    /// GraphQL and JSON-RPC subscriptions stop after this long (default:
    /// each request's duration or max_duration_ms, else 30s)
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    duration: Option<Duration>,
    /// Spawn the workspace's stdio MCP servers that are not trusted yet, and
    /// record the approvals
    #[arg(long)]
    trust: bool,
}

pub fn run(args: &RunArgs) -> Result<u8, CliError> {
    one_stdout_reporter(&args.reporter, &args.report)?;
    // Checked before anything else, so the usage error comes first.
    args.flags.snapshot_mode()?;
    let ws = args.selection.workspace()?;
    let env = args.selection.environment(&ws)?;
    let cwd = current_dir()?;
    let scopes = scopes(&ws, &cwd, &args.paths)?;
    let filters: Vec<Filter> = args.filter.iter().map(|f| Filter::parse(f)).collect();
    let plan = plan(&ws, &scopes, &filters);
    if plan.is_empty() {
        return Err(
            ConfigError::Usage("no requests to run (check the paths and --filter)".into()).into(),
        );
    }
    let data = data_run(args, &ws, &cwd, &scopes, &plan)?;
    let store = std::sync::Arc::new(secret_store(&ws.root)?);
    let mut options = args.flags.options()?;
    options.bail = args.bail.is_some();
    options.bail_scope = match args.bail {
        Some(Bail::Iteration) => BailScope::Iteration,
        _ => BailScope::Run,
    };
    options.iterations = args.iterations.unwrap_or(1);
    options.data = data;
    options.parallel = args.parallel as usize;
    options.history = history_store(args.flags.no_history, Some(&ws), &ws.settings);
    let graphql = GraphqlRunner {
        max_events: args.max_events,
        duration: args.duration,
        ..GraphqlRunner::default()
    };
    options.graphql = Some(&graphql);
    let jsonrpc = JsonrpcRunner {
        max_events: args.max_events,
        max_duration: args.duration,
        ..JsonrpcRunner::default()
    };
    options.jsonrpc = Some(&jsonrpc);
    let mcp = McpRunner::new(
        args.trust,
        super::mcp_client::run_connect_options(store.clone()),
    );
    options.mcp = Some(&mcp);
    let grpc = GrpcRunner::default();
    options.grpc = Some(&grpc);
    let cancel = options.cancel.clone();
    let ctx = RunContext {
        root: &ws.root,
        settings: &ws.settings,
        environment: env,
        secrets: &store,
        in_workspace: true,
    };
    let runner = Runner::new(ctx, options)?;
    let record = runtime()?.block_on(async {
        cancel_on_ctrl_c(&cancel);
        let record = runner.run(&plan).await;
        jsonrpc.close().await;
        mcp.shutdown().await;
        record
    });
    if let Err(e) = runner.finish() {
        print_warnings(&[e.to_string()]);
    }
    print_warnings(&store.take_warnings());
    write_reports(&args.reporter, &args.report, &record)?;
    Ok(exit_code(&record))
}

/// The run's exit code; a run whose errors include a call a method guard
/// refused exits 4, like `rustman rpc`.
fn exit_code(record: &RunRecord) -> u8 {
    let code = run_code(&record.summary);
    let guarded = record.requests.iter().any(|r| {
        r.jsonrpc
            .as_ref()
            .is_some_and(|j| j.refusal == Some(RpcRefusal::Guard))
    });
    if code == REQUEST && guarded {
        SAFETY
    } else {
        code
    }
}

/// What `--bail` skips.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Bail {
    // Every remaining request of the run.
    Run,
    // The rest of the failing iteration; the next one runs.
    Iteration,
}

/// The rows of a data-driven run: `--data`, else the data file a
/// `_collection.yaml` of the run names (unless `--no-data`). Every problem
/// is a usage error, found before anything is sent.
fn data_run(
    args: &RunArgs,
    ws: &Workspace,
    cwd: &Path,
    scopes: &[PathBuf],
    plan: &Plan<'_>,
) -> Result<Option<DataRun>, ConfigError> {
    let Some((path, shown)) = data_file(args, ws, cwd, scopes)? else {
        if let Some(column) = &args.data_label {
            return Err(usage(format!(
                "--data-label {column}: there is no data file"
            )));
        }
        return Ok(None);
    };
    if args.iterations.is_some() {
        let message = if args.data.is_some() {
            "--data and --iterations cannot be combined (iterations = row count)".to_owned()
        } else {
            format!("{shown} sets the iterations (one per row); pass --no-data to use --iterations")
        };
        return Err(usage(message));
    }
    let rows = DataRows::read(&path).map_err(|e| usage(format!("{shown}: {e}")))?;
    let secret = secret_columns(plan, &args.secret_columns);
    DataRun::new(rows, secret, args.data_label.clone())
        .map(Some)
        .map_err(usage)
}

/// The data file of the run and how messages name it.
fn data_file(
    args: &RunArgs,
    ws: &Workspace,
    cwd: &Path,
    scopes: &[PathBuf],
) -> Result<Option<(PathBuf, String)>, ConfigError> {
    if args.no_data {
        return Ok(None);
    }
    if let Some(path) = &args.data {
        return Ok(Some((cwd.join(path), format!("--data {}", path.display()))));
    }
    let found = collection_data(ws, scopes).map_err(usage)?;
    Ok(found.map(|d| (d.path, format!("{} data", d.collection_file))))
}

fn usage(message: String) -> ConfigError {
    ConfigError::Usage(message)
}

/// The run paths relative to the workspace root. Every path must exist and
/// lie inside the workspace.
pub(super) fn scopes(
    ws: &Workspace,
    cwd: &Path,
    paths: &[PathBuf],
) -> Result<Vec<PathBuf>, ConfigError> {
    let root = dunce::canonicalize(&ws.root).map_err(|e| ConfigError::Io {
        path: ws.root.clone(),
        source: e,
    })?;
    let default = [PathBuf::from(".")];
    let paths = if paths.is_empty() {
        &default[..]
    } else {
        paths
    };
    paths
        .iter()
        .map(|path| {
            let full = dunce::canonicalize(cwd.join(path)).map_err(|_| {
                ConfigError::Usage(format!("{}: no such file or folder", path.display()))
            })?;
            full.strip_prefix(&root)
                .map(Path::to_path_buf)
                .map_err(|_| {
                    ConfigError::Usage(format!("{} is outside the workspace", path.display()))
                })
        })
        .collect()
}

/// Two reports on stdout would be concatenated into a document no tool
/// can read, so at most one reporter (or `--report`) may go there.
fn one_stdout_reporter(
    reporters: &[Reporter],
    targets: &[ReportTarget],
) -> Result<(), ConfigError> {
    let to_stdout: Vec<String> = reporters
        .iter()
        .filter(|r| r.path.is_none())
        .map(|r| r.format.to_string())
        .chain(
            targets
                .iter()
                .filter(|t| t.path.is_none())
                .map(|t| t.format.to_string()),
        )
        .collect();
    if to_stdout.len() <= 1 {
        return Ok(());
    }
    Err(ConfigError::Usage(format!(
        "only one reporter can write to stdout, got {}; give the others a path, as in json:out/report.json",
        to_stdout.join(", ")
    )))
}

/// Write every report: to its file, or to stdout (pretty by default, unless
/// a `--report` goes there).
fn write_reports(
    reporters: &[Reporter],
    targets: &[ReportTarget],
    record: &RunRecord,
) -> Result<(), CliError> {
    write_targets(targets, record)?;
    let default = ["pretty".parse::<Reporter>().expect("pretty is a reporter")];
    let to_stdout = targets.iter().any(|t| t.path.is_none());
    let reporters = if reporters.is_empty() && !to_stdout {
        &default[..]
    } else {
        reporters
    };
    // Styled text (the pretty report) loses its colors unless they are on.
    let mut stdout = anstream::stdout().lock();
    for reporter in reporters {
        let io = |source| ConfigError::Io {
            path: reporter
                .path
                .clone()
                .unwrap_or_else(|| PathBuf::from("<stdout>")),
            source,
        };
        if let Some(text) = reporter.write(record).map_err(io)? {
            // A closed stdout (`| head`) is not worth failing the run over.
            let _ = stdout.write_all(text.as_bytes());
        }
    }
    let _ = stdout.flush();
    Ok(())
}

/// Render the `--report` targets from the run's canonical report.
fn write_targets(targets: &[ReportTarget], record: &RunRecord) -> Result<(), CliError> {
    if targets.is_empty() {
        return Ok(());
    }
    super::report::write_all(targets, &Report::Run(Box::new(RunReport::new(record))))
}
