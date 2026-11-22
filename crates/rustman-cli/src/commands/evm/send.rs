//! `rustman evm send <request> --sign`: an `evm-tx` request through the
//! signing gate (evm-signing), whose steps live in
//! `rustman_evm_signing::flow`. This command only adds the terminal: the
//! preview and summary on stdout, prompts on stderr, and the exit code
//! (a refusal exits with code 4; a receipt timeout or an unreadable
//! receipt prints the hash and explorer link and exits with code 3; Ctrl+C
//! before the transaction is signed exits with code 130, during the
//! receipt wait with code 3 and the audit record kept).

use std::io::{self, BufRead, IsTerminal, Write};
use std::sync::mpsc;
use std::time::Duration;

use clap::Args;
use rustman_core::model::{EvmRequest, Request};
use rustman_core::run::{RequestRecord, RequestStatus};
use rustman_evm_signing::send::Wait;
use rustman_evm_signing::{Console, Ending, SignOptions, Signing, Zeroizing};
use rustman_http::runner::{EvmTxCall, RunOptions, Runner};
use tokio_util::sync::CancellationToken;

use super::{finish, shown};
use crate::commands::exec::{
    cancel_on_ctrl_c, history_store, overrides, parse_duration, runtime, secret_store,
};
use crate::commands::session::{Kind, Session};
use crate::commands::{EnvSelection, print_warnings};
use crate::exit::{ASSERTION, CliError, REQUEST, SUCCESS};

/// The environment variable `--signer` falls back to.
pub const SIGNER_VAR: &str = "RUSTMAN_EVM_SIGNER";

/// Time between two receipt polls.
const POLL: Duration = Duration::from_millis(250);

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  rustman evm send requests/transfer.rman.yaml --env local
  rustman evm send transfer --env sepolia --sign --signer deployer
  rustman evm send transfer --env local --sign --yes-local --confirmations 2

Signing needs [evm] allow_signing = true in rustman.toml, --sign, a signer
from [evm.signers], an environment that pins chain_id, a simulation that does
not revert (or --force-revert) and your confirmation. On chain 1 and
[evm] protected_chains the chain name must be typed. Without a terminal, only
chain 31337 and [evm] headless_signing_chains can be signed on, with
--yes-local. Raw private keys are never accepted; import a key with
`rustman evm signer import`.")]
pub struct SendArgs {
    /// An evm-tx request (file, id or name)
    #[arg(value_name = "REQUEST")]
    request: String,
    /// Sign and send the transaction (without it, only the preview)
    #[arg(long)]
    sign: bool,
    /// The signer id from [evm.signers] (default: RUSTMAN_EVM_SIGNER, then
    /// the request's `signer`)
    #[arg(long, value_name = "ID")]
    signer: Option<String>,
    /// Go on when the simulation reverts; the confirmation warns
    #[arg(long)]
    force_revert: bool,
    /// Confirm without a prompt; only on chain 31337 and
    /// [evm] headless_signing_chains
    #[arg(long)]
    yes_local: bool,
    /// Blocks to wait for, counting the transaction's own
    #[arg(long, value_name = "N", default_value_t = 1,
          value_parser = clap::value_parser!(u64).range(1..))]
    confirmations: u64,
    /// How long to wait for the receipt: 120s, 5m, or milliseconds
    #[arg(long, value_name = "DURATION", value_parser = parse_duration,
          default_value = "120s")]
    receipt_timeout: Duration,
    #[command(flatten)]
    selection: EnvSelection,
    /// Set a variable, above every other source (repeatable): --var k=v
    #[arg(long = "var", value_name = "K=V")]
    vars: Vec<String>,
    /// Total timeout per node call: 500ms, 5s, 2m, or milliseconds
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    timeout: Option<Duration>,
    /// Do not verify TLS certificates
    #[arg(long, short = 'k')]
    insecure: bool,
    /// Do not record the transaction in history
    #[arg(long)]
    no_history: bool,
}

/// `evm-tx` requests.
const EVM_TX: Kind = Kind {
    name: "evm-tx",
    matches: |request| matches!(request, Request::Evm(evm) if matches!(**evm, EvmRequest::Tx(_))),
};

pub fn run(args: &SendArgs) -> Result<u8, CliError> {
    let session = Session::open(&args.selection, false)?;
    let target = session.find(&args.request, &EVM_TX)?;
    let environment = match &session.ws {
        Some(ws) => args.selection.environment(ws)?,
        None => None,
    };
    let store = secret_store(session.root())?;
    let options = RunOptions {
        overrides: overrides(&args.vars)?,
        timeout: args.timeout,
        insecure: args.insecure,
        history: history_store(args.no_history, session.ws.as_ref(), &session.settings),
        ..RunOptions::default()
    };
    let cancel = options.cancel.clone();
    let console = TerminalConsole {
        stdout: true,
        cancel: cancel.clone(),
    };
    let ctx = rustman_http::runner::RunContext {
        root: session.root(),
        settings: &session.settings,
        environment,
        secrets: &store,
        in_workspace: true,
    };
    let providers = crate::commands::exec::secret_providers(session.root(), &session.settings)?;
    let runner = Runner::new(ctx, options)?.with_secret_providers(providers);
    let signing = Signing::new(args.sign_options(), &console, keychain());
    let call = EvmTxCall {
        runner: &runner,
        request: &target,
        record: RequestRecord::new(target.entry.id(), String::new(), String::new()),
        root: session.root(),
        settings: &session.settings,
        environment,
    };
    let code = runtime()?.block_on(async {
        cancel_on_ctrl_c(&cancel);
        let result = signing
            .send(&call)
            .await
            .map(|ending| report(&runner, ending));
        finish(result, &runner.known_secrets())
    });
    print_warnings(&store.take_warnings());
    Ok(code)
}

