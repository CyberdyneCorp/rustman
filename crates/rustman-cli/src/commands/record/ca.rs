//! `rustman record ca init | export | install | uninstall | rotate |
//! remove`: the local CA's lifecycle (traffic-recorder spec: Local CA generation and
//! storage, CA is never trusted automatically, CA rotation). Trust stores
//! change only here, each command printed before it runs, and `install`
//! `rotate` and `remove` need a typed `yes` (or `--yes`).

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use rustman_core::ConfigError;
use rustman_core::secrets::provider::config::user_config_dir;
use rustman_core::secrets::{BACKEND_VAR, Keychain, SecretBackend, keychain, workspace_id};
use rustman_proxy::ca::store::CaStore;
use rustman_proxy::ca::trust::{
    self, Cmd, INSTALL_WARNING, InstallOutcome, InstallPlan, SystemRunner, TrustStore,
};
use rustman_proxy::ca::{CaError, CaInfo, info_from_pem};

use crate::exit::{ASSERTION, CliError, SUCCESS};

#[derive(Debug, Args)]
#[command(subcommand_required = true, arg_required_else_help = true)]
pub struct CaArgs {
    #[command(subcommand)]
    command: CaCommand,
}

#[derive(Debug, Subcommand)]
enum CaCommand {
    /// Create this workspace's local CA (key in the OS keychain, certificate
    /// in the user config directory); it is not trusted until `ca install`
    Init(WorkspaceArg),
    /// Print the CA certificate (PEM, never the key) and its fingerprint
    Export(ExportArgs),
    /// Trust the CA in this machine's trust store, after confirmation
    Install(ConfirmArgs),
    /// Remove the CA from every trust store Rustman installed it in
    Uninstall(WorkspaceArg),
    /// Replace the CA: uninstall and delete the old one, create a new one
    Rotate(ConfirmArgs),
    /// Delete the CA: uninstall it, then delete its key from the keychain
    /// and its certificate
    Remove(ConfirmArgs),
}

