//! Local Anvil nodes (evm-toolkit "Anvil fork management", design D8):
//! `anvil` found on `PATH` and run as a child process killed when its
//! owner drops it, a fork of an environment's chain, and the temporary
//! `fork-<chain>` environment that points at it.
//!
//! On Unix Anvil runs in a process group of its own, so a terminal's
//! Ctrl+C reaches only Rustman, which then removes the environment and
//! stops the node in that order. An exit that skips destructors
//! (`std::process::exit` on a second interrupt) stops running nodes first
//! with [`kill_all_forks`]. An owner killed outright (SIGKILL) cannot stop
//! Anvil or remove the file; the file records Anvil's process id, so the
//! next fork removes files whose Anvil is gone
//! ([`remove_stale_environments`]).
//!
//! Anvil's stdout and stderr are discarded: its banner lists the private
//! keys of its development accounts, and its errors may quote the fork
//! URL, which can hold an API key.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use rustman_core::environment::{ENVIRONMENTS_DIR, Environment, is_valid_name};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};
use tokio::sync::oneshot;

use crate::chain::{ChainInfo, ChainRegistry};
use crate::error::EvmError;

/// The error when `anvil` is not on `PATH` (exit code 2).
pub const ANVIL_NOT_FOUND: &str = "anvil not found; install Foundry (https://getfoundry.sh)";

/// Where a fork listens unless told otherwise.
pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 8545;

/// How long a node may take to listen: a fork first reads its upstream.
pub const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// The first line of a temporary environment file.
pub const TEMP_MARKER: &str = "# Temporary: written by `rustman evm fork`, removed when it exits.";

/// The program name: Foundry installs a native `anvil.exe` on Windows. A
/// `.cmd`/`.bat` wrapper is never run, since killing it would leave the
/// real node behind.
const ANVIL_PROGRAM: &str = if cfg!(windows) { "anvil.exe" } else { "anvil" };

/// The `anvil` program in the directories of `path` (a `PATH` value).
pub fn find_anvil(path: Option<&OsStr>) -> Result<PathBuf, EvmError> {
    path.into_iter()
        .flat_map(std::env::split_paths)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(ANVIL_PROGRAM))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| EvmError::config(ANVIL_NOT_FOUND))
}

/// How to run Anvil.
#[derive(Clone, Debug)]
pub struct AnvilOptions {
    pub program: PathBuf,
    pub host: String,
    pub port: u16,
    /// `--fork-url`: the upstream endpoint. It may hold an API key: it is
    /// never printed, but Anvil only takes it as an argument, so it shows
    /// in the process list to other local users while the fork runs.
    pub fork_url: Option<String>,
    /// `--fork-block-number`.
    pub fork_block: Option<u64>,
    /// More arguments, such as `--block-time 1`.
    pub extra: Vec<String>,
    pub ready_timeout: Duration,
}

impl AnvilOptions {
    pub fn new(program: PathBuf) -> Self {
        Self {
            program,
            host: DEFAULT_HOST.to_owned(),
            port: DEFAULT_PORT,
            fork_url: None,
            fork_block: None,
            extra: Vec::new(),
            ready_timeout: READY_TIMEOUT,
        }
    }

    /// The command-line arguments.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "--host".to_owned(),
            self.host.clone(),
            "--port".to_owned(),
            self.port.to_string(),
        ];
        if let Some(url) = &self.fork_url {
            args.extend(["--fork-url".to_owned(), url.clone()]);
        }
        if let Some(block) = self.fork_block {
            args.extend(["--fork-block-number".to_owned(), block.to_string()]);
        }
        args.extend(self.extra.iter().cloned());
        args
    }
}

/// Read Anvil's stdout: the first `Listening on` line is sent back, and the
/// rest is drained (and dropped) so Anvil never blocks on a full pipe.
fn watch_listening(stdout: ChildStdout) -> oneshot::Receiver<String> {
    let (found, listening) = oneshot::channel();
    tokio::spawn(async move {
        let mut found = Some(found);
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.contains(LISTENING)
                && let Some(found) = found.take()
            {
                let _ = found.send(line);
            }
        }
    });
    listening
}

/// What Anvil prints once its server is bound.
const LISTENING: &str = "Listening on";

