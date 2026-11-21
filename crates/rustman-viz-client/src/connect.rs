//! Starting the companion and the handshake (design D2): the producer sends
//! `hello`, the companion answers `hello_ack` or `hello_reject`, or writes
//! an `error` line `vulkan_unavailable` and exits with code 3.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rustman_viz_protocol::{
    FrameError, Hello, LineDecoder, Message, VULKAN_UNAVAILABLE, View, encode_line, is_compatible,
};

use crate::{EXIT_NO_GRAPHICS, VizError};

/// How long a companion that closed its stdout has to exit before the
/// producer gives up waiting for its exit code.
const EXIT_WAIT: Duration = Duration::from_secs(2);

/// How the companion is started.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// The `producer` of `hello`, e.g. `rustman 0.1.0`.
    pub producer: String,
    /// How long the companion has to answer `hello`.
    pub handshake_timeout: Duration,
    /// The file the companion's stderr is appended to (in the workspace's
    /// state directory); discarded when `None` or when it cannot be opened.
    pub log: Option<PathBuf>,
}

impl ConnectOptions {
    pub fn new(producer: impl Into<String>) -> Self {
        Self {
            producer: producer.into(),
            handshake_timeout: Duration::from_secs(10),
            log: None,
        }
    }
}

/// A companion that accepted the handshake.
#[derive(Debug)]
pub struct Connection {
    pub child: Child,
    pub stdin: ChildStdin,
    /// The companion's name and version, from `hello_ack`.
    pub viewer: String,
    /// The views it can open.
    pub views: Vec<View>,
}

/// Start `program` and perform the handshake. On any failure the companion
/// is stopped.
pub fn connect(program: &Path, options: &ConnectOptions) -> Result<Connection, VizError> {
    let stderr = options
        .log
        .as_deref()
        .and_then(|path| OpenOptions::new().create(true).append(true).open(path).ok())
        .map_or_else(Stdio::null, Stdio::from);
    let mut child = crate::spawn(
        Command::new(program)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr),
    )
    .map_err(|e| VizError::failed(format!("cannot start {}: {e}", program.display())))?;
    match handshake(&mut child, options) {
        Ok((stdin, viewer, views)) => Ok(Connection {
            child,
            stdin,
            viewer,
            views,
        }),
        Err(e) => {
            let _ = child.kill();
            reap(child);
            Err(e)
        }
    }
}

/// Wait for `child` in the background so it never lingers as a zombie.
pub(crate) fn reap(mut child: Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

fn handshake(
    child: &mut Child,
    options: &ConnectOptions,
) -> Result<(ChildStdin, String, Vec<View>), VizError> {
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| VizError::failed("no stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| VizError::failed("no stdout"))?;
    let replies = read_messages(stdout);
    // A companion that exits at once closes the pipe; its reply (or exit
    // code) below says why, so a failed write is not the error to report.
    let hello = Message::Hello(Hello::new(options.producer.clone()));
    if let Ok(line) = encode_line(&hello) {
        let _ = stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush());
    }
    match replies.recv_timeout(options.handshake_timeout) {
        Ok(reply) => {
            let (viewer, views) = accept(reply)?;
            Ok((stdin, viewer, views))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(exited(child)),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(VizError::failed(format!(
            "no reply to hello within {:?}",
            options.handshake_timeout
        ))),
    }
}

/// The companion's messages, on a thread of its own (it keeps draining the
/// pipe after the handshake, so the companion never blocks on stdout). The
/// protocol's decoder frames them, so a line longer than its limit is
/// skipped instead of buffered.
fn read_messages(mut stdout: ChildStdout) -> mpsc::Receiver<Result<Message, FrameError>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut decoder = LineDecoder::new();
        let mut chunk = vec![0; 64 * 1024];
        while let Ok(n @ 1..) = stdout.read(&mut chunk) {
            for message in decoder.push(&chunk[..n]) {
                // The receiver is gone after the handshake: keep draining.
                let _ = tx.send(message);
            }
        }
    });
    rx
}

/// The viewer and its views from the reply to `hello`, or why it refused.
pub(crate) fn accept(reply: Result<Message, FrameError>) -> Result<(String, Vec<View>), VizError> {
    let message = reply.map_err(|e| VizError::failed(format!("bad reply to hello: {e}")))?;
    match message {
        Message::HelloAck(ack) if is_compatible(&ack.protocol) => Ok((ack.viewer, ack.views)),
        Message::HelloAck(ack) => Err(VizError::Rejected {
            theirs: ack.protocol,
        }),
        Message::HelloReject(reject) => Err(VizError::Rejected {
            theirs: reject.supported.join(", "),
        }),
        Message::Error(e) if e.code == VULKAN_UNAVAILABLE => Err(VizError::Graphics),
        Message::Error(e) => Err(VizError::failed(e.message)),
        other => Err(VizError::failed(format!(
            "unexpected reply {}",
            other.kind().unwrap_or("unknown")
        ))),
    }
}

