//! Hand-off to the viz companion (spec: Open in viz companion; design D10).
//!
//! `v` in the response viewer looks for `rustman-viz` the way
//! `visual-dashboards` defines it (`RUSTMAN_VIZ`, then `PATH`, then next to
//! the running executable), starts it, performs the `rustman-viz/1`
//! handshake and sends the current exchange to its response inspector. The
//! exchange is already redacted (the viewer only ever holds redacted data).
//! When no companion can be used, a toast says why and offers the HTML
//! report instead. The TUI depends on the protocol types only.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use tempfile::TempPath;

use rustman_viz_protocol::{
    BlobRef, Body, Close, FrameError, Header, Hello, InspectResponse, LineDecoder, Message,
    OpenView, PROTOCOL, VULKAN_UNAVAILABLE, View, encode_inspect, encode_line, is_compatible,
};

use crate::app::{Effect, State};
use crate::keymap::ActionId;
use crate::response::Source;
use crate::toast::Severity;

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

/// How long the companion has to read the exchange once it has answered
/// `hello` (a body can be up to 16 MiB on one line).
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a companion that closed its stdout has to exit before the
/// hand-off gives up waiting for its exit code.
const EXIT_WAIT: Duration = Duration::from_secs(2);

/// The companion's exit code when graphics cannot start.
const EXIT_NO_GRAPHICS: i32 = 3;

/// Bodies spilled for companions that are still running. Each is removed
/// when its companion exits, and [`cleanup`] removes the rest when the TUI
/// quits (the reaping threads do not outlive the process).
static SPILLED: Spills = Spills::new();

/// Spilled body files kept until their companion exits.
struct Spills(Mutex<Vec<TempPath>>);

impl Spills {
    const fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }

    fn keep(&self, path: TempPath) -> PathBuf {
        let kept = path.to_path_buf();
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(path);
        kept
    }

    /// Remove the file at `path` (its companion exited).
    fn release(&self, path: &Path) {
        let mut kept = self.0.lock().unwrap_or_else(|e| e.into_inner());
        kept.retain(|p| p.as_ref() as &Path != path);
    }

    fn clear(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

/// Remove the bodies spilled for companions that are still running. Called
/// when the TUI quits.
pub fn cleanup() {
    SPILLED.clear();
}

/// Removes the spilled bodies when dropped: when the TUI quits normally and
/// also when a panic unwinds out of it.
#[must_use = "the spilled bodies are removed when the guard is dropped"]
pub struct CleanupGuard(&'static Spills);

impl CleanupGuard {
    /// A guard over the bodies spilled for companions.
    pub fn new() -> Self {
        Self(&SPILLED)
    }
}

impl Default for CleanupGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        self.0.clear();
    }
}

/// Where the companion is looked for, read once when the TUI starts.
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

/// Why the exchange could not be shown in the companion.
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

/// One exchange to show, owned so it can be sent from another thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VizJob {
    pub discovery: Discovery,
    /// The window title: the request name.
    pub title: String,
    /// The request id.
    pub request: String,
    pub status: Option<u16>,
    pub content_type: Option<String>,
    /// Redacted response headers, in received order.
    pub headers: Vec<Header>,
    /// The redacted body.
    pub source: Option<Source>,
    pub binary: bool,
}

/// `viz.open`: send the current tab's exchange to the companion.
pub fn start(state: &mut State) -> Vec<Effect> {
    let Some(received) = state
        .tabs
        .active()
        .filter(|t| t.in_flight.is_none())
        .and_then(|t| t.exchange.as_deref())
    else {
        state.notify(
            Severity::Info,
            "Send the request to open its response in viz",
        );
        return Vec::new();
    };
    let record = &received.record;
    let response = record.exchange.as_ref().map(|e| &e.response);
    let headers = response.map_or_else(Vec::new, |r| {
        r.headers
            .iter()
            .map(|h| Header {
                name: h.name.clone(),
                value: h.value.clone(),
            })
            .collect()
    });
    let job = VizJob {
        discovery: state.viz.clone(),
        title: record.name.clone(),
        request: record.id.clone(),
        status: response.map(|r| r.status),
        content_type: received.content_type.clone(),
        headers,
        source: received.source.clone(),
        binary: response.is_some_and(|r| r.binary),
    };
    vec![Effect::Viz(Box::new(job))]
}

