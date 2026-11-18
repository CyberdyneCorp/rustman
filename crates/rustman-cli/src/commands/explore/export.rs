//! `rustman explore export <id> --findings <ids|all> --into <dir>`: one
//! regression request file per finding, from the exploration's report.

use std::path::PathBuf;

use clap::Args;
use rustman_core::ConfigError;
use rustman_explore::explore::export::{export, select};
use rustman_explore::explore::report::ExplorationReport;
use rustman_explore::explore::session::Session;

use super::RootArgs;
use crate::exit::CliError;

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// The exploration id, exp-<date>-<hex>
    id: String,
    /// Finding ids from the report (f001,f002) or `all`
    #[arg(long, value_name = "IDS", value_delimiter = ',', required = true)]
    findings: Vec<String>,
    /// The folder to write the request files into (created if needed)
    #[arg(long, value_name = "DIR")]
    into: PathBuf,
    #[command(flatten)]
    root: RootArgs,
}

pub fn export_command(args: &ExportArgs) -> Result<(), CliError> {
    let root = args.root.root()?;
    let session = Session::open(&root, &args.id)?;
    let path = session.report_path();
    let text = std::fs::read_to_string(&path).map_err(|_| {
        ConfigError::Usage(format!(
            "exploration {} has no report yet (finish it with `rustman explore resume {}`)",
            args.id, args.id
        ))
    })?;
    let report: ExplorationReport = serde_json::from_str(&text).map_err(|e| {
        ConfigError::Usage(format!("{} is not a valid report: {e}", path.display()))
    })?;
    let selected = select(&report, &args.findings)?;
    let written = export(&selected, &args.into)?;
    println!(
        "wrote {} request files into {}",
        written.len(),
        args.into.display()
    );
    for file in &written {
        println!("  {}", file.display());
    }
    Ok(())
}
