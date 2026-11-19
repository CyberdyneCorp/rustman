//! `rustman export curl <request>` and `rustman export snippet --lang
//! <lang> <request>`: print a request as a curl command or as code, with
//! secrets redacted unless `--show-secrets`; `rustman export postman|http
//! <path>`: a collection folder or request file as a Postman v2.1
//! collection or a `.http` file, with secrets as `{{name}}` placeholders.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use rustman_core::secrets::BACKEND_VAR;
use rustman_core::vars::{Resolver, resolve_request};
use rustman_core::{ConfigError, SecretBackend, SecretStore};
use rustman_core::{Environment, Redactor, Workspace};
use rustman_http::snippet::{RequestSnippet, request_snippet};
use rustman_http::{Lang, export_curl};
use rustman_import::ImportError;
use rustman_import::export::{Target, export};

use super::{EnvSelection, current_dir, print_warnings};
use crate::exit::CliError;

#[derive(Debug, Args)]
pub struct ExportArgs {
    #[command(subcommand)]
    command: ExportCommand,
}

#[derive(Debug, Subcommand)]
enum ExportCommand {
    /// Print a request as a single curl command
    Curl(CurlArgs),
    /// Write a collection folder or request file as a Postman Collection v2.1
    Postman(FileArgs),
    /// Write a collection folder or request file as a .http file
    Http(FileArgs),
    /// Print a request as code for another language or tool
    Snippet(SnippetArgs),
}

#[derive(Debug, Args)]
struct FileArgs {
    /// A collection folder or request file of the workspace
    #[arg(value_name = "PATH")]
    path: PathBuf,
    /// Write to FILE instead of standard output
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,
    #[command(flatten)]
    selection: EnvSelection,
}

#[derive(Debug, Args)]
struct CurlArgs {
    /// The request: a request file, its id (path without .rman.yaml) or its name
    request: String,
    /// Print secret values instead of «redacted:<name>» placeholders
    #[arg(long)]
    show_secrets: bool,
    #[command(flatten)]
    selection: EnvSelection,
}

#[derive(Debug, Args)]
struct SnippetArgs {
    /// The request: a request file, its id (path without .rman.yaml) or its name
    #[arg(required_unless_present = "list")]
    request: Option<String>,
    /// The language (see --list); `fetch`, `python` and `rust` are aliases
    #[arg(long, value_name = "LANG", required_unless_present = "list")]
    lang: Option<String>,
    /// List the supported languages and exit
    #[arg(long, exclusive = true)]
    list: bool,
    /// Print secret values instead of «redacted:<name>» placeholders
    #[arg(long, conflicts_with = "placeholders")]
    show_secrets: bool,
    /// Keep variables unresolved: {{name}} in shell tools, constants in code
    #[arg(long)]
    placeholders: bool,
    #[command(flatten)]
    selection: EnvSelection,
}

pub fn run(args: &ExportArgs) -> Result<(), CliError> {
    match &args.command {
        ExportCommand::Curl(a) => curl(a),
        ExportCommand::Postman(a) => file(a, Target::Postman),
        ExportCommand::Http(a) => file(a, Target::Http),
        ExportCommand::Snippet(a) => export_snippet(a),
    }
}

fn file(args: &FileArgs, target: Target) -> Result<(), CliError> {
    let ws = args.selection.workspace()?;
    let env = args.selection.environment(&ws)?;
    let rel = relative(&ws.root, &args.path)?;
    let exported = export(&ws, &rel, target)?;
    // Request files hold placeholders only; redacting the active
    // environment's secret values as well guards every output.
    let redactor = redactor(&ws, env)?;
    let warnings: Vec<String> = exported
        .warnings
        .iter()
        .map(|w| redactor.text(w).into_owned())
        .collect();
    print_warnings(&warnings);
    let text = redactor.text(&exported.text);
    match &args.output {
        Some(path) => {
            write_output(path, &text)?;
            eprintln!("wrote {}", path.display());
        }
        None => print!("{text}"),
    }
    Ok(())
}

fn write_output(path: &Path, text: &str) -> Result<(), ImportError> {
    let failed = |e: std::io::Error| ImportError::Export(format!("{}: {e}", path.display()));
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(failed)?;
    }
    std::fs::write(path, text).map_err(failed)
}

/// `path` relative to the workspace root; it must be inside it.
fn relative(root: &Path, path: &Path) -> Result<PathBuf, CliError> {
    let shown = path.display();
    let missing = |_| ImportError::Export(format!("{shown}: no such file or folder"));
    let full = dunce::canonicalize(current_dir()?.join(path)).map_err(missing)?;
    let root = dunce::canonicalize(root)
        .map_err(|e| ImportError::Export(format!("{}: {e}", root.display())))?;
    let rel = full
        .strip_prefix(&root)
        .map_err(|_| ImportError::Export(format!("{shown} is outside the workspace")))?;
    Ok(rel.to_path_buf())
}

/// A redactor for the secret values of `env`.
fn redactor(ws: &Workspace, env: Option<&Environment>) -> Result<Redactor, ConfigError> {
    let Some(env) = env else {
        return Ok(Redactor::new());
    };
    let store = super::exec::secret_store(&ws.root)?;
    let redactor = super::exec::secret_redactor(&store, env);
    print_warnings(&store.take_warnings());
    Ok(redactor)
}

fn secret_store(ws: &Workspace) -> Result<SecretStore, CliError> {
    let backend = SecretBackend::from_env_value(std::env::var(BACKEND_VAR).ok().as_deref())
        .map_err(ConfigError::from)?;
    Ok(SecretStore::open(&ws.root, backend))
}

fn curl(args: &CurlArgs) -> Result<(), CliError> {
    let ws = args.selection.workspace()?;
    let env = args.selection.environment(&ws)?;
    let target = ws.find_request(&args.request, &current_dir()?)?;
    let store = secret_store(&ws)?;
    let resolver = Resolver::for_request(&ws.settings, &target, env, &store);
    let resolved = resolve_request(&target, &resolver);
    print_warnings(&store.take_warnings());
    let exported = export_curl(&resolved?, args.show_secrets);
    print_warnings(&exported.warnings);
    println!("{}", exported.command);
    Ok(())
}

fn export_snippet(args: &SnippetArgs) -> Result<(), CliError> {
    if args.list {
        for lang in Lang::ALL {
            println!("{lang}");
        }
        return Ok(());
    }
    let (Some(request), Some(lang)) = (&args.request, &args.lang) else {
        unreachable!("clap requires both without --list");
    };
    let lang = Lang::parse(lang)?;
    let ws = args.selection.workspace()?;
    let env = args.selection.environment(&ws)?;
    let target = ws.find_request(request, &current_dir()?)?;
    let store = secret_store(&ws)?;
    let how = RequestSnippet {
        settings: &ws.settings,
        environment: env,
        secrets: &store,
        lang,
        show_secrets: args.show_secrets,
        placeholders: args.placeholders,
    };
    let snippet = request_snippet(target, &how)?;
    print_warnings(&snippet.warnings);
    println!("{}", snippet.code);
    Ok(())
}
