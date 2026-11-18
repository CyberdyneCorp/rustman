//! `rustman import`: collections from other tools (Postman, Insomnia,
//! Bruno, HAR, `.http` files), `rustman import curl` for a single curl
//! command, and `rustman import openapi` for a collection generated from
//! an OpenAPI document.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand, ValueEnum};
use rustman_core::format::write::request_to_yaml;
use rustman_core::model::Request;
use rustman_core::secrets::BACKEND_VAR;
use rustman_core::workspace::check_new_request_file;
use rustman_core::{ConfigError, Redactor, SecretBackend, SecretStore};
use rustman_explore::collection::collection_dir;
use rustman_explore::openapi::Diagnostic;
use rustman_explore::store::{Target, generate_for, write_import};
use rustman_explore::sync::{self, SyncPlan};
use rustman_explore::{ImportOptions as OpenApiOptions, Imported, import};
use rustman_http::{CurlError, import_curl, import_curl_words};
use rustman_import::secrets::Existing;
use rustman_import::{
    DEFAULT_MAX_SIZE, Format, HarFilter, ImportError, OnConflict, PlanOptions, SourceOptions,
    apply, load, plan,
};

use super::{EnvSelection, current_dir, exec, print_warnings};
use crate::exit::{self, CliError};

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true, arg_required_else_help = true)]
pub struct ImportArgs {
    #[command(subcommand)]
    command: Option<ImportCommand>,
    /// A source file or folder whose format is detected from its content
    #[arg(value_name = "SRC")]
    src: Option<PathBuf>,
    #[command(flatten)]
    options: ImportOptions,
}

#[derive(Debug, Subcommand)]
enum ImportCommand {
    /// Convert a curl command (an argument, or stdin) into a request file
    Curl(CurlArgs),
    /// Import a source file or folder, detecting its format from the content
    Auto(SourceArgs),
    /// Import a Postman Collection v2.0 or v2.1 export
    Postman(SourceArgs),
    /// Import a Postman environment export into environments/
    #[command(name = "postman-env")]
    PostmanEnv(SourceArgs),
    /// Import an Insomnia v4 (JSON) or v5 (YAML) export
    Insomnia(SourceArgs),
    /// Import a Bruno collection folder (or a single .bru file)
    Bruno(SourceArgs),
    /// Import the requests of a HAR 1.2 capture, grouped by host
    Har(HarArgs),
    /// Import .http / .rest files (a file or a folder) and their environments
    Http(SourceArgs),
    /// Create a collection from an OpenAPI 3.0 or 3.1 document (a file or URL)
    Openapi(OpenApiArgs),
}

#[derive(Debug, Args)]
struct HarArgs {
    #[command(flatten)]
    source: SourceArgs,
    /// Keep only requests to this host (repeatable)
    #[arg(long, value_name = "HOST")]
    host: Vec<String>,
    /// Keep only requests with this method (repeatable)
    #[arg(long, value_name = "METHOD")]
    method: Vec<String>,
    /// Keep only requests whose request or response has this MIME type (repeatable)
    #[arg(long, value_name = "TYPE")]
    content_type: Vec<String>,
    /// Keep images, fonts, stylesheets and scripts (dropped by default)
    #[arg(long)]
    include_static: bool,
}

impl HarArgs {
    fn filter(&self) -> HarFilter {
        HarFilter {
            hosts: self.host.clone(),
            methods: self.method.clone(),
            content_types: self.content_type.clone(),
            include_static: self.include_static,
        }
    }
}

#[derive(Debug, Args)]
struct SourceArgs {
    /// The exported file (or folder, for Bruno and .http sources)
    #[arg(value_name = "SRC")]
    src: PathBuf,
    #[command(flatten)]
    options: ImportOptions,
}

