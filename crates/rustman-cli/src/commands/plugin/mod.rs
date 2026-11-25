//! `rustman plugin add|list|remove|verify|api|test|new` (plugins spec,
//! "Plugin CLI", "Plugin SDK and template", "Host test harness"). The work
//! is done by `rustman-plugin`; this module parses flags and prints. No
//! command here trusts or grants anything: grants are edited in
//! `rustman.toml`, and trust is given in the TUI or with `--trust-plugins`.

mod author;

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use rustman_core::ConfigError;
use rustman_core::trust::TrustKey;
use rustman_plugin::LoadError;
use rustman_plugin::manage::{self, Listed};

use crate::commands::{current_dir, print_warnings};
use crate::exit::{CliError, SUCCESS};

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  rustman plugin add ./plugins/hmac
  rustman plugin add https://plugins.example.com/hmac-0.2.0.wasm --name hmac
  rustman plugin list
  rustman plugin verify
  rustman plugin new my-auth --kind auth
  rustman plugin test ./my-auth")]
pub struct PluginArgs {
    #[command(subcommand)]
    command: PluginCommand,
}

#[derive(Debug, Subcommand)]
enum PluginCommand {
    /// Fetch a plugin from a workspace path or an https URL, print its
    /// manifest, pin its sha256 and declare it with no grants
    Add(AddArgs),
    /// List the workspace's plugins: version, API, grants and trust state
    List(WorkspaceFlag),
    /// Remove a plugin from rustman.toml and rustman.lock
    Remove(RemoveArgs),
    /// Re-hash every plugin against its pin and update rustman.lock;
    /// exits 2 on a mismatch
    Verify(VerifyArgs),
    /// List the plugin API versions this rustman supports
    Api,
    /// Run a plugin's test cases in the sandbox, with a fake clock,
    /// randomness, environment, network and secrets; exits 1 on a failure
    Test(author::TestArgs),
    /// Scaffold a plugin project for one extension point
    New(author::NewArgs),
}

/// `--workspace`, for the commands that act on a workspace's plugins.
#[derive(Debug, Args)]
struct WorkspaceFlag {
    /// Workspace directory (default: search from the current directory upwards)
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
}

impl WorkspaceFlag {
    fn root(&self) -> Result<PathBuf, ConfigError> {
        rustman_core::discovery::find_root(&current_dir()?, self.workspace.as_deref())
    }
}

#[derive(Debug, Args)]
struct AddArgs {
    /// A plugin folder or .wasm file inside the workspace, or an https URL
    source: String,
    /// The workspace's name for the plugin (default: the manifest's name)
    #[arg(long, value_name = "NAME")]
    name: Option<String>,
    #[command(flatten)]
    workspace: WorkspaceFlag,
}

#[derive(Debug, Args)]
struct VerifyArgs {
    /// Also download URL sources that name this machine or a private
    /// network (localhost, 10.0.0.5), which a committed source may not
    /// reach otherwise; use it only for sources you trust
    #[arg(long)]
    allow_local_sources: bool,
    #[command(flatten)]
    workspace: WorkspaceFlag,
}

#[derive(Debug, Args)]
struct RemoveArgs {
    /// The plugin's name in rustman.toml
    name: String,
    #[command(flatten)]
    workspace: WorkspaceFlag,
}

pub fn run(args: &PluginArgs) -> Result<u8, CliError> {
    match &args.command {
        PluginCommand::Add(args) => add(args),
        PluginCommand::List(args) => list(args),
        PluginCommand::Remove(args) => remove(args),
        PluginCommand::Verify(args) => verify(args),
        PluginCommand::Api => {
            api();
            Ok(SUCCESS)
        }
        PluginCommand::Test(args) => author::test(args),
        PluginCommand::New(args) => author::new(args),
    }
}

fn usage(error: LoadError) -> CliError {
    CliError::Config(ConfigError::Usage(error.to_string()))
}