/// The port in a `Listening on <host>:<port>` line.
fn listening_port(line: &str, host: &str) -> Option<u16> {
    let (_, rest) = line.split_once(&format!("{host}:"))?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// A running Anvil node, stopped when dropped.
#[derive(Debug)]
pub struct Anvil {
    child: Child,
    host: String,
    port: u16,
}

impl Anvil {
    /// Start Anvil and wait until it says it listens. Readiness is Anvil's
    /// own `Listening on <address>` line, not a connection to the port: a
    /// process that took the port between the check below and Anvil's
    /// start is never mistaken for the fork. Port 0 lets Anvil pick a free
    /// port, read back from that line, so no port is reserved and released
    /// beforehand.
    pub async fn start(options: &AnvilOptions) -> Result<Self, EvmError> {
        let address = format!("{}:{}", options.host, options.port);
        if options.port != 0 && std::net::TcpListener::bind(&address).is_err() {
            return Err(EvmError::config(format!("{address} is already in use")));
        }
        let mut command = Command::new(&options.program);
        command
            .args(options.args())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|e| {
            EvmError::config(format!("cannot start {}: {e}", options.program.display()))
        })?;
        if let Some(pid) = child.id() {
            live().insert(pid);
        }
        let listening = child.stdout.take().map(watch_listening);
        let mut anvil = Self {
            child,
            host: options.host.clone(),
            port: options.port,
        };
        let line = anvil
            .wait_ready(&address, listening, options.ready_timeout)
            .await?;
        anvil.port = listening_port(&line, &options.host)
            .filter(|port| options.port == 0 || *port == options.port)
            .ok_or_else(|| {
                EvmError::config(format!("anvil listens elsewhere than {address}: {line}"))
            })?;
        Ok(anvil)
    }

    /// Wait for the `Listening on` line and return it.
    async fn wait_ready(
        &mut self,
        address: &str,
        listening: Option<oneshot::Receiver<String>>,
        timeout: Duration,
    ) -> Result<String, EvmError> {
        let not_listening = || {
            EvmError::config(format!(
                "anvil did not listen on {address} within {}s",
                timeout.as_secs()
            ))
        };
        let exited = |status: std::process::ExitStatus| {
            EvmError::config(format!(
                "anvil exited ({status}) before listening on {address}"
            ))
        };
        let listening = listening.ok_or_else(not_listening)?;
        let line = tokio::select! {
            line = listening => line,
            status = self.wait_exit() => return Err(exited(status)),
            () = tokio::time::sleep(timeout) => return Err(not_listening()),
        };
        match line {
            Ok(line) => Ok(line),
            // Its stdout closed without the line: Anvil is exiting.
            Err(_) => match tokio::time::timeout(timeout, self.wait_exit()).await {
                Ok(status) => Err(exited(status)),
                Err(_) => Err(not_listening()),
            },
        }
    }

    /// Wait for the process to exit on its own (an upstream that went
    /// away, a crash) and return its status.
    pub async fn wait_exit(&mut self) -> std::process::ExitStatus {
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// The node's process id while it runs.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// The port the node listens on (the one Anvil picked for port 0).
    pub fn port(&self) -> u16 {
        self.port
    }

    /// `http://<host>:<port>`.
    pub fn url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    /// `ws://<host>:<port>`.
    pub fn ws_url(&self) -> String {
        format!("ws://{}:{}", self.host, self.port)
    }

    /// Stop the node and wait for it to exit.
    pub async fn stop(mut self) {
        self.forget();
        let _ = self.child.kill().await;
    }

    /// No longer list the node for [`kill_all_forks`]: it is being stopped.
    fn forget(&self) {
        if let Some(pid) = self.child.id() {
            live().remove(&pid);
        }
    }
}

impl Drop for Anvil {
    /// The child itself is killed on drop (`kill_on_drop`).
    fn drop(&mut self) {
        self.forget();
    }
}

/// The process ids of the nodes started and not stopped yet.
static LIVE: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

fn live() -> std::sync::MutexGuard<'static, BTreeSet<u32>> {
    LIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Kill every Anvil this process started and has not stopped yet. For
/// exits that skip destructors, such as `std::process::exit`. On Windows
/// it does nothing: there Anvil shares the console's Ctrl+C.
pub fn kill_all_forks() {
    kill(std::mem::take(&mut *live()));
}

fn kill(pids: BTreeSet<u32>) {
    #[cfg(unix)]
    for pid in pids {
        use rustix::process::{Pid, Signal, kill_process};
        if let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) {
            let _ = kill_process(pid, Signal::KILL);
        }
    }
    #[cfg(not(unix))]
    drop(pids);
}