impl SendArgs {
    fn sign_options(&self) -> SignOptions {
        SignOptions {
            sign: self.sign,
            signer: signer_flag(self.signer.as_deref()),
            force_revert: self.force_revert,
            yes_local: self.yes_local,
            wait: Wait {
                confirmations: self.confirmations,
                timeout: self.receipt_timeout,
                poll: POLL,
            },
        }
    }
}

/// The signing gate of `rustman run --sign`: one at a time, with the
/// run's nonces, the signer from `RUSTMAN_EVM_SIGNER` or each request.
pub(crate) fn run_signing(yes_local: bool, console: &TerminalConsole) -> Signing<'_> {
    let options = SignOptions {
        sign: true,
        signer: signer_flag(None),
        yes_local,
        wait: Wait {
            poll: POLL,
            ..Wait::default()
        },
        ..SignOptions::default()
    };
    Signing::new(options, console, keychain())
}

/// `--signer`, else `RUSTMAN_EVM_SIGNER`.
pub(crate) fn signer_flag(flag: Option<&str>) -> Option<String> {
    flag.map(str::to_owned)
        .or_else(|| std::env::var(SIGNER_VAR).ok())
}

/// The OS keychain (or the tests' stand-in), when there is one.
pub(crate) fn keychain() -> Option<Box<dyn rustman_core::secrets::Keychain>> {
    super::signer::the_keychain().ok()
}

/// Print failed assertions, capture errors and warnings; the exit code.
fn report(runner: &Runner<'_>, ending: Ending) -> u8 {
    let redactor = runner.known_secrets();
    let (record, succeeded) = match ending {
        Ending::Mined { record, succeeded } => (record, succeeded),
        Ending::Unconfirmed { record, explorer } => {
            print_record(&record, &redactor);
            if let Some(url) = explorer {
                eprintln!("explorer: {url}");
            }
            return REQUEST;
        }
    };
    print_record(&record, &redactor);
    if !succeeded {
        eprintln!("error: the transaction reverted on chain");
        return ASSERTION;
    }
    match record.status {
        RequestStatus::Passed => SUCCESS,
        _ => ASSERTION,
    }
}

fn print_record(record: &RequestRecord, redactor: &rustman_core::Redactor) {
    for failed in record.assertions.iter().filter(|a| !a.passed()) {
        eprintln!("fail: {}", shown(redactor, &failed.to_string()));
    }
    if let Some(error) = &record.error {
        eprintln!("error: {}", shown(redactor, error));
    }
    for warning in &record.warnings {
        eprintln!("warning: {}", shown(redactor, warning));
    }
}

/// The terminal the gate talks to: `show` on stdout (`evm send`) or
/// stderr (`run`, whose stdout is the report); prompts on stderr. Ctrl+C
/// (`cancel`) interrupts a prompt.
pub(crate) struct TerminalConsole {
    pub stdout: bool,
    pub cancel: CancellationToken,
}

/// How often a prompt looks at Ctrl+C while it waits for a line.
const PROMPT_TICK: Duration = Duration::from_millis(50);

impl Console for TerminalConsole {
    fn interactive(&self) -> bool {
        io::stdin().is_terminal()
    }

    fn show(&self, text: &str) {
        match self.stdout {
            true => println!("{text}"),
            false => eprintln!("{text}"),
        }
    }

    fn note(&self, text: &str) {
        eprintln!("{text}");
    }

    /// The line is read on a thread of its own, so Ctrl+C (heard on the
    /// signal thread, which cancels the token) ends the wait at once even
    /// though the read itself cannot be interrupted. Input typed before
    /// the question is discarded, so an answer typed ahead never confirms
    /// a summary the user has not seen yet.
    fn ask(&self, question: &str) -> Option<String> {
        discard_type_ahead();
        eprint!("{question}");
        let _ = io::stderr().flush();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = io::stdin().lock().read_line(&mut line);
            let _ = tx.send(line);
        });
        loop {
            if self.cancel.is_cancelled() {
                eprintln!();
                return None;
            }
            match rx.recv_timeout(PROMPT_TICK) {
                Ok(line) => return Some(line.trim_end_matches(['\r', '\n']).to_owned()),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Some(String::new()),
            }
        }
    }

    fn password(&self, question: &str) -> Result<Option<Zeroizing<String>>, String> {
        if !io::stdin().is_terminal() {
            return Ok(None);
        }
        rpassword::prompt_password(question)
            .map(|password| Some(Zeroizing::new(password)))
            .map_err(|e| e.to_string())
    }

    fn cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

/// Drop what was typed on the terminal and not read yet. On Windows the
/// console input buffer is not flushed (that needs an unsafe call); a
/// typed-ahead answer can still reach the prompt there.
fn discard_type_ahead() {
    #[cfg(unix)]
    {
        let stdin = io::stdin();
        if stdin.is_terminal() {
            let _ = rustix::termios::tcflush(&stdin, rustix::termios::QueueSelector::IFlush);
        }
    }
}
