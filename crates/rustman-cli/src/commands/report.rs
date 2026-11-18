//! `rustman report`: print the report JSON Schema, render a canonical report
//! in another format, and diff two reports. The work happens in
//! `rustman_report`; these commands read files and pick the output.

use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand, ValueEnum};
use rustman_core::ConfigError;
use rustman_report::exploration::Severity;
use rustman_report::{RenderFormat, RenderOptions, Report, ReportTarget, diff};

use super::current_dir;
use crate::exit::{self, CliError};

#[derive(Debug, Args)]
pub struct ReportArgs {
    #[command(subcommand)]
    command: ReportCommand,
}

#[derive(Debug, Subcommand)]
enum ReportCommand {
    /// Print the JSON Schema of rustman.report/v1
    Schema,
    /// Render a JSON report as json, junit, markdown, html or sarif
    Render(RenderArgs),
    /// Compare two reports: new, fixed and unchanged findings, coverage and
    /// assertion changes
    Diff(DiffArgs),
}

#[derive(Debug, Args)]
struct RenderArgs {
    /// The JSON report (from `rustman run --reporter json` or `rustman
    /// explore run`)
    #[arg(value_name = "REPORT")]
    report: PathBuf,
    /// Output format: json, junit, markdown, html or sarif
    #[arg(long, value_name = "FORMAT", value_parser = parse_format)]
    format: RenderFormat,
    /// Write to this file instead of stdout
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    output: Option<PathBuf>,
    /// JUnit fails operations with findings at or above this severity
    /// (default: the exploration's --fail-on)
    #[arg(long, value_name = "SEVERITY")]
    fail_on: Option<Severity>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum DiffFormat {
    Markdown,
    Json,
}

#[derive(Debug, Args)]
struct DiffArgs {
    /// The earlier JSON report
    #[arg(value_name = "OLD")]
    old: PathBuf,
    /// The later JSON report
    #[arg(value_name = "NEW")]
    new: PathBuf,
    /// Exit 1 when a new finding is at or above this severity
    #[arg(long, value_name = "SEVERITY", default_value = "high")]
    fail_on: Severity,
    /// Output format
    #[arg(long, value_enum, default_value = "markdown")]
    format: DiffFormat,
}

fn parse_format(value: &str) -> Result<RenderFormat, String> {
    value.parse()
}

pub fn run(args: &ReportArgs) -> Result<u8, CliError> {
    match &args.command {
        ReportCommand::Schema => {
            print(&rustman_report::report_schema());
            Ok(exit::SUCCESS)
        }
        ReportCommand::Render(a) => render(a).map(|()| exit::SUCCESS),
        ReportCommand::Diff(a) => diff_command(a),
    }
}

fn render(args: &RenderArgs) -> Result<(), CliError> {
    let report = read(&args.report)?;
    let options = RenderOptions {
        fail_on: args.fail_on,
        ..render_options(&report)?
    };
    let target = ReportTarget {
        format: args.format,
        path: args.output.clone(),
    };
    write_target(&target, &report, &options)
}

fn diff_command(args: &DiffArgs) -> Result<u8, CliError> {
    let (old, new) = (read(&args.old)?, read(&args.new)?);
    let diff = diff(&old, &new).map_err(|e| ConfigError::Usage(e.to_string()))?;
    print(&match args.format {
        DiffFormat::Markdown => diff.to_markdown(),
        DiffFormat::Json => diff.to_json(),
    });
    Ok(if diff.regressions(args.fail_on) > 0 {
        exit::ASSERTION
    } else {
        exit::SUCCESS
    })
}

/// Read a canonical report; an unknown major version is a usage error.
fn read(path: &Path) -> Result<Report, CliError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Report::parse(&text).map_err(|e| ConfigError::Usage(format!("{}: {e}", path.display())).into())
}

/// The options to render `report` from the current directory: SARIF paths
/// relative to it, and the exploration's document for line numbers.
fn render_options(report: &Report) -> Result<RenderOptions, CliError> {
    let cwd = current_dir()?;
    let base = dunce::canonicalize(&cwd).unwrap_or(cwd);
    let document = match report {
        Report::Exploration(r) if !r.spec.contains("://") => {
            std::fs::read_to_string(base.join(&r.spec)).ok()
        }
        _ => None,
    };
    Ok(RenderOptions {
        fail_on: None,
        base: Some(base),
        document,
    })
}

/// Write every `--report` rendering of `report`.
pub fn write_all(targets: &[ReportTarget], report: &Report) -> Result<(), CliError> {
    if targets.is_empty() {
        return Ok(());
    }
    let options = render_options(report)?;
    for target in targets {
        write_target(target, report, &options)?;
    }
    Ok(())
}

/// Write one rendering to its file, or to stdout.
fn write_target(
    target: &ReportTarget,
    report: &Report,
    options: &RenderOptions,
) -> Result<(), CliError> {
    let io = |source| ConfigError::Io {
        path: target
            .path
            .clone()
            .unwrap_or_else(|| PathBuf::from("<stdout>")),
        source,
    };
    if let Some(text) = target.write(report, options).map_err(io)? {
        print(&text);
    }
    Ok(())
}

fn print(text: &str) {
    let mut stdout = std::io::stdout().lock();
    // A closed stdout (`| head`) is not worth failing over.
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}