#[derive(Debug, Args)]
struct WorkspaceArg {
    /// Workspace whose CA to use (default: search from the current
    /// directory upwards)
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ExportArgs {
    /// Write the PEM certificate to FILE instead of stdout
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
    #[command(flatten)]
    workspace: WorkspaceArg,
}

#[derive(Debug, Args)]
struct ConfirmArgs {
    /// Do not ask for confirmation
    #[arg(long)]
    yes: bool,
    #[command(flatten)]
    workspace: WorkspaceArg,
}

pub fn run(args: &CaArgs) -> Result<u8, CliError> {
    match &args.command {
        CaCommand::Init(a) => init(&new_store(&root(a)?)?),
        CaCommand::Export(a) => export(&existing(&a.workspace)?, a.out.as_deref()),
        CaCommand::Install(a) => install(&existing(&a.workspace)?, a.yes),
        CaCommand::Uninstall(a) => uninstall(&existing(a)?),
        CaCommand::Rotate(a) => rotate(&new_store(&root(&a.workspace)?)?, a.yes),
        CaCommand::Remove(a) => remove(&existing(&a.workspace)?, a.yes),
    }
}

/// CA errors are configuration errors (exit code 2).
pub fn ca_error(err: CaError) -> CliError {
    ConfigError::Usage(err.to_string()).into()
}

fn root(arg: &WorkspaceArg) -> Result<PathBuf, CliError> {
    let cwd = super::super::current_dir()?;
    Ok(rustman_core::discovery::find_root(
        &cwd,
        arg.workspace.as_deref(),
    )?)
}

/// The CA store of a workspace that must already have a CA.
fn existing(arg: &WorkspaceArg) -> Result<CaStore, CliError> {
    existing_store(&root(arg)?)?.ok_or_else(|| ca_error(CaError::NotFound))
}

/// The CA store of the workspace at `root`, or `None` when the workspace
/// has no id yet (so no CA either). Nothing is created.
pub fn existing_store(root: &Path) -> Result<Option<CaStore>, CliError> {
    let config = user_config_dir().ok_or_else(|| ca_error(CaError::NoConfigDir))?;
    let id = workspace_id::load(root).map_err(ConfigError::from)?;
    let keychain = record_keychain()?;
    Ok(id.map(|id| CaStore::new(keychain, &config, &id)))
}

/// The CA store of the workspace at `root`, for creating a CA: refused
/// without a keychain before anything is written.
pub fn new_store(root: &Path) -> Result<CaStore, CliError> {
    let keychain = record_keychain()?.ok_or_else(|| ca_error(CaError::NoKeychain))?;
    let config = user_config_dir().ok_or_else(|| ca_error(CaError::NoConfigDir))?;
    let id = workspace_id::load_or_create(root).map_err(ConfigError::from)?;
    Ok(CaStore::new(Some(keychain), &config, &id))
}

/// The OS keychain, unless `RUSTMAN_SECRET_STORE=file` keeps Rustman away
/// from it; the CA key never falls back to a file.
fn record_keychain() -> Result<Option<Box<dyn Keychain>>, CliError> {
    if let Some(stand_in) = super::test_hooks::DirKeychain::from_env() {
        return Ok(Some(Box::new(stand_in)));
    }
    let backend = SecretBackend::from_env_value(std::env::var(BACKEND_VAR).ok().as_deref())
        .map_err(ConfigError::from)?;
    Ok(match backend {
        SecretBackend::Keychain => keychain::os_keychain(),
        SecretBackend::File => None,
    })
}

/// Create the CA, after the prompt `rustman record --intercept` shows when
/// there is none.
pub fn create(store: &CaStore) -> Result<rustman_proxy::ca::LocalCa, CliError> {
    let ca = store.init().map_err(ca_error)?;
    print_created(store, &ca.info().map_err(ca_error)?);
    Ok(ca)
}

fn init(store: &CaStore) -> Result<u8, CliError> {
    create(store)?;
    println!("It is not trusted yet: run 'rustman record ca install' to trust it on this machine.");
    Ok(SUCCESS)
}

fn print_created(store: &CaStore, info: &CaInfo) {
    println!("Created the local CA of this workspace");
    print_info(info);
    println!("  key:     OS keychain (rustman / {})", store.account());
    println!("  cert:    {}", store.cert_path().display());
}

fn print_info(info: &CaInfo) {
    println!("  subject: {}", info.subject);
    println!("  SHA-256: {}", info.sha256);
    println!("  expires: {}", info.expires());
}

fn export(store: &CaStore, out: Option<&Path>) -> Result<u8, CliError> {
    let pem = store
        .certificate()
        .map_err(ca_error)?
        .ok_or_else(|| ca_error(CaError::NotFound))?;
    let info = info_from_pem(&pem).map_err(ca_error)?;
    match out {
        None => {
            print!("{pem}");
            eprintln!("SHA-256: {}", info.sha256);
        }
        Some(path) => {
            std::fs::write(path, &pem).map_err(|source| ConfigError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            println!("Wrote {}", path.display());
            println!("SHA-256: {}", info.sha256);
        }
    }
    Ok(SUCCESS)
}

fn install(store: &CaStore, yes: bool) -> Result<u8, CliError> {
    let detected = TrustStore::detect(std::env::consts::OS, &SystemRunner);
    let plan = trust::plan_install(store, detected).map_err(ca_error)?;
    print_plan(&plan);
    let confirm = |_: &InstallPlan| yes || confirmed("Type yes to trust this CA: ");
    let outcome =
        trust::install(store, &plan, &SystemRunner, confirm, &mut announce).map_err(ca_error)?;
    match outcome {
        InstallOutcome::Installed => {
            println!("Installed. Remove it with 'rustman record ca uninstall'.");
            println!("Firefox keeps its own store: see docs/record.md to trust the CA there.");
            Ok(SUCCESS)
        }
        InstallOutcome::Declined => {
            println!("Not installed; no trust store was changed.");
            Ok(ASSERTION)
        }
    }
}

fn print_plan(plan: &InstallPlan) {
    println!("This trusts the Rustman local CA in {}:", plan.store.name());
    print_info(&plan.info);
    println!("{INSTALL_WARNING}");
    println!("Commands:");
    for command in &plan.commands {
        println!("  {command}");
    }
}

fn uninstall(store: &CaStore) -> Result<u8, CliError> {
    let commands = trust::plan_uninstall(store).map_err(ca_error)?;
    if commands.is_empty() {
        println!("Rustman did not install this workspace's CA in any trust store.");
        return Ok(SUCCESS);
    }
    let removed = trust::uninstall(store, &SystemRunner, &mut announce).map_err(ca_error)?;
    println!("Removed the CA from {removed} trust store(s).");
    Ok(SUCCESS)
}

fn rotate(store: &CaStore, yes: bool) -> Result<u8, CliError> {
    let commands = trust::plan_uninstall(store).map_err(ca_error)?;
    println!(
        "This replaces the local CA: the old key is deleted from the keychain and the old CA \
         is removed from the trust stores Rustman installed it in."
    );
    for command in &commands {
        println!("  {command}");
    }
    if !yes && !confirmed("Type yes to rotate the CA: ") {
        println!("Not rotated; nothing was changed.");
        return Ok(ASSERTION);
    }
    let rotation = trust::rotate(store, &SystemRunner, &mut announce).map_err(ca_error)?;
    if let Some(old) = &rotation.old {
        println!("old SHA-256: {}", old.sha256);
    }
    println!("new SHA-256: {}", rotation.new.sha256);
    println!("The new CA is not trusted yet: run 'rustman record ca install' to trust it.");
    Ok(SUCCESS)
}

fn remove(store: &CaStore, yes: bool) -> Result<u8, CliError> {
    let pem = store
        .certificate()
        .map_err(ca_error)?
        .ok_or_else(|| ca_error(CaError::NotFound))?;
    let info = info_from_pem(&pem).map_err(ca_error)?;
    let commands = trust::plan_uninstall(store).map_err(ca_error)?;
    println!("This deletes the local CA of this workspace:");
    print_info(&info);
    println!("  key:     OS keychain (rustman / {})", store.account());
    println!("  cert:    {}", store.cert_path().display());
    if !commands.is_empty() {
        println!("It is first removed from the trust stores Rustman installed it in:");
        for command in &commands {
            println!("  {command}");
        }
    }
    if !yes && !confirmed("Type yes to delete the CA: ") {
        println!("Not removed; nothing was changed.");
        return Ok(ASSERTION);
    }
    let removal = trust::remove(store, &SystemRunner, &mut announce).map_err(ca_error)?;
    println!(
        "Deleted the CA {} (uninstalled from {} trust store(s)).",
        removal.old.sha256, removal.uninstalled
    );
    println!("Run 'rustman record ca init' to create a new one.");
    Ok(SUCCESS)
}

fn announce(command: &Cmd) {
    println!("$ {command}");
}

/// Ask `prompt` and read one line: only `yes` confirms.
pub fn confirmed(prompt: &str) -> bool {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line);
    read.is_ok() && line.trim() == "yes"
}
