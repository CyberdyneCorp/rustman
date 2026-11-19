//! The stdio transport (mcp-client spec "Stdio transport"): the server is a
//! child process with the definition's args and cwd (the workspace root by
//! default). Its environment is the few variables of ours that programs
//! need to start ([`INHERITED_ENV`]) plus the definition's `env`; nothing
//! else of Rustman's environment (tokens, cloud credentials) reaches it. JSON-RPC goes over stdin and
//! stdout; stderr is captured into a per-session log (bounded in lines and
//! in line length). Shutting down closes stdin, waits [`SHUTDOWN_GRACE`],
//! then kills the process with everything it started (see
//! [`super::process_tree`]).

use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustman_core::Redactor;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use super::error::ClientError;
use super::process_tree::{self, ProcessTree};
use super::resolve::ResolvedStdio;

/// How long a server has to exit once its stdin is closed.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// How many stderr lines a disconnection reports.
pub const STDERR_TAIL: usize = 50;

/// How many stderr lines the log keeps; older ones are dropped.
const STDERR_KEEP: usize = 5_000;

/// The longest stderr line kept, in bytes; the rest of a longer line is
/// dropped and the line is marked [`TRUNCATED`].
pub const STDERR_LINE_MAX: usize = 4096;

/// Ends a stderr line cut at [`STDERR_LINE_MAX`].
pub const TRUNCATED: &str = " [truncated]";

/// The variables a stdio server inherits from Rustman's environment, when
/// set: what launchers such as `npx`, `uvx` or `python` need to find
/// programs, the user's home and a temporary folder. The definition's `env`
/// is applied over them.
pub const INHERITED_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TERM",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TMPDIR",
    "TEMP",
    "TMP",
    #[cfg(windows)]
    "SystemRoot",
    #[cfg(windows)]
    "SystemDrive",
    #[cfg(windows)]
    "windir",
    #[cfg(windows)]
    "ComSpec",
    #[cfg(windows)]
    "PATHEXT",
    #[cfg(windows)]
    "USERPROFILE",
    #[cfg(windows)]
    "APPDATA",
    #[cfg(windows)]
    "LOCALAPPDATA",
    #[cfg(windows)]
    "ProgramData",
    #[cfg(windows)]
    "ProgramFiles",
    #[cfg(windows)]
    "ProgramFiles(x86)",
    #[cfg(windows)]
    "HOMEDRIVE",
    #[cfg(windows)]
    "HOMEPATH",
    #[cfg(windows)]
    "NUMBER_OF_PROCESSORS",
    #[cfg(windows)]
    "PROCESSOR_ARCHITECTURE",
];

/// How long an exit waits for the last stderr lines to be read.
const STDERR_DRAIN: Duration = Duration::from_secs(2);

/// What a stdio server wrote to stderr, secrets redacted. Clones share it.
#[derive(Clone, Debug, Default)]
pub struct StderrLog {
    lines: Arc<Mutex<VecDeque<String>>>,
    redactor: Redactor,
}

impl StderrLog {
    fn new(redactor: Redactor) -> Self {
        Self {
            lines: Arc::default(),
            redactor,
        }
    }

    fn push(&self, line: &str) {
        let line = self.redactor.text(line).into_owned();
        let mut lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        if lines.len() == STDERR_KEEP {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    /// Every line kept, oldest first.
    pub fn lines(&self) -> Vec<String> {
        self.tail(usize::MAX)
    }

    /// The last `n` lines, oldest first.
    pub fn tail(&self, n: usize) -> Vec<String> {
        let lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        lines
            .iter()
            .skip(lines.len().saturating_sub(n))
            .cloned()
            .collect()
    }
}

/// How a stdio server process ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessExit {
    /// The exit code; `None` when ended by a signal.
    pub code: Option<i32>,
    /// Killed after the shutdown grace period.
    pub killed: bool,
}

/// A running stdio server: the pipes for the protocol, its stderr log and
/// a watch on its exit.
pub(crate) struct StdioProcess {
    pub stdout: ChildStdout,
    pub stdin: ChildStdin,
    pub stderr: StderrLog,
    pub exit: watch::Receiver<Option<ProcessExit>>,
    pub shutdown: Shutdown,
}

/// Asks the process watcher to stop the process once stdin is closed.
#[derive(Debug)]
pub(crate) struct Shutdown(Option<oneshot::Sender<Duration>>);

impl Shutdown {
    /// Give the process `grace` to exit on its own, then kill it.
    pub fn begin(&mut self, grace: Duration) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(grace);
        }
    }
}

