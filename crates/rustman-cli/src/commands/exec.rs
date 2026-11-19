//! What `rustman send` and `rustman run` share: flags, the async runtime,
//! Ctrl+C handling and the OAuth2 login mode.

use std::io::IsTerminal;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use clap::Args;
use rustman_core::Workspace;
use rustman_core::history::HistoryStore;
use rustman_core::model::Variables;
use rustman_core::run::RequestRecord;
use rustman_core::secrets::BACKEND_VAR;
use rustman_core::settings::{HistorySettings, Settings};
use rustman_core::snapshot::{CI_ENV, SnapshotMode};
use rustman_core::vars::parse_overrides;
use rustman_core::{ConfigError, SecretBackend, SecretStore};
use rustman_http::RequestError;
use rustman_http::runner::{LoginMode, RunOptions};
use tokio_util::sync::CancellationToken;

/// Flags that change how requests are sent, shared by `send` and `run`.
#[derive(Debug, Args)]
pub struct SendFlags {
    /// Set a variable, above every other source (repeatable): --var k=v
    #[arg(long = "var", value_name = "K=V")]
    pub vars: Vec<String>,
    /// Total timeout per request, replacing settings.timeout_ms: 500ms, 5s,
    /// 2m, or a number of milliseconds
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    pub timeout: Option<Duration>,
    /// Do not verify TLS certificates (exchanges are flagged insecure)
    #[arg(long, short = 'k')]
    pub insecure: bool,
    /// Do not record the exchanges in history
    #[arg(long)]
    pub no_history: bool,
    /// Overwrite snapshots that differ from the response (not in CI mode)
    #[arg(short = 'u', long)]
    pub update_snapshots: bool,
    /// CI mode: never write snapshots, and fail on a missing one (also on
    /// when the CI variable is true or 1)
    #[arg(long)]
    pub ci: bool,
}

impl SendFlags {
    /// Run options with these flags, a fresh cancellation token and the
    /// login mode for the current terminal.
    pub fn options(&self) -> Result<RunOptions<'static>, ConfigError> {
        Ok(RunOptions {
            overrides: overrides(&self.vars)?,
            timeout: self.timeout,
            insecure: self.insecure,
            login: login_mode(),
            snapshots: self.snapshot_mode()?,
            ..RunOptions::default()
        })
    }

    /// How snapshot files are treated: `--update-snapshots`, `--ci` and the
    /// `CI` variable; updating in CI mode is a usage error.
    pub fn snapshot_mode(&self) -> Result<SnapshotMode, ConfigError> {
        let ci_env = std::env::var(CI_ENV).ok();
        SnapshotMode::detect(self.update_snapshots, self.ci, ci_env.as_deref())
            .map_err(|message| ConfigError::Usage(message.to_owned()))
    }
}

/// Print, on stderr, each snapshot a sent request recorded or updated, such
/// as `snapshot default recorded: users/__snapshots__/get-user.default.snap.yaml`.
pub fn print_snapshot_notes(record: &RequestRecord) {
    let written = record
        .assertions
        .iter()
        .filter_map(|a| a.snapshot.as_ref())
        .filter(|s| s.wrote());
    for snapshot in written {
        eprintln!("{}", snapshot.note());
    }
}

pub(crate) fn overrides(vars: &[String]) -> Result<Variables, ConfigError> {
    parse_overrides(vars).map_err(ConfigError::Usage)
}

pub use rustman_core::duration::parse_duration;

/// Where exchanges are recorded: the workspace's history, or for ad-hoc
/// requests outside one the platform data directory. `None` with
/// `--no-history` or `[history] enabled = false`.
pub fn history_store(
    no_history: bool,
    ws: Option<&Workspace>,
    settings: &Settings,
) -> Option<HistoryStore> {
    if no_history || !settings.history.enabled {
        return None;
    }
    match ws {
        Some(ws) => Some(HistoryStore::for_workspace(&ws.root, &settings.history)),
        None => match HistoryStore::global(&HistorySettings::default()) {
            Ok(store) => Some(store),
            Err(e) => {
                eprintln!("warning: history: {e}");
                None
            }
        },
    }
}

/// The OAuth2 login mode: interactive only when stdin is a terminal.
pub fn login_mode() -> LoginMode<'static> {
    if std::io::stdin().is_terminal() {
        LoginMode::Interactive(&show_login_url)
    } else {
        LoginMode::NonInteractive
    }
}

/// The secret store of the workspace at `root`, as `RUSTMAN_SECRET_STORE` selects.
pub fn secret_store(root: &Path) -> Result<SecretStore, ConfigError> {
    let backend = SecretBackend::from_env_value(std::env::var(BACKEND_VAR).ok().as_deref())
        .map_err(ConfigError::from)?;
    Ok(SecretStore::open(root, backend))
}

/// A runtime on the current thread: requests of a run interleave on it.
pub fn runtime() -> Result<tokio::runtime::Runtime, RequestError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| RequestError::Io(format!("cannot start the async runtime: {e}")))
}