fn add(args: &AddArgs) -> Result<u8, CliError> {
    let root = args.workspace.root()?;
    let source = manage::source_text(&root, &current_dir()?, &args.source).map_err(usage)?;
    let options = manage::fetch_options(&root).map_err(usage)?;
    let added = super::exec::runtime()?
        .block_on(manage::add(&root, &source, args.name.as_deref(), &options))
        .map_err(usage)?;
    let manifest = &added.manifest;
    let points: Vec<&str> = manifest
        .extension_points
        .iter()
        .map(|p| p.as_str())
        .collect();
    let capabilities: Vec<String> = manifest
        .capabilities
        .iter()
        .map(ToString::to_string)
        .collect();
    println!(
        "plugin {} {} (API {})",
        manifest.name, manifest.version, manifest.api
    );
    if let Some(description) = &manifest.description {
        println!("  {description}");
    }
    println!("  extension points: {}", points.join(", "));
    println!("  requested capabilities: {}", list_or_none(&capabilities));
    println!("  sha256: {}", added.declaration.sha256);
    println!(
        "added [plugins.{}] to rustman.toml with grants = [] and updated rustman.lock",
        added.declaration.name
    );
    println!("grant what it needs in rustman.toml, then trust the workspace's plugin set");
    Ok(SUCCESS)
}

fn list(args: &WorkspaceFlag) -> Result<u8, CliError> {
    let root = args.root()?;
    let Some((plugins, trust)) = manage::list(&root, TrustKey::machine()).map_err(usage)? else {
        println!("no plugins declared in rustman.toml");
        return Ok(SUCCESS);
    };
    print!("{}", table(&plugins));
    println!("plugin set: {}", trust.as_str());
    Ok(SUCCESS)
}

/// The aligned `NAME VERSION API SOURCE GRANTS` table of `plugins`.
fn table(plugins: &[Listed]) -> String {
    let rows: Vec<[String; 5]> =
        std::iter::once(["NAME", "VERSION", "API", "SOURCE", "GRANTS"].map(str::to_owned))
            .chain(plugins.iter().map(|p| {
                [
                    p.name.clone(),
                    p.version.clone(),
                    p.api.clone(),
                    p.source.clone(),
                    list_or_none(&p.grants),
                ]
            }))
            .collect();
    let widths: Vec<usize> = (0..4)
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for row in &rows {
        for (cell, width) in row.iter().zip(&widths) {
            out.push_str(&format!("{cell:<width$}  "));
        }
        out.push_str(&row[4]);
        out.push('\n');
    }
    out
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}

fn remove(args: &RemoveArgs) -> Result<u8, CliError> {
    let root = args.workspace.root()?;
    manage::remove(&root, &args.name).map_err(usage)?;
    println!(
        "removed [plugins.{}] from rustman.toml and rustman.lock",
        args.name
    );
    Ok(SUCCESS)
}

fn verify(args: &VerifyArgs) -> Result<u8, CliError> {
    let root = args.workspace.root()?;
    let mut fetch = manage::fetch_options(&root).map_err(usage)?;
    fetch.allow_local = args.allow_local_sources;
    let (set, lock_changed) = super::exec::runtime()?
        .block_on(manage::verify(&root, fetch))
        .map_err(usage)?;
    let skipped: Vec<String> = set.skipped.iter().map(ToString::to_string).collect();
    print_warnings(&skipped);
    for plugin in &set.plugins {
        println!(
            "ok {} {} sha256 {}",
            plugin.name(),
            plugin.manifest.version,
            plugin.declaration.sha256
        );
    }
    if set.is_empty() {
        println!("no plugins declared in rustman.toml");
    }
    if lock_changed {
        println!("updated rustman.lock");
    }
    Ok(SUCCESS)
}

fn api() {
    use rustman_plugin::manifest::{API_VERSION, PACKAGE, supported_api};
    println!("{PACKAGE}@{API_VERSION}");
    println!("supported plugin API majors: {}", supported_api());
}

/// A path as shown in messages, with `/` separators.
fn shown(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