/// Why a companion closed its stdout before answering. One that keeps
/// running is not waited for past [`EXIT_WAIT`] (the caller kills it).
fn exited(child: &mut Child) -> VizError {
    let Some(status) = wait_for_exit(child, EXIT_WAIT) else {
        return VizError::failed("closed its output without answering hello");
    };
    match status.code() {
        Some(EXIT_NO_GRAPHICS) => VizError::Graphics,
        Some(code) => VizError::failed(format!("exited with code {code} before the handshake")),
        None => VizError::failed("exited before the handshake"),
    }
}

/// The child's exit status, if it exits within `limit`.
pub(crate) fn wait_for_exit(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_replies() {
        let accept = |line: &str| accept(rustman_viz_protocol::decode_line(line));
        let ack = r#"{"type":"hello_ack","protocol":"rustman-viz/1","viewer":"stub 1","views":["run_dashboard"]}"#;
        assert_eq!(
            accept(ack),
            Ok(("stub 1".to_owned(), vec![View::RunDashboard]))
        );
        let reject = r#"{"type":"hello_reject","supported":["rustman-viz/2"]}"#;
        let err = accept(reject).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Viz companion speaks rustman-viz/2, rustman needs rustman-viz/1; upgrade rustman-viz"
        );
        let vulkan = r#"{"type":"error","code":"vulkan_unavailable","message":"no loader"}"#;
        assert_eq!(accept(vulkan), Err(VizError::Graphics));
        assert!(matches!(accept("not json"), Err(VizError::Failed(_))));
    }

    #[cfg(unix)]
    fn script(dir: &Path, text: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("stub-viz");
        std::fs::write(&script, format!("#!/bin/sh\n{text}\n")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    #[cfg(unix)]
    #[test]
    fn no_graphics_is_reported_from_the_exit_code() {
        let tmp = tempfile::tempdir().unwrap();
        let program = script(
            tmp.path(),
            r#"echo '{"type":"error","code":"vulkan_unavailable","message":"no loader"}'; exit 3"#,
        );
        let err = connect(&program, &ConnectOptions::new("test")).unwrap_err();
        assert_eq!(err, VizError::Graphics);
        let program = script(tmp.path(), "exit 3");
        let err = connect(&program, &ConnectOptions::new("test")).unwrap_err();
        assert_eq!(err, VizError::Graphics);
    }

    #[cfg(unix)]
    #[test]
    fn a_companion_that_closes_stdout_but_keeps_running_is_not_waited_for() {
        let tmp = tempfile::tempdir().unwrap();
        let program = script(tmp.path(), "exec >&- 2>&-\nexec sleep 60");
        let started = Instant::now();
        let err = connect(&program, &ConnectOptions::new("test")).unwrap_err();
        assert!(
            err.to_string()
                .contains("closed its output without answering hello"),
            "{err}"
        );
        assert!(started.elapsed() < Duration::from_secs(8));
    }

    #[cfg(unix)]
    #[test]
    fn an_endless_line_from_the_companion_is_not_buffered() {
        // More than the protocol's 16 MiB line limit, with no newline until
        // the end: the decoder skips it and the handshake fails cleanly.
        let tmp = tempfile::tempdir().unwrap();
        let program = script(
            tmp.path(),
            "IFS= read -r hello\nhead -c 17000000 /dev/zero | tr '\\0' x\necho\nexec sleep 60",
        );
        let err = connect(&program, &ConnectOptions::new("test")).unwrap_err();
        assert!(err.to_string().contains("exceeds the"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn the_companion_stderr_goes_to_the_log() {
        let tmp = tempfile::tempdir().unwrap();
        let ack = r#"{"type":"hello_ack","protocol":"rustman-viz/1","viewer":"stub 1","views":[]}"#;
        let program = script(
            tmp.path(),
            &format!("echo 'companion log line' >&2\nIFS= read -r hello\necho '{ack}'"),
        );
        let log = tmp.path().join("viz.log");
        let options = ConnectOptions {
            log: Some(log.clone()),
            ..ConnectOptions::new("test")
        };
        let mut connection = connect(&program, &options).unwrap();
        assert_eq!(connection.viewer, "stub 1");
        let _ = connection.child.wait();
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("companion log line"), "{text}");
    }
}
