//! `rustman contract`: OpenAPI breaking-change detection with the rule
//! catalog of `rustman_contract` (contract-testing "OpenAPI diff command",
//! "Rule catalog is documented and versioned", "Diff exit codes and
//! strictness", "Allowlist with justification", "Git-aware base",
//! "Contract reports"), Pact contracts ([`pact`]) and passive conformance
//! ([`conformance`]). The work happens in `rustman_contract`; these
//! commands load the inputs and pick the output.

mod conformance;
mod pact;

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand, ValueEnum};
use rustman_contract::allowlist::{ALLOWLIST_FILE, Allowlist, Day};
use rustman_contract::git::GitTree;
use rustman_contract::{Spec, Strictness, Verdict, catalog, diff, output, report};
use rustman_core::{ConfigError, Redactor, Workspace};
use rustman_explore::openapi::Diagnostic;
use rustman_explore::{ImportError, ImportOptions};
use rustman_report::{Report, ReportTarget};

use super::{EnvSelection, current_dir, exec, print_warnings};
use crate::exit::{ASSERTION, CliError, SUCCESS};

#[derive(Debug, Args)]
pub struct ContractArgs {
    #[command(subcommand)]
    command: ContractCommand,
}

#[derive(Debug, Subcommand)]
enum ContractCommand {
    /// Compare two OpenAPI documents and classify every difference as
    /// breaking, non-breaking or unknown; exits 1 on a breaking difference
    Diff(DiffArgs),
    /// List the rule catalog the diff classifies differences with
    Rules(RulesArgs),
    /// Generate Pact contracts from a collection, and verify them against a
    /// provider
    Pact(pact::PactArgs),
    /// Check recorded exchanges (history or a run report) against an
    /// OpenAPI document without sending any request
    Verify(conformance::VerifyArgs),
}

/// `--fail-on`: the least severe classification that fails the diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum FailOn {
    Breaking,
    Unknown,
    NonBreaking,
}

#[derive(Debug, Args)]
struct DiffArgs {
    /// The old document (a file or an http(s) URL); with --base, the
    /// document's path, read at the ref and from the working tree
    #[arg(value_name = "OLD")]
    old: String,
    /// The new document (a file or an http(s) URL)
    #[arg(
        value_name = "NEW",
        required_unless_present = "base",
        conflicts_with = "base"
    )]
    new: Option<String>,
    /// Read the old document (and the files its $refs name) from this git
    /// ref, as in origin/main
    #[arg(long, value_name = "REF")]
    base: Option<String>,
    /// Exit 1 on unknown differences too
    #[arg(long)]
    fail_on_unknown: bool,
    /// The least severe classification that exits 1: breaking (default),
    /// unknown, or non-breaking for any difference (frozen APIs)
    #[arg(
        long,
        value_name = "CLASSIFICATION",
        value_enum,
        default_value = "breaking"
    )]
    fail_on: FailOn,
    /// The allowlist of accepted differences (default:
    /// contracts/allowlist.yaml in the workspace, or the current folder)
    #[arg(long, value_name = "FILE")]
    allowlist: Option<PathBuf>,
    /// Also write the report as json, junit, markdown, html or sarif, to a
    /// file or stdout, as in markdown=out/contract.md (repeatable)
    #[arg(long = "report", value_name = "FORMAT[=PATH]")]
    report: Vec<ReportTarget>,
    /// Environment whose secret values are redacted from the output
    /// (default: RUSTMAN_ENV, then default_environment)
    #[arg(long, value_name = "NAME")]
    env: Option<String>,
    /// Workspace directory (default: search from the current directory
    /// upwards; a diff needs none)
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
}

impl DiffArgs {
    fn strictness(&self) -> Strictness {
        Strictness {
            unknown: self.fail_on_unknown || self.fail_on != FailOn::Breaking,
            any: self.fail_on == FailOn::NonBreaking,
        }
    }

    fn selection(&self) -> EnvSelection {
        EnvSelection {
            env: self.env.clone(),
            workspace: self.workspace.clone(),
        }
    }
}

#[derive(Debug, Args)]
struct RulesArgs {
    /// Print a JSON array (each rule with its id, classification,
    /// description, example and the catalog version) instead of a table
    #[arg(long)]
    json: bool,
}

pub fn run(args: &ContractArgs) -> Result<u8, CliError> {
    match &args.command {
        ContractCommand::Diff(args) => run_diff(args),
        ContractCommand::Rules(args) => {
            rules(args);
            Ok(SUCCESS)
        }
        ContractCommand::Pact(args) => pact::run(args),
        ContractCommand::Verify(args) => conformance::run(args),
    }
}