/// The hand-off finished: say where the exchange went, or why it did not
/// and how to get an HTML report instead.
pub fn on_done(state: &mut State, result: Result<String, VizError>) {
    match result {
        Ok(viewer) => state.notify(Severity::Success, format!("Opened in {viewer}")),
        Err(e) => {
            let keys = state.keymap.label(ActionId::ReportHtml);
            let offer = if keys.is_empty() {
                "Export HTML report from the command palette".to_owned()
            } else {
                format!("Export HTML report: {keys}")
            };
            state.notify(Severity::Warn, format!("{e}. {offer}"));
        }
    }
}

/// Find, start and feed the companion. Blocks until the handshake is done
/// and the exchange is written; the companion keeps running on its own.
pub fn open(job: &VizJob) -> Result<String, VizError> {
    open_with(job, WRITE_TIMEOUT)
}

fn open_with(job: &VizJob, write_timeout: Duration) -> Result<String, VizError> {
    let program = job.discovery.find()?;
    let mut child = Command::new(&program)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| VizError::Failed(format!("cannot start {}: {e}", program.display())))?;
    match session(&mut child, job, write_timeout) {
        Ok((viewer, spilled)) => {
            reap(child, spilled);
            Ok(viewer)
        }
        Err(e) => {
            let _ = child.kill();
            reap(child, None);
            Err(e)
        }
    }
}

/// Wait for the child in the background so it never lingers as a zombie,
/// then remove the body file it was given (the producer owns `blob_ref`
/// files; the companion reads them while it runs).
fn reap(mut child: Child, spilled: Option<TempPath>) {
    let kept = spilled.map(|path| SPILLED.keep(path));
    std::thread::spawn(move || {
        let _ = child.wait();
        if let Some(path) = kept {
            SPILLED.release(&path);
        }
    });
}

fn session(
    child: &mut Child,
    job: &VizJob,
    write_timeout: Duration,
) -> Result<(String, Option<TempPath>), VizError> {
    let mut stdin = child.stdin.take().ok_or_else(|| failed("no stdin"))?;
    let stdout = child.stdout.take().ok_or_else(|| failed("no stdout"))?;
    let replies = read_messages(stdout);
    let producer = format!("rustman {}", env!("CARGO_PKG_VERSION"));
    // A companion that exits at once closes the pipe; its reply (or exit
    // code) below says why, so a failed write is not the error to report.
    let _ = write(&mut stdin, &Message::Hello(Hello::new(producer)));
    let viewer = match replies.recv_timeout(HANDSHAKE_TIMEOUT) {
        Ok(reply) => handshake(reply)?,
        Err(mpsc::RecvTimeoutError::Disconnected) => return Err(exited(child)),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            return Err(failed("no reply to hello within 10 s"));
        }
    };
    let spilled = write_exchange(stdin, job.clone(), write_timeout)?;
    Ok((viewer, spilled))
}

/// Write the exchange on a thread of its own so a companion that stops
/// reading cannot hold the hand-off forever: after `timeout` it is an error
/// (the caller then kills the companion, which ends the blocked write).
fn write_exchange(
    mut stdin: ChildStdin,
    job: VizJob,
    timeout: Duration,
) -> Result<Option<TempPath>, VizError> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(send_exchange(&mut stdin, &job));
    });
    rx.recv_timeout(timeout).unwrap_or_else(|_| {
        Err(failed(&format!(
            "the companion did not read the exchange within {timeout:?}"
        )))
    })
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
                // The receiver is gone after the handshake: keep draining.
                let _ = tx.send(message);
            }
        }
    });
    rx
}

