//! `rustman run <path>...`: execute the requests under files and folders, in
//! collection order, and write the reports.

use std::io::Write;
use std::path::{Path, PathBuf};

use clap::Args;
use rustman_core::run::{Filter, RunRecord, plan};
use rustman_core::{ConfigError, Workspace};
use rustman_http::runner::{RunContext, Runner};
use rustman_report::Reporter;

use super::exec::{SendFlags, cancel_on_ctrl_c, history_store, runtime, secret_store};
use super::{EnvSelection, current_dir, print_warnings};
use crate::exit::{CliError, run_code};

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Request files or folders to run (default: the current directory)
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,
    #[command(flatten)]
    selection: EnvSelection,
    #[command(flatten)]
    flags: SendFlags,
    /// Skip the remaining requests after the first failed or errored one
    #[arg(long)]
    bail: bool,
    /// Run only matching requests: tag:<name>, or a glob over request ids
    /// such as 'users/*' (repeatable; a request matching any runs)
    #[arg(long, value_name = "FILTER")]
    filter: Vec<String>,
    /// Run the selected requests this many times
    #[arg(long, value_name = "N", default_value_t = 1,
          value_parser = clap::value_parser!(u32).range(1..))]
    iterations: u32,
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
}

pub fn run(args: &RunArgs) -> Result<u8, CliError> {
    one_stdout_reporter(&args.reporter)?;
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
    let store = secret_store(&ws.root)?;
    let mut options = args.flags.options()?;
    options.bail = args.bail;
    options.iterations = args.iterations;
    options.parallel = args.parallel as usize;
    options.history = history_store(args.flags.no_history, Some(&ws), &ws.settings);
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
        runner.run(&plan).await
    });
    if let Err(e) = runner.finish() {
        print_warnings(&[e.to_string()]);
    }
    print_warnings(&store.take_warnings());
    write_reports(&args.reporter, &record)?;
    Ok(run_code(&record.summary))
}

/// The run paths relative to the workspace root. Every path must exist and
/// lie inside the workspace.
fn scopes(ws: &Workspace, cwd: &Path, paths: &[PathBuf]) -> Result<Vec<PathBuf>, ConfigError> {
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
/// can read, so at most one reporter may go there.
fn one_stdout_reporter(reporters: &[Reporter]) -> Result<(), ConfigError> {
    let to_stdout: Vec<String> = reporters
        .iter()
        .filter(|r| r.path.is_none())
        .map(|r| r.format.to_string())
        .collect();
    if to_stdout.len() <= 1 {
        return Ok(());
    }
    Err(ConfigError::Usage(format!(
        "only one reporter can write to stdout, got {}; give the others a path, as in json:out/report.json",
        to_stdout.join(", ")
    )))
}

/// Write every report: to its file, or to stdout (pretty by default).
fn write_reports(reporters: &[Reporter], record: &RunRecord) -> Result<(), ConfigError> {
    let default = ["pretty".parse::<Reporter>().expect("pretty is a reporter")];
    let reporters = if reporters.is_empty() {
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