/// Print `text` on stdout, or on stderr when a report goes to stdout (which
/// keeps it machine-readable).
fn print_summary(text: &str, targets: &[ReportTarget]) {
    if targets.iter().any(|t| t.path.is_none()) {
        eprint!("{text}");
    } else {
        print!("{text}");
    }
}

fn run_diff(args: &DiffArgs) -> Result<u8, CliError> {
    rustman_report::render::one_stdout(&args.report).map_err(ConfigError::Usage)?;
    let ws = workspace(&args.selection())?;
    let redactor = redactor(&args.selection(), ws.as_ref())?;
    let allowlist = read_allowlist(args, ws.as_ref())?;
    let (old, new) = load_pair(args)?;
    let result = diff(&old, &new);
    let applied = allowlist.apply(&result.differences, Day::today());
    let verdict = Verdict::new(result, applied, args.strictness());
    let warnings: Vec<String> = verdict
        .warnings
        .iter()
        .map(|w| redactor.text(w).into_owned())
        .collect();
    print_warnings(&warnings);
    print_summary(&redactor.text(&output::human(&verdict)), &args.report);
    let report = report::diff_report(&verdict, &redactor);
    super::report::write_all(&args.report, &Report::Contract(Box::new(report)))?;
    Ok(if verdict.failing() > 0 {
        ASSERTION
    } else {
        SUCCESS
    })
}

/// The old and new documents: two sources, or with `--base` one path read
/// at the ref and from the working tree.
fn load_pair(args: &DiffArgs) -> Result<(Spec, Spec), CliError> {
    let runtime = exec::runtime()?;
    let options = ImportOptions::default();
    let shown =
        |result: Result<Spec, ImportError>| result.inspect_err(|e| print_diagnostics(e.warnings()));
    let old = match &args.base {
        Some(reference) => {
            let tree = GitTree::open(reference, &current_dir()?)?;
            shown(runtime.block_on(tree.load(&args.old, &options)))?
        }
        None => shown(runtime.block_on(Spec::load(&args.old, &options)))?,
    };
    let new_source = args.new.as_deref().unwrap_or(&args.old);
    let new = shown(runtime.block_on(Spec::load(new_source, &options)))?;
    Ok((old, new))
}

/// The workspace, when there is one: a diff runs without.
fn workspace(selection: &EnvSelection) -> Result<Option<Workspace>, ConfigError> {
    match selection.workspace() {
        Ok(ws) => Ok(Some(ws)),
        Err(ConfigError::NoWorkspace) if selection.workspace.is_none() => Ok(None),
        Err(e) => Err(e),
    }
}

/// A redactor for the secret values of the active environment, if any.
fn redactor(selection: &EnvSelection, ws: Option<&Workspace>) -> Result<Redactor, ConfigError> {
    let Some(ws) = ws else {
        return Ok(Redactor::new());
    };
    let Some(env) = selection.environment(ws)? else {
        return Ok(Redactor::new());
    };
    let store = exec::secret_store(&ws.root)?;
    let redactor = exec::secret_redactor(&store, env);
    print_warnings(&store.take_warnings());
    Ok(redactor)
}

/// `--allowlist`, or `contracts/allowlist.yaml` in the workspace (the
/// current folder without one); an absent default file allows nothing.
fn read_allowlist(args: &DiffArgs, ws: Option<&Workspace>) -> Result<Allowlist, CliError> {
    let cwd = current_dir()?;
    let (path, shown) = match &args.allowlist {
        Some(path) => {
            if !path.is_file() {
                let message = format!("{}: no such allowlist file", path.display());
                return Err(ConfigError::Usage(message).into());
            }
            (path.clone(), path.clone())
        }
        None => {
            let root = ws.map_or(cwd.as_path(), |ws| ws.root.as_path());
            (root.join(ALLOWLIST_FILE), shown_allowlist(root, &cwd))
        }
    };
    Allowlist::read(&path, &shown).map_err(|errors| ConfigError::Invalid(errors).into())
}

/// The default allowlist as messages name it: relative to the current
/// folder when the workspace root is in it.
fn shown_allowlist(root: &Path, cwd: &Path) -> PathBuf {
    let canonical = |p: &Path| dunce::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    match canonical(root).strip_prefix(canonical(cwd)) {
        Ok(rel) => rel.join(ALLOWLIST_FILE),
        Err(_) => root.join(ALLOWLIST_FILE),
    }
}

fn print_diagnostics<'a>(diagnostics: impl IntoIterator<Item = &'a Diagnostic>) {
    let lines: Vec<String> = diagnostics.into_iter().map(ToString::to_string).collect();
    print_warnings(&lines);
}

fn rules(args: &RulesArgs) {
    if args.json {
        let json =
            serde_json::to_string_pretty(&catalog::listing()).expect("the catalog serializes");
        println!("{json}");
    } else {
        print!("{}", catalog::table());
    }
}
