//! `rustman contract pact generate` and `rustman contract pact verify`
//! (contract-testing "Pact generation from a collection", "Pact
//! specification versions", "Provider verification", "Provider states",
//! "Verification passes the safety policy").

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand, ValueEnum};
use rustman_contract::input::{MAX_INPUT_BYTES, read_text};
use rustman_contract::pact::{self, Generator, Pact, PactVersion, Verifier};
use rustman_contract::sender::{GovernedSenders, Targets};
use rustman_contract::{output, report};
use rustman_core::history::HistoryStore;
use rustman_core::safety::{EnvironmentInfo, Permission};
use rustman_core::{ConfigError, Redactor, Workspace};
use rustman_report::{Report, ReportTarget};

use super::{print_summary, redactor, workspace};
use crate::commands::{EnvSelection, current_dir, exec, print_warnings};
use crate::exit::{ASSERTION, CliError, SAFETY, SUCCESS};

#[derive(Debug, Args)]
pub struct PactArgs {
    #[command(subcommand)]
    command: PactCommand,
}

#[derive(Debug, Subcommand)]
enum PactCommand {
    /// Write a Pact file from a collection's http requests and their
    /// assertions
    Generate(GenerateArgs),
    /// Replay Pact interactions against a provider and compare its responses
    Verify(VerifyArgs),
}

/// `--pact-version`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum SpecVersion {
    #[value(name = "3")]
    V3,
    #[value(name = "4")]
    V4,
}

#[derive(Debug, Args)]
struct GenerateArgs {
    /// Collection folders or request files (default: the whole workspace)
    #[arg(value_name = "COLLECTION")]
    paths: Vec<PathBuf>,
    /// The consumer's name
    #[arg(long, value_name = "NAME")]
    consumer: String,
    /// The provider's name
    #[arg(long, value_name = "NAME")]
    provider: String,
    /// The Pact Specification version to write
    #[arg(long, value_name = "VERSION", value_enum, default_value = "4")]
    pact_version: SpecVersion,
    /// The file to write (default:
    /// contracts/pacts/<consumer>-<provider>.json in the workspace)
    #[arg(long, value_name = "FILE")]
    output: Option<PathBuf>,
    #[command(flatten)]
    selection: EnvSelection,
}

#[derive(Debug, Args)]
struct VerifyArgs {
    /// The Pact files to verify
    #[arg(value_name = "PACT_FILE", required = true)]
    pacts: Vec<PathBuf>,
    /// The provider's base URL, as in http://localhost:8080
    #[arg(long, value_name = "URL")]
    provider_url: String,
    /// The provider's state-change endpoint; each provider state is POSTed
    /// to it before (setup) and after (teardown) its interaction
    #[arg(long, value_name = "URL")]
    provider_states_url: Option<String>,
    /// Replay POST, PUT and PATCH interactions
    #[arg(long)]
    allow_mutations: bool,
    /// Replay DELETE interactions
    #[arg(long)]
    allow_destructive: bool,
    /// Confirm a production provider by repeating its exact host
    #[arg(long, value_name = "HOST")]
    production: Option<String>,
    /// Also write the report as json, junit, markdown, html or sarif, to a
    /// file or stdout, as in junit=out/pact.xml (repeatable)
    #[arg(long = "report", value_name = "FORMAT[=PATH]")]
    report: Vec<ReportTarget>,
    /// Environment whose secret values are redacted from the output
    /// (default: RUSTMAN_ENV, then default_environment)
    #[arg(long, value_name = "NAME")]
    env: Option<String>,
    /// Workspace directory, for [safety] and redaction (default: search
    /// from the current directory upwards; verification needs none)
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
}

pub fn run(args: &PactArgs) -> Result<u8, CliError> {
    match &args.command {
        PactCommand::Generate(args) => generate(args),
        PactCommand::Verify(args) => verify(args),
    }
}

