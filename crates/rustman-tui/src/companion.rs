//! Finding, starting and greeting the viz companion (`rustman-viz`), shared
//! by every producer: the TUI's response hand-off ([`crate::viz`]) and
//! `rustman bench --viz` ([`crate::bench_viz`]).
//!
//! The companion is looked for the way `visual-dashboards` defines it
//! (`RUSTMAN_VIZ`, then `PATH`, then next to the running executable),
//! started with piped stdin and stdout, and greeted with the `rustman-viz/1`
//! handshake. What is sent after the handshake is the producer's business.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rustman_viz_protocol::{
    FrameError, Hello, HelloAck, LineDecoder, Message, PROTOCOL, VULKAN_UNAVAILABLE, encode_line,
    is_compatible,
};

/// The companion's executable name.
pub const COMPANION: &str = if cfg!(windows) {
    "rustman-viz.exe"
} else {
    "rustman-viz"
};

/// The environment variable that names the companion explicitly.
pub const OVERRIDE_VAR: &str = "RUSTMAN_VIZ";

/// How long the companion has to answer `hello`.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a companion that closed its stdout has to exit before the
/// handshake gives up waiting for its exit code.
const EXIT_WAIT: Duration = Duration::from_secs(2);

/// The companion's exit code when graphics cannot start.
const EXIT_NO_GRAPHICS: i32 = 3;

/// Where the companion is looked for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    /// `RUSTMAN_VIZ`, when set and not empty.
    pub override_path: Option<PathBuf>,
    /// The `PATH` value.
    pub path: Option<OsString>,
    /// The directory of the running `rustman` executable.
    pub exe_dir: Option<PathBuf>,
}

impl Discovery {
    /// The process environment and executable.
    pub fn from_env() -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf));
        Self {
            override_path: std::env::var_os(OVERRIDE_VAR)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            path: std::env::var_os("PATH"),
            exe_dir,
        }
    }

    /// The companion to start. An override that is not an executable file
    /// is an error naming it; nothing else is tried then.
    pub fn find(&self) -> Result<PathBuf, VizError> {
        if let Some(path) = &self.override_path {
            return if is_executable(path) {
                Ok(path.clone())
            } else {
                Err(VizError::NotFound(format!(
                    "{OVERRIDE_VAR}={} is not an executable file",
                    path.display()
                )))
            };
        }
        let on_path = self
            .path
            .iter()
            .flat_map(std::env::split_paths)
            .map(|dir| dir.join(COMPANION));
        let sibling = self.exe_dir.iter().map(|dir| dir.join(COMPANION));
        on_path
            .chain(sibling)
            .find(|c| is_executable(c))
            .ok_or_else(|| {
                VizError::NotFound(format!(
                    "no {COMPANION} on PATH or next to rustman; set {OVERRIDE_VAR}"
                ))
            })
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Why the companion could not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VizError {
    #[error("Viz companion not found: {0}")]
    NotFound(String),
    #[error("Viz companion speaks {theirs}, rustman needs {PROTOCOL}; upgrade rustman-viz")]
    Rejected { theirs: String },
    #[error("Graphics unavailable (Vulkan)")]
    Graphics,
    #[error("Viz companion failed: {0}")]
    Failed(String),
}

/// A companion that accepted the handshake.
#[derive(Debug)]
pub struct Companion {
    pub child: Child,
    /// Where the producer writes its messages.
    pub stdin: ChildStdin,
    /// The companion's messages after `hello_ack`, read on a thread of
    /// their own (which keeps draining the pipe if this is dropped).
    pub replies: mpsc::Receiver<Result<Message, FrameError>>,
    /// The companion's answer: its name and views.
    pub ack: HelloAck,
}

/// Find, start and greet the companion. A companion that does not accept
/// the handshake is killed.
pub fn connect(discovery: &Discovery) -> Result<Companion, VizError> {
    let program = discovery.find()?;
    let mut child = rustman_viz_client::spawn(
        Command::new(&program)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .map_err(|e| VizError::Failed(format!("cannot start {}: {e}", program.display())))?;
    match greet(&mut child) {
        Ok((stdin, replies, ack)) => Ok(Companion {
            child,
            stdin,
            replies,
            ack,
        }),
        Err(e) => {
            let _ = child.kill();
            detach(child);
            Err(e)
        }
    }
}

/// Wait for the child in the background so it never lingers as a zombie.
pub fn detach(mut child: Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

type Greeted = (
    ChildStdin,
    mpsc::Receiver<Result<Message, FrameError>>,
    HelloAck,
);

fn greet(child: &mut Child) -> Result<Greeted, VizError> {
    let mut stdin = child.stdin.take().ok_or_else(|| failed("no stdin"))?;
    let stdout = child.stdout.take().ok_or_else(|| failed("no stdout"))?;
    let replies = read_messages(stdout);
    let producer = format!("rustman {}", env!("CARGO_PKG_VERSION"));
    // A companion that exits at once closes the pipe; its reply (or exit
    // code) below says why, so a failed write is not the error to report.
    let _ = write_message(&mut stdin, &Message::Hello(Hello::new(producer)));
    let ack = match replies.recv_timeout(HANDSHAKE_TIMEOUT) {
        Ok(reply) => handshake(reply)?,
        Err(mpsc::RecvTimeoutError::Disconnected) => return Err(exited(child)),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            return Err(failed("no reply to hello within 10 s"));
        }
    };
    Ok((stdin, replies, ack))
}

/// The companion's messages, on a thread of its own (it keeps draining the
/// pipe after the handshake). The protocol's decoder frames them, so a line
/// longer than its limit is skipped instead of buffered.
fn read_messages(
    mut stdout: std::process::ChildStdout,
) -> mpsc::Receiver<Result<Message, FrameError>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut decoder = LineDecoder::new();
        let mut chunk = vec![0; 64 * 1024];
        while let Ok(n @ 1..) = stdout.read(&mut chunk) {
            for message in decoder.push(&chunk[..n]) {
                // The receiver may be gone after the handshake: keep draining.
                let _ = tx.send(message);
            }
        }
    });
    rx
}