/// Spawn the server of `stdio`. `name` names it in errors; `redactor` hides
/// the definition's secrets in its stderr.
pub(crate) fn spawn(
    name: &str,
    stdio: &ResolvedStdio,
    redactor: &Redactor,
) -> Result<StdioProcess, ClientError> {
    let mut child = command(stdio).spawn().map_err(|e| ClientError::Spawn {
        name: name.to_owned(),
        message: redactor
            .text(&format!("{}: {e}", redactor.resolved(&stdio.command)))
            .into_owned(),
    })?;
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err(ClientError::Spawn {
            name: name.to_owned(),
            message: "the process pipes are not available".into(),
        });
    };
    let tree = ProcessTree::new(child.id());
    let log = StderrLog::new(redactor.clone());
    let reader = tokio::spawn(read_stderr(stderr, log.clone()));
    let (exit_tx, exit) = watch::channel(None);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let watched = Watched {
        child,
        tree,
        reader,
    };
    tokio::spawn(watch_process(watched, shutdown_rx, exit_tx));
    Ok(StdioProcess {
        stdout,
        stdin,
        stderr: log,
        exit,
        shutdown: Shutdown(Some(shutdown_tx)),
    })
}

fn command(stdio: &ResolvedStdio) -> Command {
    let mut command = Command::new(process_tree::program(&stdio.command.text));
    process_tree::isolate(&mut command);
    command
        .args(stdio.args.iter().map(|a| a.text.as_str()))
        .env_clear()
        .envs(inherited_env())
        .envs(stdio.env.iter().map(|(k, v)| (k, v.text.as_str())))
        .current_dir(&stdio.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

/// The [`INHERITED_ENV`] variables set in our environment.
fn inherited_env() -> impl Iterator<Item = (&'static str, std::ffi::OsString)> {
    INHERITED_ENV
        .iter()
        .filter_map(|name| Some((*name, std::env::var_os(name)?)))
}

async fn read_stderr(mut stderr: ChildStderr, log: StderrLog) {
    let mut chunk = vec![0; 8192];
    let mut line = LineBuffer::default();
    while let Ok(read @ 1..) = stderr.read(&mut chunk).await {
        line.feed(&chunk[..read], &log);
    }
    line.finish(&log);
}

/// Splits stderr into lines, keeping at most [`STDERR_LINE_MAX`] bytes of
/// each so a server cannot grow the client's memory with an endless line.
#[derive(Debug, Default)]
struct LineBuffer {
    bytes: Vec<u8>,
    truncated: bool,
}

impl LineBuffer {
    fn feed(&mut self, data: &[u8], log: &StderrLog) {
        for piece in data.split_inclusive(|b| *b == b'\n') {
            match piece.strip_suffix(b"\n") {
                Some(body) => {
                    self.append(body);
                    self.flush(log);
                }
                None => self.append(piece),
            }
        }
    }

    fn append(&mut self, body: &[u8]) {
        let room = STDERR_LINE_MAX - self.bytes.len();
        if body.len() > room {
            self.truncated = true;
        }
        self.bytes.extend_from_slice(&body[..body.len().min(room)]);
    }

    fn flush(&mut self, log: &StderrLog) {
        let text = String::from_utf8_lossy(&self.bytes);
        let text = text.strip_suffix('\r').unwrap_or(&text);
        if self.truncated {
            log.push(&format!("{text}{TRUNCATED}"));
        } else {
            log.push(text);
        }
        self.bytes.clear();
        self.truncated = false;
    }

    /// The last line, when stderr ended without a newline.
    fn finish(&mut self, log: &StderrLog) {
        if !self.bytes.is_empty() || self.truncated {
            self.flush(log);
        }
    }
}

/// The spawned server as its watcher owns it.
struct Watched {
    child: Child,
    tree: ProcessTree,
    reader: JoinHandle<()>,
}

/// Own the child until it exits: on its own, or when asked to shut down
/// (wait the grace period, then kill). The exit is published once the
/// stderr reader has drained, so the log is complete.
async fn watch_process(
    mut watched: Watched,
    shutdown: oneshot::Receiver<Duration>,
    exit: watch::Sender<Option<ProcessExit>>,
) {
    let Watched { child, tree, .. } = &mut watched;
    let ended = tokio::select! {
        status = child.wait() => ProcessExit { code: status.ok().and_then(|s| s.code()), killed: false },
        grace = shutdown => stop(child, tree, grace.unwrap_or(SHUTDOWN_GRACE)).await,
    };
    watched.tree.reap();
    let _ = tokio::time::timeout(STDERR_DRAIN, watched.reader).await;
    let _ = exit.send(Some(ended));
}

async fn stop(child: &mut Child, tree: &mut ProcessTree, grace: Duration) -> ProcessExit {
    if let Ok(status) = tokio::time::timeout(grace, child.wait()).await {
        return ProcessExit {
            code: status.ok().and_then(|s| s.code()),
            killed: false,
        };
    }
    tree.kill().await;
    let _ = child.kill().await;
    ProcessExit {
        code: None,
        killed: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_keeps_the_latest_lines_redacted() {
        let mut redactor = Redactor::new();
        redactor.add_value("token", "s3cr3t-value");
        let log = StderrLog::new(redactor);
        for i in 0..(STDERR_KEEP + 3) {
            log.push(&format!("line {i} s3cr3t-value"));
        }
        let lines = log.lines();
        assert_eq!(lines.len(), STDERR_KEEP);
        assert!(lines[0].starts_with("line 3 "), "{}", lines[0]);
        assert!(lines.iter().all(|l| !l.contains("s3cr3t-value")));
        let tail = log.tail(STDERR_TAIL);
        assert_eq!(tail.len(), STDERR_TAIL);
        assert!(tail[STDERR_TAIL - 1].starts_with(&format!("line {} ", STDERR_KEEP + 2)));
    }

    #[test]
    fn an_endless_stderr_line_is_cut_and_the_next_one_kept() {
        let log = StderrLog::new(Redactor::new());
        let mut line = LineBuffer::default();
        let flood = vec![b'x'; 1 << 20];
        for _ in 0..64 {
            line.feed(&flood, &log);
            assert!(line.bytes.len() <= STDERR_LINE_MAX);
        }
        line.feed(b"\r\nnext\npartial", &log);
        line.finish(&log);
        let lines = log.lines();
        assert_eq!(
            lines.len(),
            3,
            "{:?}",
            lines.iter().map(String::len).collect::<Vec<_>>()
        );
        assert_eq!(lines[0].len(), STDERR_LINE_MAX + TRUNCATED.len());
        assert!(lines[0].ends_with(TRUNCATED));
        assert_eq!(lines[1], "next");
        assert_eq!(lines[2], "partial");
    }

    #[test]
    fn lines_split_across_reads_are_joined() {
        let log = StderrLog::new(Redactor::new());
        let mut line = LineBuffer::default();
        line.feed(b"warn", &log);
        line.feed(b"ing: a\r", &log);
        line.feed(b"\n", &log);
        line.finish(&log);
        assert_eq!(log.lines(), vec!["warning: a".to_owned()]);
    }

    #[test]
    fn the_grace_period_is_five_seconds() {
        assert_eq!(SHUTDOWN_GRACE, Duration::from_secs(5));
        assert_eq!(STDERR_TAIL, 50);
    }
}
