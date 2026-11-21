//! `rustman docs build|serve`: static API documentation generated from the
//! workspace (api-docs-generation). The work is done by `rustman-docs`;
//! this module reads flags, opens the secret store and prints a summary.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Args, Subcommand, ValueEnum};
use rustman_core::discovery::find_root;
use rustman_docs::serve::{BuildOutcome, Preview, PreviewSource};
use rustman_docs::{BuildOptions, ExampleSource, Format, build, load_workspace, workspace_secrets};
use tokio_util::sync::CancellationToken;

use super::exec::{cancel_on_ctrl_c, runtime, secret_store};
use super::{current_dir, print_warnings};
use crate::exit::{CliError, SUCCESS};

#[derive(Debug, Args)]
#[command(subcommand_required = true, arg_required_else_help = true)]
pub struct DocsArgs {
    #[command(subcommand)]
    command: DocsCommand,
    /// Workspace directory (default: search from the current directory upwards)
    #[arg(long, value_name = "DIR", global = true)]
    workspace: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum DocsCommand {
    /// Generate a static, offline documentation site from the workspace
    Build(BuildArgs),
    /// Preview the documentation on 127.0.0.1 with live reload
    Serve(ServeArgs),
}

#[derive(Debug, Args)]
struct BuildArgs {
    /// Output folder; only files a previous build wrote are replaced
    #[arg(long, value_name = "DIR", default_value = "site")]
    out: PathBuf,
    /// Output format
    #[arg(long, value_enum, default_value_t = FormatArg::Html)]
    format: FormatArg,
    /// Environment whose [docs].publish_variables values are shown (default: none)
    #[arg(long, value_name = "NAME")]
    env: Option<String>,
    /// Where opted-in examples come from
    #[arg(long, value_enum, default_value_t = ExamplesArg::History)]
    examples: ExamplesArg,
    /// Write into a non-empty folder that rustman docs did not generate
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
struct ServeArgs {
    /// Port on 127.0.0.1
    #[arg(long, default_value_t = 4000)]
    port: u16,
    /// Environment whose [docs].publish_variables values are shown (default: none)
    #[arg(long, value_name = "NAME")]
    env: Option<String>,
    /// Where opted-in examples come from
    #[arg(long, value_enum, default_value_t = ExamplesArg::History)]
    examples: ExamplesArg,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum FormatArg {
    Html,
    Markdown,
    Both,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ExamplesArg {
    History,
    Snapshot,
}

impl From<ExamplesArg> for ExampleSource {
    fn from(arg: ExamplesArg) -> Self {
        match arg {
            ExamplesArg::History => Self::History,
            ExamplesArg::Snapshot => Self::Snapshot,
        }
    }
}

pub fn run(args: &DocsArgs) -> Result<u8, CliError> {
    match &args.command {
        DocsCommand::Build(build_args) => run_build(args, build_args),
        DocsCommand::Serve(serve) => run_serve(args, serve),
    }
}

/// Build into a temporary folder, serve it on loopback and rebuild on
/// every workspace change until Ctrl+C.
fn run_serve(args: &DocsArgs, serve: &ServeArgs) -> Result<u8, CliError> {
    let root = find_root(&current_dir()?, args.workspace.as_deref())?;
    let source = PreviewSource::new(
        root.clone(),
        serve.env.clone(),
        serve.examples.into(),
        Box::new(|root: &Path| secret_store(root)),
    );
    runtime()?.block_on(async {
        let preview = Preview::bind(serve.port, Arc::new(source), Box::new(report)).await?;
        preview.rebuild().await;
        let _watcher = preview.watch(&root)?;
        let stop = CancellationToken::new();
        cancel_on_ctrl_c(&stop);
        println!("docs: serving on http://{}", preview.local_addr());
        preview.serve(stop).await;
        Ok::<_, CliError>(SUCCESS)
    })
}

/// Print a preview build's warnings and result on stderr.
fn report(outcome: &BuildOutcome) {
    print_warnings(&outcome.warnings);
    match &outcome.result {
        Ok(built) => eprintln!("docs: built {} files", built.files.len()),
        Err(message) => eprintln!("error: {message}"),
    }
}

fn run_build(args: &DocsArgs, build_args: &BuildArgs) -> Result<u8, CliError> {
    let ws = load_workspace(&current_dir()?, args.workspace.as_deref())?;
    print_warnings(&ws.warnings);
    let store = secret_store(&ws.root)?;
    let (secrets, warnings) = workspace_secrets(&ws, &store);
    print_warnings(&warnings);
    let mut options = BuildOptions::new(current_dir()?.join(&build_args.out));
    options.shown_out = shown(&build_args.out);
    options.format = match build_args.format {
        FormatArg::Html => Format::Html,
        FormatArg::Markdown => Format::Markdown,
        FormatArg::Both => Format::Both,
    };
    options.env = build_args.env.clone();
    options.examples = build_args.examples.into();
    options.force = build_args.force;
    let report = build(&ws, &secrets, &options)?;
    print_warnings(&report.warnings);
    for id in &report.published_examples {
        eprintln!("published example: {id}");
    }
    println!(
        "wrote {} files to {}",
        report.files.len(),
        options.shown_out
    );
    Ok(SUCCESS)
}

/// `--out` as messages show it, with a final `/`.
fn shown(out: &std::path::Path) -> String {
    let mut text = out.to_string_lossy().replace('\\', "/");
    if !text.ends_with('/') {
        text.push('/');
    }
    text
}