/// The environment a fork of `chain` (as the user named it) reads from:
/// the environment named `chain`, else the first pinned to the chain
/// `chain` names (a chain name, alias or id). The chain forked is the one
/// the node reports.
pub fn fork_source<'a>(
    environments: &'a [Environment],
    registry: &ChainRegistry,
    chain: &str,
) -> Result<&'a Environment, EvmError> {
    if let Some(env) = environments.iter().find(|e| e.name == chain) {
        return Ok(env);
    }
    let info: ChainInfo = registry
        .find(chain)
        .ok_or_else(|| EvmError::config(format!("unknown chain or environment '{chain}'")))?;
    environments
        .iter()
        .find(|e| e.chain_id == Some(info.id))
        .ok_or_else(|| {
            EvmError::config(format!(
                "no environment for {}; add one with 'chain_id: {}' and an 'rpc' variable",
                info.label(),
                info.id
            ))
        })
}

/// `fork-<chain>`, or `fork-<id>` when the name is not a valid
/// environment name.
pub fn fork_environment_name(chain: &str, chain_id: u64) -> String {
    let name = format!("fork-{}", chain.trim().to_ascii_lowercase());
    if is_valid_name(&name) {
        name
    } else {
        format!("fork-{chain_id}")
    }
}

/// An environment file that exists while this value lives.
#[derive(Debug)]
pub struct TempEnvironment {
    name: String,
    path: PathBuf,
}