#[derive(Debug, Args)]
struct ImportOptions {
    /// Collection folder to write into, inside the workspace
    /// (default: the slug of the source collection's name)
    #[arg(long, value_name = "DIR")]
    into: Option<PathBuf>,
    /// Convert and print the planned files and secrets without writing anything
    #[arg(long)]
    dry_run: bool,
    /// When a file already exists: keep it, replace it, or write next to it
    #[arg(long, value_name = "POLICY", value_enum, default_value_t)]
    on_conflict: ConflictPolicy,
    /// Also write the report as JSON (rustman.import-report/v1) to FILE
    #[arg(long, value_name = "FILE")]
    report: Option<PathBuf>,
    /// Write the unconverted script statements, with suggestions, as Markdown to FILE
    #[arg(long, value_name = "FILE")]
    migration_report: Option<PathBuf>,
    /// Refuse sources larger than this (bytes, or with K, M or G; default 100M)
    #[arg(long, value_name = "SIZE", value_parser = parse_size)]
    max_size: Option<u64>,
    /// Exit with 1 when a feature was unsupported, dropped or unmapped
    #[arg(long)]
    fail_on_unsupported: bool,
    #[command(flatten)]
    selection: EnvSelection,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum ConflictPolicy {
    #[default]
    Skip,
    Overwrite,
    Rename,
}

impl From<ConflictPolicy> for OnConflict {
    fn from(policy: ConflictPolicy) -> Self {
        match policy {
            ConflictPolicy::Skip => Self::Skip,
            ConflictPolicy::Overwrite => Self::Overwrite,
            ConflictPolicy::Rename => Self::Rename,
        }
    }
}

#[derive(Debug, Args)]
struct OpenApiArgs {
    /// The document: a file path or an http(s) URL, in JSON or YAML
    #[arg(value_name = "SOURCE")]
    source: String,
    /// Write the collection to this folder (default: the API title in
    /// kebab-case, in the workspace root); it must not exist or be empty
    #[arg(long, value_name = "DIR")]
    into: Option<PathBuf>,
    /// Fetch $refs to other http(s) documents instead of refusing them
    #[arg(long)]
    allow_remote_refs: bool,
    /// Update an imported collection: merge the new document, keeping your
    /// edits, and move removed operations to _removed/
    #[arg(long)]
    sync: bool,
    /// With --sync: print the change summary without writing any file
    #[arg(long, requires = "sync")]
    dry_run: bool,
    /// With --sync: print the change summary as JSON
    #[arg(long, requires = "sync")]
    json: bool,
    /// Workspace directory (default: search from the current directory upwards)
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct CurlArgs {
    /// Write the request file here instead of printing it (never overwrites)
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
    /// The curl command: one quoted argument, or its words after `--`
    #[arg(
        value_name = "COMMAND",
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    command: Vec<String>,
}

/// Run the command; `Ok` carries the exit code.
pub fn run(args: &ImportArgs) -> Result<u8, CliError> {
    let Some(command) = &args.command else {
        let src = args
            .src
            .as_ref()
            .ok_or_else(|| ConfigError::Usage("import: missing source".to_owned()))?;
        return source(None, src, &args.options, HarFilter::default());
    };
    let all = HarFilter::default;
    let (format, a, filter) = match command {
        ImportCommand::Curl(a) => return curl(a).map(|()| exit::SUCCESS),
        ImportCommand::Openapi(a) => return openapi(a).map(|()| exit::SUCCESS),
        ImportCommand::Har(h) => (Some(Format::Har), &h.source, h.filter()),
        ImportCommand::Auto(a) => (None, a, all()),
        ImportCommand::Postman(a) => (Some(Format::Postman), a, all()),
        ImportCommand::PostmanEnv(a) => (Some(Format::PostmanEnv), a, all()),
        ImportCommand::Insomnia(a) => (Some(Format::Insomnia), a, all()),
        ImportCommand::Bruno(a) => (Some(Format::Bruno), a, all()),
        ImportCommand::Http(a) => (Some(Format::Http), a, all()),
    };
    source(format, &a.src, &a.options, filter)
}

/// Import the file or folder `src` in `format` (detected when `None`).
fn source(
    format: Option<Format>,
    src: &Path,
    o: &ImportOptions,
    har: HarFilter,
) -> Result<u8, CliError> {
    let ws = o.selection.workspace()?;
    // One import at a time per workspace, from reading it to writing it;
    // the workspace is read again once the lock is held.
    let _lock = if o.dry_run {
        None
    } else {
        Some(rustman_import::lock_workspace(&ws.root)?)
    };
    let ws = if o.dry_run {
        ws
    } else {
        o.selection.workspace()?
    };
    let env = o.selection.environment(&ws)?;
    let shown = src.display().to_string();
    let source_options = SourceOptions {
        max_size: o.max_size.unwrap_or(DEFAULT_MAX_SIZE),
        har,
    };
    let (format, collection) = load(format, src, &shown, &source_options)?;
    let destination = o
        .into
        .as_deref()
        .map(|into| destination(&ws.root, into))
        .transpose()?;
    let backend = SecretBackend::from_env_value(std::env::var(BACKEND_VAR).ok().as_deref())
        .map_err(ConfigError::from)?;
    let store = SecretStore::open(&ws.root, backend);
    let lookup = |env: &str, name: &str| store.get(env, name).ok().flatten();
    let options = PlanOptions {
        root: &ws.root,
        source: shown,
        format,
        destination,
        on_conflict: o.on_conflict.into(),
        environment: env.map(|e| e.name.clone()),
        existing: Existing {
            environments: &ws.environments,
            lookup: &lookup,
        },
        dry_run: o.dry_run,
    };
    let plan = plan(collection, &options)?;
    if !o.dry_run {
        let save = |batch: &[(&str, &_)]| store.set_many(batch);
        let applied = apply(&plan, &ws.root, &save);
        print_warnings(&store.take_warnings());
        applied?;
    }
    // Reports name secrets only; redacting them as well guards every
    // output. Values that are ordinary words or numbers (`password`,
    // `123456`) are left out: the report shows them only as data (URLs,
    // names), which redacting would corrupt.
    let redactor = plan
        .secrets
        .iter()
        .filter(|m| rustman_import::secrets::is_secret_like(m.secret.expose()))
        .fold(Redactor::new(), |r, m| r.with(&m.secret));
    print!("{}", redactor.text(&plan.report.summary_text()));
    if let Some(path) = &o.report {
        write_report(path, &redactor.text(&plan.report.to_json()))?;
    }
    if let Some(path) = &o.migration_report {
        let markdown = rustman_import::migration::markdown(&plan.report);
        write_report(path, &redactor.text(&markdown))?;
    }
    let strict_failure = o.fail_on_unsupported && plan.report.has_losses();
    Ok(if strict_failure {
        exit::ASSERTION
    } else {
        exit::SUCCESS
    })
}

/// `into`, relative to the workspace root with `/` separators; it must be
/// inside the workspace.
fn destination(root: &Path, into: &Path) -> Result<String, CliError> {
    let io = |source| ConfigError::Io {
        path: root.to_path_buf(),
        source,
    };
    let root = dunce::canonicalize(root).map_err(io)?;
    let cwd = dunce::canonicalize(current_dir()?).map_err(io)?;
    let outside = || CliError::from(ImportError::OutsideWorkspace);
    let raw = into.to_str().ok_or_else(outside)?;
    let joined = rustman_core::paths::contain(&root, &cwd, raw).map_err(|_| outside())?;
    let rel = joined.strip_prefix(&root).map_err(|_| outside())?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Ok(parts.join("/"))
}

fn write_report(path: &Path, text: &str) -> Result<(), CliError> {
    let io = |source| ImportError::io(path, source);
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    std::fs::write(path, text).map_err(io)?;
    Ok(())
}

/// A size in bytes: `1048576`, `512K`, `100M`, `2G` (or `KiB`, `MiB`, `GiB`).
fn parse_size(text: &str) -> Result<u64, String> {
    let trimmed = text.trim();
    let digits_end = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (digits, unit) = trimmed.split_at(digits_end);
    let shift = match unit.trim().to_ascii_uppercase().as_str() {
        "" | "B" => 0,
        "K" | "KB" | "KIB" => 10,
        "M" | "MB" | "MIB" => 20,
        "G" | "GB" | "GIB" => 30,
        _ => return Err(format!("invalid size '{text}' (e.g. 1048576, 512K, 100M)")),
    };
    digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(1 << shift))
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("invalid size '{text}' (e.g. 1048576, 512K, 100M)"))
}