/// The viewer name from the reply to `hello`, or why it refused.
fn handshake(reply: Result<Message, FrameError>) -> Result<String, VizError> {
    let message = reply.map_err(|e| failed(&format!("bad reply to hello: {e}")))?;
    match message {
        Message::HelloAck(ack) if is_compatible(&ack.protocol) => Ok(ack.viewer),
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

fn failed(message: &str) -> VizError {
    VizError::Failed(message.to_owned())
}

fn write(stdin: &mut ChildStdin, message: &Message) -> Result<(), VizError> {
    let line = encode_line(message).map_err(|e| failed(&e.to_string()))?;
    write_line(stdin, &line)
}

fn write_line(stdin: &mut ChildStdin, line: &str) -> Result<(), VizError> {
    stdin
        .write_all(line.as_bytes())
        .and_then(|()| stdin.flush())
        .map_err(|e| failed(&format!("cannot write to the companion: {e}")))
}

/// `open_view`, `inspect_response` (or its `blob_ref`) and `close`. A body
/// spilled to a file is returned so it lives as long as the companion.
fn send_exchange(stdin: &mut ChildStdin, job: &VizJob) -> Result<Option<TempPath>, VizError> {
    let open = OpenView {
        view: View::ResponseInspector,
        title: Some(job.title.clone()),
    };
    write(stdin, &Message::OpenView(open))?;
    let inspect = InspectResponse {
        request: Some(job.request.clone()),
        status: job.status,
        content_type: job.content_type.clone(),
        headers: job.headers.clone(),
        selector: None,
        body: Body::Inline {
            text: body_text(job)?,
        },
    };
    let media_type = job.content_type.clone();
    let mut spilled = None;
    let line = encode_inspect(inspect, |text| {
        let (blob, path) = spill(text, media_type)?;
        spilled = Some(path);
        Ok::<_, std::io::Error>(blob)
    })
    .map_err(|e| failed(&format!("cannot spill the body: {e}")))?
    .map_err(|e| failed(&e.to_string()))?;
    write_line(stdin, &line)?;
    write(stdin, &Message::Close(Close::default()))?;
    Ok(spilled)
}

/// The redacted body as text (a binary body is left out).
fn body_text(job: &VizJob) -> Result<String, VizError> {
    let Some(source) = &job.source else {
        return Ok(String::new());
    };
    if job.binary {
        return Ok(String::new());
    }
    let bytes = source
        .read_all()
        .map_err(|e| failed(&format!("cannot read the body: {e}")))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Write a body too large for one line to a file the companion reads. The
/// file is removed when the returned path is dropped (after the companion
/// exits, or at once when the hand-off fails).
fn spill(text: &str, media_type: Option<String>) -> std::io::Result<(BlobRef, TempPath)> {
    let mut file = tempfile::Builder::new()
        .prefix("rustman-viz-")
        .suffix(".body")
        .tempfile()?;
    file.write_all(text.as_bytes())?;
    let path = file.into_temp_path();
    let blob = BlobRef {
        path: path.display().to_string(),
        bytes: text.len() as u64,
        media_type,
    };
    Ok((blob, path))
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

    #[cfg(unix)]
    const ACK: &str =
        r#"{"type":"hello_ack","protocol":"rustman-viz/1","viewer":"stub 1","views":[]}"#;

    /// A companion that answers `hello`, then runs `rest` (sh).
    #[cfg(unix)]
    fn companion(dir: &Path, rest: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("stub-viz");
        let text = format!("#!/bin/sh\nIFS= read -r hello\nprintf '%s\\n' '{ACK}'\n{rest}\n");
        std::fs::write(&script, text).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    #[cfg(unix)]
    fn job(program: PathBuf, body_len: usize) -> VizJob {
        VizJob {
            discovery: Discovery {
                override_path: Some(program),
                ..Discovery::default()
            },
            title: "Big".into(),
            request: "big".into(),
            status: Some(200),
            content_type: Some("text/plain".into()),
            headers: Vec::new(),
            source: Some(Source::Memory(vec![b'a'; body_len].into())),
            binary: false,
        }
    }

    #[cfg(unix)]
    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_companion_that_stops_reading_is_given_up_on() {
        let tmp = tempfile::tempdir().unwrap();
        let script = companion(tmp.path(), "exec sleep 60");
        // Larger than any pipe buffer, so the write blocks.
        let job = job(script, 4 << 20);
        let started = std::time::Instant::now();
        let err = open_with(&job, Duration::from_millis(500)).unwrap_err();
        assert!(
            err.to_string()
                .contains("the companion did not read the exchange within 500ms"),
            "{err}"
        );
        assert!(started.elapsed() < Duration::from_secs(15));
    }

    #[cfg(unix)]
    #[test]
    fn a_spilled_body_lives_until_the_companion_exits() {
        let tmp = tempfile::tempdir().unwrap();
        let seen = tmp.path().join("seen");
        let rest = format!(
            "cat > \"$0.log\"\npath=$(sed -n 's/.*\"path\":\"\\([^\"]*\\)\".*/\\1/p' \"$0.log\")\n\
             echo \"$path\" > {seen}.tmp\ntest -f \"$path\" && echo present >> {seen}.tmp\n\
             mv {seen}.tmp {seen}",
            seen = seen.display()
        );
        let script = companion(tmp.path(), &rest);
        let job = job(script, rustman_viz_protocol::MAX_LINE_BYTES + 1);
        assert_eq!(open(&job), Ok("stub 1".to_owned()));
        wait_until("the companion never finished", || seen.exists());
        let text = std::fs::read_to_string(&seen).unwrap();
        let mut lines = text.lines();
        let path = PathBuf::from(lines.next().unwrap());
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("rustman-viz-"),
            "{text}"
        );
        assert_eq!(lines.next(), Some("present"), "the companion could read it");
        wait_until("the spilled body was not removed", || !path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_companion_that_closes_stdout_but_keeps_running_is_not_waited_for() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("stub-viz");
        std::fs::write(&script, "#!/bin/sh\nexec >&- 2>&-\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        let err = open(&job(script, 10)).unwrap_err();
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
        let script = tmp.path().join("stub-viz");
        let text = "#!/bin/sh\nIFS= read -r hello\n\
                    head -c 17000000 /dev/zero | tr '\\0' x\necho\nexec sleep 60\n";
        std::fs::write(&script, text).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = open(&job(script, 10)).unwrap_err();
        assert!(err.to_string().contains("exceeds the"), "{err}");
    }

    #[test]
    fn spilled_bodies_are_removed_on_release_or_cleanup() {
        let spills = Spills::new();
        let (_, first) = spill("one", None).unwrap();
        let (_, second) = spill("two", None).unwrap();
        let first = spills.keep(first);
        let second = spills.keep(second);
        assert!(first.exists() && second.exists());
        spills.release(&first);
        assert!(!first.exists() && second.exists());
        spills.clear();
        assert!(!second.exists(), "a companion still running at quit");
    }

    #[test]
    fn a_panic_still_removes_spilled_bodies() {
        // Regression: cleanup only ran on the normal return path.
        static SPILLS: Spills = Spills::new();
        let (_, path) = spill("body", None).unwrap();
        let kept = SPILLS.keep(path);
        let unwound = std::panic::catch_unwind(|| {
            let _guard = CleanupGuard(&SPILLS);
            panic!("the UI loop panicked");
        });
        assert!(unwound.is_err());
        assert!(!kept.exists(), "the spilled body outlived the panic");
    }

    #[test]
    fn handshake_replies() {
        let handshake = |line: &str| handshake(rustman_viz_protocol::decode_line(line));
        let ack = r#"{"type":"hello_ack","protocol":"rustman-viz/1","viewer":"stub 1","views":[]}"#;
        assert_eq!(handshake(ack), Ok("stub 1".to_owned()));
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