/// The companion's `hello_ack`, or why it refused.
pub(crate) fn handshake(reply: Result<Message, FrameError>) -> Result<HelloAck, VizError> {
    let message = reply.map_err(|e| failed(&format!("bad reply to hello: {e}")))?;
    match message {
        Message::HelloAck(ack) if is_compatible(&ack.protocol) => Ok(ack),
        Message::HelloAck(ack) => Err(VizError::Rejected {
            theirs: ack.protocol,
        }),
        Message::HelloReject(reject) => Err(VizError::Rejected {
            theirs: reject.supported.join(", "),
        }),
        Message::Error(e) if e.code == VULKAN_UNAVAILABLE => Err(VizError::Graphics),
        Message::Error(e) => Err(failed(&e.message)),
        other => Err(failed(&format!(
            "unexpected reply {}",
            other.kind().unwrap_or("unknown")
        ))),
    }
}

/// Why a companion closed its stdout before answering. One that keeps
/// running is not waited for past [`EXIT_WAIT`] (the caller kills it).
fn exited(child: &mut Child) -> VizError {
    let Some(status) = wait_for_exit(child, EXIT_WAIT) else {
        return failed("closed its output without answering hello");
    };
    match status.code() {
        Some(EXIT_NO_GRAPHICS) => VizError::Graphics,
        Some(code) => failed(&format!("exited with code {code} before the handshake")),
        None => failed("exited before the handshake"),
    }
}

/// The child's exit status, if it exits within `limit`.
fn wait_for_exit(child: &mut Child, limit: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => return None,
        }
    }
}

pub(crate) fn failed(message: &str) -> VizError {
    VizError::Failed(message.to_owned())
}

/// Write one message line.
pub fn write_message(out: &mut impl Write, message: &Message) -> Result<(), VizError> {
    let line = encode_line(message).map_err(|e| failed(&e.to_string()))?;
    write_line(out, &line)
}

/// Write one encoded line.
pub fn write_line(out: &mut impl Write, line: &str) -> Result<(), VizError> {
    out.write_all(line.as_bytes())
        .and_then(|()| out.flush())
        .map_err(|e| failed(&format!("cannot write to the companion: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_executable(path: &Path) {
        std::fs::write(path, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn the_override_wins_over_path() {
        let tmp = tempfile::tempdir().unwrap();
        let (opt, bin) = (tmp.path().join("opt"), tmp.path().join("bin"));
        std::fs::create_dir_all(&opt).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        make_executable(&opt.join(COMPANION));
        make_executable(&bin.join(COMPANION));
        let discovery = Discovery {
            override_path: Some(opt.join(COMPANION)),
            path: Some(bin.clone().into_os_string()),
            exe_dir: None,
        };
        assert_eq!(discovery.find(), Ok(opt.join(COMPANION)));
        let discovery = Discovery {
            override_path: None,
            ..discovery
        };
        assert_eq!(discovery.find(), Ok(bin.join(COMPANION)));
    }

    #[test]
    fn the_sibling_is_used_last() {
        let tmp = tempfile::tempdir().unwrap();
        make_executable(&tmp.path().join(COMPANION));
        let discovery = Discovery {
            override_path: None,
            path: Some(OsString::new()),
            exe_dir: Some(tmp.path().to_path_buf()),
        };
        assert_eq!(discovery.find(), Ok(tmp.path().join(COMPANION)));
    }

    #[test]
    fn an_invalid_override_names_the_path_and_does_not_fall_back() {
        let tmp = tempfile::tempdir().unwrap();
        make_executable(&tmp.path().join(COMPANION));
        let nope = tmp.path().join("nope").join(COMPANION);
        let discovery = Discovery {
            override_path: Some(nope.clone()),
            path: Some(tmp.path().as_os_str().to_owned()),
            exe_dir: Some(tmp.path().to_path_buf()),
        };
        let err = discovery.find().unwrap_err();
        assert!(
            err.to_string().contains(&nope.display().to_string()),
            "{err}"
        );
        assert!(err.to_string().starts_with("Viz companion not found"));
    }

    #[test]
    fn nothing_found_is_not_found() {
        let err = Discovery::default().find().unwrap_err();
        assert!(matches!(err, VizError::NotFound(_)), "{err}");
    }

    #[test]
    fn handshake_replies() {
        let handshake = |line: &str| handshake(rustman_viz_protocol::decode_line(line));
        let ack = r#"{"type":"hello_ack","protocol":"rustman-viz/1","viewer":"stub 1","views":["bench"]}"#;
        let accepted = handshake(ack).unwrap();
        assert_eq!(accepted.viewer, "stub 1");
        assert_eq!(accepted.views, [rustman_viz_protocol::View::Bench]);
        let reject = r#"{"type":"hello_reject","supported":["rustman-viz/2"]}"#;
        let err = handshake(reject).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Viz companion speaks rustman-viz/2, rustman needs rustman-viz/1; upgrade rustman-viz"
        );
        let vulkan = r#"{"type":"error","code":"vulkan_unavailable","message":"no loader"}"#;
        assert_eq!(handshake(vulkan), Err(VizError::Graphics));
        assert!(matches!(handshake("not json"), Err(VizError::Failed(_))));
    }
}