fn generate(args: &GenerateArgs) -> Result<u8, CliError> {
    for name in [&args.consumer, &args.provider] {
        check_name(name)?;
    }
    let ws = args.selection.workspace()?;
    let env = args.selection.environment(&ws)?;
    let store = exec::secret_store(&ws.root)?;
    let redactor = env.map_or_else(Redactor::new, |env| exec::secret_redactor(&store, env));
    let cwd = current_dir()?;
    let scopes = crate::commands::run::scopes(&ws, &cwd, &args.paths)?;
    let plan = rustman_core::run::plan(&ws, &scopes, &[]);
    let recorded = recorded_statuses(&ws);
    let generator = Generator {
        settings: &ws.settings,
        environment: env,
        secrets: &store,
        redactor: &redactor,
        version: match args.pact_version {
            SpecVersion::V3 => PactVersion::V3,
            SpecVersion::V4 => PactVersion::V4,
        },
        recorded_status: &|file: &str| recorded.get(file).copied(),
    };
    let generated = generator
        .generate(&args.consumer, &args.provider, &plan.requests)
        .map_err(|e| ConfigError::Usage(e.to_string()))?;
    print_warnings(&store.take_warnings());
    print_warnings(&generated.warnings);
    let (path, shown) = match &args.output {
        Some(path) => (cwd.join(path), path.display().to_string()),
        None => {
            let relative = pact::default_file(&args.consumer, &args.provider);
            (ws.root.join(&relative), relative)
        }
    };
    write_pact(&path, &generated.pact, &redactor)?;
    let count = generated.pact.interactions.len();
    let version = generated.pact.version.spec_version();
    println!("wrote {shown} ({count} interactions, Pact {version})");
    Ok(SUCCESS)
}

/// Names become file names: letters, digits, `.`, `_` and `-` only.
fn check_name(name: &str) -> Result<(), ConfigError> {
    let fine = !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if fine {
        return Ok(());
    }
    Err(ConfigError::Usage(format!(
        "invalid pacticipant name '{name}' (use letters, digits, '.', '_' and '-')"
    )))
}

/// The status last recorded for each request file, from the workspace
/// history (for `status: 2xx` in a v3 Pact).
fn recorded_statuses(ws: &Workspace) -> HashMap<String, u16> {
    let store = HistoryStore::for_workspace(&ws.root, &ws.settings.history);
    let mut out = HashMap::new();
    for entry in store.list().map(|l| l.entries).unwrap_or_default() {
        if let Some(status) = entry.status {
            out.entry(entry.request).or_insert(status);
        }
    }
    out
}

fn write_pact(path: &Path, pact: &Pact, redactor: &rustman_core::Redactor) -> Result<(), CliError> {
    let mut json = pact.to_json();
    redactor.json(&mut json);
    let text = serde_json::to_string_pretty(&json).expect("a Pact serializes") + "\n";
    let io = |source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    std::fs::write(path, text).map_err(io)?;
    Ok(())
}

fn verify(args: &VerifyArgs) -> Result<u8, CliError> {
    rustman_report::render::one_stdout(&args.report).map_err(ConfigError::Usage)?;
    let selection = args.selection();
    let ws = workspace(&selection)?;
    let redactor = redactor(&selection, ws.as_ref())?;
    let pacts = read_pacts(&args.pacts)?;
    let settings = ws
        .as_ref()
        .map(|w| w.settings.safety.clone())
        .unwrap_or_default();
    let environment = match &ws {
        Some(ws) => selection.environment(ws)?,
        None => None,
    };
    let sender = GovernedSenders::new(
        &settings,
        Targets {
            provider_url: &args.provider_url,
            states_url: args.provider_states_url.as_deref(),
            permission: Permission::from_flags(args.allow_mutations, args.allow_destructive),
            production: args.production.as_deref(),
            environment: environment.map(EnvironmentInfo::from).unwrap_or_default(),
        },
    )?;
    let verifier = Verifier {
        sender: &sender,
        provider_url: &args.provider_url,
        states_url: args.provider_states_url.as_deref(),
    };
    let runtime = exec::runtime()?;
    let verification = runtime
        .block_on(verifier.verify(&pacts))
        .map_err(ConfigError::Usage)?;
    let warnings: Vec<String> = verification
        .warnings
        .iter()
        .map(|w| redactor.text(w).into_owned())
        .collect();
    print_warnings(&warnings);
    print_summary(
        &redactor.text(&output::pact_human(&verification)),
        &args.report,
    );
    let report = report::pact_report(&verification, &redactor);
    crate::commands::report::write_all(&args.report, &Report::Contract(Box::new(report)))?;
    Ok(if verification.count("blocked") > 0 {
        SAFETY
    } else if verification.count("failed") > 0 {
        ASSERTION
    } else {
        SUCCESS
    })
}

/// Every Pact file, read before anything is sent.
fn read_pacts(paths: &[PathBuf]) -> Result<Vec<(String, Pact)>, CliError> {
    let mut out = Vec::new();
    for path in paths {
        let shown = path.display().to_string();
        let text = read_text(path, MAX_INPUT_BYTES).map_err(|source| ConfigError::Io {
            path: path.clone(),
            source,
        })?;
        let read = pact::read(&shown, &text).map_err(ConfigError::Usage)?;
        print_warnings(&read.warnings);
        out.push((shown, read.pact));
    }
    Ok(out)
}