/// Cancel `token` on Ctrl+C (SIGINT, and also SIGTERM and SIGHUP, on Unix;
/// the console handler on Windows); a second one quits at once with exit
/// code 130, even while reports are being written after the run, after
/// killing the stdio MCP servers still running (they sit in process groups
/// of their own, out of reach of terminal signals). The signal is watched
/// on its own thread, so it is heard whatever the main thread is doing; it
/// is registered before this returns.
pub fn cancel_on_ctrl_c(token: &CancellationToken) {
    let token = token.clone();
    let (ready, registered) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("ctrl-c".into())
        .spawn(move || {
            let Ok(runtime) = runtime() else { return };
            runtime.block_on(async move {
                let Ok(mut interrupts) = interrupts() else {
                    return;
                };
                let _ = ready.send(());
                watch_interrupts(&mut interrupts, &token, quit_interrupted).await;
            });
        });
    if spawned.is_ok() {
        // An error means the thread could not listen; Ctrl+C then keeps
        // its default behavior and still ends the process.
        let _ = registered.recv();
    }
}

/// A source of Ctrl+C notifications; `None` when it ends.
trait Interrupts {
    fn recv(&mut self) -> impl Future<Output = Option<()>>;
}

/// SIGINT, SIGTERM and SIGHUP: each asks the process to stop.
#[cfg(unix)]
struct Terminations([tokio::signal::unix::Signal; 3]);

#[cfg(unix)]
impl Interrupts for Terminations {
    async fn recv(&mut self) -> Option<()> {
        let [int, term, hup] = &mut self.0;
        tokio::select! {
            got = int.recv() => got,
            got = term.recv() => got,
            got = hup.recv() => got,
        }
    }
}

#[cfg(windows)]
impl Interrupts for tokio::signal::windows::CtrlC {
    fn recv(&mut self) -> impl Future<Output = Option<()>> {
        tokio::signal::windows::CtrlC::recv(self)
    }
}

#[cfg(unix)]
fn interrupts() -> std::io::Result<impl Interrupts> {
    use tokio::signal::unix::{SignalKind, signal};
    Ok(Terminations([
        signal(SignalKind::interrupt())?,
        signal(SignalKind::terminate())?,
        signal(SignalKind::hangup())?,
    ]))
}

#[cfg(windows)]
fn interrupts() -> std::io::Result<impl Interrupts> {
    tokio::signal::windows::ctrl_c()
}

/// The first interrupt cancels `token`; the second calls `quit`.
async fn watch_interrupts(
    interrupts: &mut impl Interrupts,
    token: &CancellationToken,
    quit: impl FnOnce(),
) {
    if interrupts.recv().await.is_none() {
        return;
    }
    eprintln!("interrupted: cancelling in-flight requests (Ctrl+C again to quit now)");
    token.cancel();
    if interrupts.recv().await.is_some() {
        quit();
    }
}

fn quit_interrupted() {
    eprintln!("interrupted again: quitting");
    // `exit` runs no destructor: stop the servers' process trees first.
    rustman_mcp::client::kill_all_servers();
    std::process::exit(i32::from(crate::exit::INTERRUPTED));
}

/// Print the authorization URL and try to open it in the default browser.
pub fn show_login_url(url: &str) {
    eprintln!("Open this URL in your browser to log in:\n  {url}");
    let mut command = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(windows) {
        let mut command = Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    } else {
        Command::new("xdg-open")
    };
    // Without a browser the printed URL is the way in.
    let _ = command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `count` interrupts, then the end of the source.
    struct Scripted(u32);

    impl Interrupts for Scripted {
        async fn recv(&mut self) -> Option<()> {
            let more = self.0 > 0;
            self.0 = self.0.saturating_sub(1);
            more.then_some(())
        }
    }

    fn watch(count: u32) -> (bool, bool) {
        let token = CancellationToken::new();
        let mut quit = false;
        let runtime = runtime().unwrap();
        runtime.block_on(watch_interrupts(&mut Scripted(count), &token, || {
            quit = true
        }));
        (token.is_cancelled(), quit)
    }

    #[test]
    fn a_first_ctrl_c_cancels_and_a_second_quits() {
        assert_eq!(watch(0), (false, false));
        assert_eq!(watch(1), (true, false));
        assert_eq!(watch(2), (true, true));
    }

    #[test]
    fn durations_take_a_unit_or_milliseconds() {
        assert_eq!(parse_duration("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration("5s"), Ok(Duration::from_secs(5)));
        assert_eq!(parse_duration("2m"), Ok(Duration::from_secs(120)));
        assert_eq!(parse_duration("1500"), Ok(Duration::from_millis(1500)));
    }

    #[test]
    fn bad_durations_are_rejected() {
        for text in ["", "0", "5h", "fast", "-1s", "1.5s"] {
            assert!(parse_duration(text).is_err(), "{text}");
        }
        assert_eq!(
            parse_duration("5h").unwrap_err(),
            "invalid duration '5h' (use 500ms, 5s, 2m or milliseconds)"
        );
    }
}