fn openapi(args: &OpenApiArgs) -> Result<(), CliError> {
    let root = rustman_core::discovery::find_root(&current_dir()?, args.workspace.as_deref())?;
    let options = OpenApiOptions {
        allow_remote_refs: args.allow_remote_refs,
        ..OpenApiOptions::default()
    };
    let imported = exec::runtime()?
        .block_on(import(&args.source, &options))
        .inspect_err(|e| print_diagnostics(e.warnings()))?;
    print_diagnostics(imported.warnings.iter());
    let dir = match &args.into {
        Some(dir) => current_dir()?.join(dir),
        None => root.join(collection_dir(&imported.model)),
    };
    let target = Target::new(root, dir);
    if args.sync && is_non_empty_dir(&target.dir) {
        return sync_collection(&imported, &target, args);
    }
    let generated = generate_for(&imported, &target);
    print_diagnostics(generated.warnings.iter());
    let outcome = write_import(&imported, &generated, &target)?;
    let cwd = current_dir()?;
    let shown = match &args.into {
        Some(dir) => dir.clone(),
        None => target
            .dir
            .strip_prefix(&cwd)
            .unwrap_or(&target.dir)
            .to_path_buf(),
    };
    println!(
        "imported {} operation{} into {}",
        generated.requests,
        if generated.requests == 1 { "" } else { "s" },
        shown.display()
    );
    for name in &outcome.created {
        println!("created environment '{name}'");
    }
    let kept: Vec<String> = outcome
        .kept
        .iter()
        .map(|name| {
            format!(
                "environment '{name}' already exists and was not changed (use --sync to update it)"
            )
        })
        .collect();
    print_warnings(&kept);
    Ok(())
}