impl TempEnvironment {
    /// Write `environments/<name>.yaml` under `root` with `chain_id`, an
    /// `rpc` variable and, as a comment, the process id of the Anvil it
    /// points at. A file of that name that this command did not write is
    /// never replaced.
    pub fn write(
        root: &Path,
        name: &str,
        url: &str,
        chain_id: u64,
        pid: Option<u32>,
    ) -> Result<Self, EvmError> {
        let dir = root.join(ENVIRONMENTS_DIR);
        let path = dir.join(format!("{name}.yaml"));
        if let Ok(existing) = fs::read_to_string(&path)
            && !existing.starts_with(TEMP_MARKER)
        {
            return Err(EvmError::config(format!(
                "environment '{name}' already exists ({}); remove it or fork another chain",
                path.display()
            )));
        }
        let pid_line = pid
            .map(|pid| format!("{PID_PREFIX}{pid}\n"))
            .unwrap_or_default();
        let text =
            format!("{TEMP_MARKER}\n{pid_line}chain_id: {chain_id}\nvariables:\n  rpc: {url}\n");
        fs::create_dir_all(&dir)
            .and_then(|()| fs::write(&path, text))
            .map_err(|e| EvmError::config(format!("{}: {e}", path.display())))?;
        Ok(Self {
            name: name.to_owned(),
            path,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempEnvironment {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// The comment line of a temporary environment naming its Anvil's pid.
const PID_PREFIX: &str = "# anvil pid: ";

/// Remove the temporary environments under `root` left behind by a fork
/// whose owner was killed outright: files this command wrote whose
/// recorded Anvil process no longer exists. Files without a pid, and
/// every file where liveness cannot be checked (Windows), are kept.
/// Returns the names removed.
pub fn remove_stale_environments(root: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(root.join(ENVIRONMENTS_DIR)) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for path in entries.flatten().map(|entry| entry.path()) {
        let Some(name) = path.file_stem().and_then(OsStr::to_str) else {
            continue;
        };
        let is_fork = name.starts_with("fork-") && path.extension() == Some(OsStr::new("yaml"));
        let stale = is_fork
            && fs::read_to_string(&path)
                .ok()
                .and_then(|text| recorded_pid(&text))
                .is_some_and(process_gone);
        if stale && fs::remove_file(&path).is_ok() {
            removed.push(name.to_owned());
        }
    }
    removed.sort();
    removed
}

/// The Anvil pid of a temporary environment's text (`None` for any other
/// file).
fn recorded_pid(text: &str) -> Option<u32> {
    let mut lines = text.lines();
    if lines.next() != Some(TEMP_MARKER) {
        return None;
    }
    lines.next()?.strip_prefix(PID_PREFIX)?.trim().parse().ok()
}

/// Whether no process `pid` exists (`false` when unsure).
#[cfg(unix)]
fn process_gone(pid: u32) -> bool {
    use rustix::io::Errno;
    use rustix::process::{Pid, test_kill_process};
    let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return false;
    };
    test_kill_process(pid) == Err(Errno::SRCH)
}

#[cfg(not(unix))]
fn process_gone(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_path_has_no_anvil() {
        let err = find_anvil(Some(OsStr::new(""))).unwrap_err();
        assert_eq!(err.to_string(), ANVIL_NOT_FOUND);
        assert_eq!(err.exit_code(), 2);
        assert!(find_anvil(None).is_err());
    }

    #[test]
    fn anvil_is_found_in_path_order() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) { "anvil.exe" } else { "anvil" };
        fs::write(second.path().join(name), "").unwrap();
        let path = std::env::join_paths([first.path(), second.path()]).unwrap();
        assert_eq!(find_anvil(Some(&path)).unwrap(), second.path().join(name));
    }

    /// A stand-in `anvil`: a shell script running `body`.
    #[cfg(unix)]
    fn fake_anvil(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("anvil");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn readiness_is_anvils_listening_line_not_an_open_port() {
        // Regression: readiness was any connection to the port, so a
        // process that took the port after the free-port check passed for
        // the fork.
        let dir = tempfile::tempdir().unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let says = fake_anvil(
            dir.path(),
            &format!("echo 'Listening on 127.0.0.1:{port}'; sleep 30"),
        );
        let mut options = AnvilOptions::new(says);
        options.port = port;
        options.ready_timeout = Duration::from_secs(20);
        let anvil = Anvil::start(&options).await.unwrap();
        anvil.stop().await;
        let silent_dir = tempfile::tempdir().unwrap();
        options.program = fake_anvil(silent_dir.path(), "sleep 30");
        options.ready_timeout = Duration::from_millis(300);
        let err = Anvil::start(&options).await.unwrap_err();
        assert!(err.to_string().contains("did not listen"), "{err}");
        let failing_dir = tempfile::tempdir().unwrap();
        options.program = fake_anvil(failing_dir.path(), "exit 3");
        options.ready_timeout = Duration::from_secs(20);
        let err = Anvil::start(&options).await.unwrap_err();
        assert!(err.to_string().contains("anvil exited"), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn port_zero_reads_the_port_anvil_picked() {
        // Regression: tests reserved a port, released it and passed it to
        // Anvil, so a parallel test could take it in between.
        let dir = tempfile::tempdir().unwrap();
        let program = fake_anvil(dir.path(), "echo 'Listening on 127.0.0.1:41234'; sleep 30");
        let mut options = AnvilOptions::new(program);
        options.port = 0;
        options.ready_timeout = Duration::from_secs(20);
        assert_eq!(options.args()[3], "0");
        let anvil = Anvil::start(&options).await.unwrap();
        assert_eq!(anvil.port(), 41234);
        assert_eq!(anvil.url(), "http://127.0.0.1:41234");
        anvil.stop().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_node_that_exits_on_its_own_is_seen() {
        let dir = tempfile::tempdir().unwrap();
        // It stays up a moment after the line: one that exits at once can
        // be seen exiting before its line is read (a start failure).
        let program = fake_anvil(
            dir.path(),
            "echo 'Listening on 127.0.0.1:41235'; sleep 2; exit 7",
        );
        let mut options = AnvilOptions::new(program);
        options.port = 0;
        options.ready_timeout = Duration::from_secs(20);
        let mut anvil = Anvil::start(&options).await.unwrap();
        let status = tokio::time::timeout(Duration::from_secs(20), anvil.wait_exit())
            .await
            .unwrap();
        assert_eq!(status.code(), Some(7));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn listed_nodes_are_killed_without_their_owner() {
        // Regression: a second interrupt exits without destructors, and
        // Anvil, in a process group of its own, kept running.
        let dir = tempfile::tempdir().unwrap();
        let program = fake_anvil(dir.path(), "echo 'Listening on 127.0.0.1:41236'; sleep 30");
        let mut options = AnvilOptions::new(program);
        options.port = 0;
        options.ready_timeout = Duration::from_secs(20);
        let mut anvil = Anvil::start(&options).await.unwrap();
        let pid = anvil.pid().unwrap();
        // What `kill_all_forks` does, for this node only: other tests'
        // nodes run in parallel.
        assert!(live().remove(&pid));
        kill(BTreeSet::from([pid]));
        let status = tokio::time::timeout(Duration::from_secs(20), anvil.wait_exit())
            .await
            .unwrap();
        assert!(!status.success());
        assert!(!live().contains(&pid));
    }

    #[test]
    fn listening_lines_give_their_port() {
        let line = "Listening on 127.0.0.1:8545";
        assert_eq!(listening_port(line, "127.0.0.1"), Some(8545));
        assert_eq!(listening_port(line, "0.0.0.0"), None);
        assert_eq!(listening_port("Listening on 127.0.0.1:", "127.0.0.1"), None);
    }

    #[test]
    fn only_the_native_program_is_found() {
        // Regression: on Windows an anvil.cmd/anvil.bat wrapper was run,
        // and killing it left the real node running.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("anvil.cmd"), "").unwrap();
        fs::write(dir.path().join("anvil.bat"), "").unwrap();
        let path = std::env::join_paths([dir.path()]).unwrap();
        assert!(find_anvil(Some(&path)).is_err());
    }

    #[test]
    fn fork_arguments() {
        let mut options = AnvilOptions::new(PathBuf::from("anvil"));
        options.fork_url = Some("https://node.example/KEY".into());
        options.fork_block = Some(20_000_000);
        assert_eq!(
            options.args(),
            [
                "--host",
                "127.0.0.1",
                "--port",
                "8545",
                "--fork-url",
                "https://node.example/KEY",
                "--fork-block-number",
                "20000000"
            ]
        );
    }

    #[test]
    fn environments_are_chosen_by_name_then_chain_id() {
        let env = |name: &str, chain_id: Option<u64>| Environment {
            name: name.to_owned(),
            chain_id,
            ..Environment::default()
        };
        let all = [
            env("local", None),
            env("prod", Some(1)),
            env("sepolia", Some(11_155_111)),
        ];
        let registry = ChainRegistry::built_in();
        let found = fork_source(&all, &registry, "mainnet").unwrap();
        assert_eq!(found.name, "prod");
        let found = fork_source(&all, &registry, "1").unwrap();
        assert_eq!(found.name, "prod");
        let found = fork_source(&all, &registry, "sepolia").unwrap();
        assert_eq!(found.name, "sepolia");
        // Regression: an environment whose name is no chain name was
        // refused as an unknown chain.
        let found = fork_source(&all, &registry, "local").unwrap();
        assert_eq!(found.name, "local");
        let err = fork_source(&all, &registry, "base").unwrap_err();
        assert_eq!(
            err.to_string(),
            "no environment for Base (8453); add one with 'chain_id: 8453' and an 'rpc' variable"
        );
        let err = fork_source(&all, &registry, "nowhere").unwrap_err();
        assert_eq!(err.to_string(), "unknown chain or environment 'nowhere'");
        assert_eq!(fork_environment_name("mainnet", 1), "fork-mainnet");
        assert_eq!(fork_environment_name("my chain", 9), "fork-9");
    }

    #[test]
    fn temporary_environments_are_removed_and_never_replace_a_real_one() {
        let root = tempfile::tempdir().unwrap();
        let temp = TempEnvironment::write(
            root.path(),
            "fork-mainnet",
            "http://127.0.0.1:1",
            1,
            Some(42),
        )
        .unwrap();
        let text = fs::read_to_string(temp.path()).unwrap();
        let env = rustman_core::environment::parse(temp.path(), "fork-mainnet", &text).unwrap();
        assert_eq!(env.chain_id, Some(1));
        assert_eq!(
            env.variables.get("rpc").unwrap().to_string(),
            "http://127.0.0.1:1"
        );
        let path = temp.path().to_owned();
        drop(temp);
        assert!(!path.exists());
        fs::write(&path, "variables: {}\n").unwrap();
        let err =
            TempEnvironment::write(root.path(), "fork-mainnet", "http://x", 1, None).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "variables: {}\n");
    }

    #[cfg(unix)]
    #[test]
    fn environments_of_a_killed_fork_are_removed_on_the_next_start() {
        // Regression: a SIGKILLed `rustman evm fork` left fork-<chain>.yaml
        // pointing at a dead node for good.
        let root = tempfile::tempdir().unwrap();
        let mut gone = std::process::Command::new("true").spawn().unwrap();
        let gone_pid = gone.id();
        gone.wait().unwrap();
        let stale = TempEnvironment::write(root.path(), "fork-dead", "http://x", 1, Some(gone_pid))
            .unwrap();
        let live = TempEnvironment::write(
            root.path(),
            "fork-live",
            "http://y",
            1,
            Some(std::process::id()),
        )
        .unwrap();
        let unknown = TempEnvironment::write(root.path(), "fork-old", "http://z", 1, None).unwrap();
        let (stale_path, live_path, unknown_path) = (
            stale.path().to_owned(),
            live.path().to_owned(),
            unknown.path().to_owned(),
        );
        std::mem::forget((stale, live, unknown));
        let user = root.path().join(ENVIRONMENTS_DIR).join("fork-mine.yaml");
        fs::write(&user, format!("{PID_PREFIX}{gone_pid}\nvariables: {{}}\n")).unwrap();
        assert_eq!(remove_stale_environments(root.path()), ["fork-dead"]);
        assert!(!stale_path.exists());
        assert!(live_path.exists() && unknown_path.exists() && user.exists());
    }
}
