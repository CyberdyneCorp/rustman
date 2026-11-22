//! `rustman evm signer import <id>` (evm-signing "Approved signers only"):
//! the one place a private key is accepted. It is read with a no-echo
//! prompt on a terminal (or from stdin when piped), stored in the OS
//! keychain, and never printed.

use std::io::{self, IsTerminal, Read};

use clap::{Args, Subcommand};
use rustman_core::secrets::{BACKEND_VAR, Keychain, SecretBackend, keychain};
use rustman_evm::EvmError;
use rustman_evm_signing::Zeroizing;
use rustman_evm_signing::import::{import_key, imported_message};

use crate::exit::{CliError, SUCCESS};

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  rustman evm signer import deployer
  rustman evm signer import ci < ci.key

The key is read with a no-echo prompt (or from stdin when piped), stored in
the OS keychain, and used from rustman.toml with:
  [evm.signers.<id>]
  keychain = true")]
pub struct SignerArgs {
    #[command(subcommand)]
    command: SignerCommand,
}

#[derive(Debug, Subcommand)]
enum SignerCommand {
    /// Store a private key in the OS keychain as signer <ID>
    Import(ImportArgs),
}

#[derive(Debug, Args)]
struct ImportArgs {
    /// The signer id, as used in [evm.signers.<ID>] and `signer:`
    #[arg(value_name = "ID")]
    id: String,
}

pub fn run(args: &SignerArgs) -> Result<u8, CliError> {
    match &args.command {
        SignerCommand::Import(args) => Ok(import(args)),
    }
}

fn import(args: &ImportArgs) -> u8 {
    let result = rustman_evm_signing::config::check_id(&args.id)
        .and_then(|()| the_keychain())
        .and_then(|keychain| {
            let key = read_key(&args.id)?;
            import_key(keychain.as_ref(), &args.id, &key)
        });
    match result {
        Ok(address) => {
            println!("{}", imported_message(&args.id, address));
            SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            error.exit_code()
        }
    }
}

/// The OS keychain (or the stand-in a test points at). Signer keys never
/// fall back to a file, so `RUSTMAN_SECRET_STORE=file` means none.
pub(super) fn the_keychain() -> Result<Box<dyn Keychain>, EvmError> {
    if let Some(stand_in) = crate::commands::record::test_hooks::keychain() {
        return Ok(stand_in);
    }
    let backend = SecretBackend::from_env_value(std::env::var(BACKEND_VAR).ok().as_deref())
        .map_err(|e| EvmError::config(e.to_string()))?;
    let keychain = match backend {
        SecretBackend::Keychain => keychain::os_keychain(),
        SecretBackend::File => None,
    };
    keychain.ok_or_else(|| {
        EvmError::config("no OS keychain is available; signer keys are never stored in files")
    })
}

/// The key: a no-echo prompt on a terminal, else the first line of stdin.
fn read_key(id: &str) -> Result<Zeroizing<String>, EvmError> {
    let failed = |e: io::Error| EvmError::config(format!("cannot read the key: {e}"));
    if io::stdin().is_terminal() {
        let prompt = format!("private key for signer '{id}' (input hidden): ");
        return rpassword::prompt_password(prompt)
            .map(Zeroizing::new)
            .map_err(failed);
    }
    let mut input = Zeroizing::new(String::new());
    io::stdin().read_to_string(&mut input).map_err(failed)?;
    let line = input.lines().next().unwrap_or_default();
    Ok(Zeroizing::new(line.to_owned()))
}