fn is_non_empty_dir(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some())
}

/// `--sync`: merge `imported` into the collection at `target`.
fn sync_collection(
    imported: &Imported,
    target: &Target,
    args: &OpenApiArgs,
) -> Result<(), CliError> {
    let mut plan: SyncPlan = sync::plan(imported, target)?;
    print_diagnostics(plan.generated.warnings.iter());
    print_warnings(&plan.warnings);
    plan.summary.dry_run = args.dry_run;
    if !args.dry_run {
        sync::apply(&plan)?;
    }
    if args.json {
        let json = serde_json::to_string_pretty(&plan.summary).unwrap_or_default();
        println!("{json}");
    } else {
        print!("{}", plan.summary);
    }
    Ok(())
}

fn print_diagnostics<'a>(diagnostics: impl IntoIterator<Item = &'a Diagnostic>) {
    let lines: Vec<String> = diagnostics.into_iter().map(ToString::to_string).collect();
    print_warnings(&lines);
}

fn curl(args: &CurlArgs) -> Result<(), CliError> {
    let imported = match args.command.as_slice() {
        [] => import_curl(&read_stdin()?)?,
        [command] => import_curl(command)?,
        words => import_curl_words(words)?,
    };
    print_warnings(&imported.warnings);
    let request = Request::Http(Box::new(imported.request));
    let yaml = request_to_yaml(&request);
    match &args.out {
        Some(path) => {
            check_new_request_file(path, &request)?;
            write_new(path, &yaml)?;
            println!("wrote {}", path.display());
        }
        None => print!("{yaml}"),
    }
    Ok(())
}

fn read_stdin() -> Result<String, CurlError> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|e| CurlError(format!("cannot read the command from stdin: {e}")))?;
    Ok(text)
}

/// Create `path` with `text`, refusing to replace an existing file.
fn write_new(path: &Path, text: &str) -> Result<(), ConfigError> {
    let io = |source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io)?;
    file.write_all(text.as_bytes()).map_err(io)
}

#[cfg(test)]
mod tests {
    use super::parse_size;

    #[test]
    fn sizes_take_binary_suffixes() {
        assert_eq!(parse_size("1048576"), Ok(1 << 20));
        assert_eq!(parse_size("512K"), Ok(512 << 10));
        assert_eq!(parse_size("100MiB"), Ok(100 << 20));
        assert_eq!(parse_size("2g"), Ok(2 << 30));
        assert!(parse_size("0").is_err());
        assert!(parse_size("ten").is_err());
        assert!(parse_size("5T").is_err());
    }
}
