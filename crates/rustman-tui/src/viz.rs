//! Hand-off to the viz companion (spec: Open in viz companion; design D10).
//!
//! `v` in the response viewer looks for `rustman-viz` the way
//! `visual-dashboards` defines it (`RUSTMAN_VIZ`, then `PATH`, then next to
//! the running executable), starts it, performs the `rustman-viz/1`
//! handshake and sends the current exchange to its response inspector. The
//! exchange is already redacted (the viewer only ever holds redacted data).
//! When no companion can be used, a toast says why and offers the HTML
//! report instead. The TUI depends on the protocol types only.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use tempfile::TempPath;

use rustman_viz_client::{ConnectOptions, connect};
use rustman_viz_protocol::{
    BlobRef, Body, Close, Header, InspectResponse, Message, OpenView, View, encode_inspect,
    encode_line,
};

pub use rustman_viz_client::{COMPANION, Discovery, OVERRIDE_VAR, VizError};

use crate::app::{Effect, State};
use crate::keymap::ActionId;
use crate::response::Source;
use crate::toast::Severity;

/// How long the companion has to read the exchange once it has answered
/// `hello` (a body can be up to 16 MiB on one line).
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

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
    let program = job.discovery.locate()?;
    let options = ConnectOptions::new(format!("rustman {}", env!("CARGO_PKG_VERSION")));
    let connection = connect(&program, &options)?;
    let (mut child, stdin) = (connection.child, connection.stdin);
    match write_exchange(stdin, job.clone(), write_timeout) {
        Ok(spilled) => {
            reap(child, spilled);
            Ok(connection.viewer)
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

    #[cfg(unix)]
    const ACK: &str =
        r#"{"type":"hello_ack","protocol":"rustman-viz/1","viewer":"stub 1","views":[]}"#;

    /// A companion that answers `hello`, then runs `rest` (sh).
    #[cfg(unix)]
    fn companion(dir: &Path, rest: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("stub-viz");
        let text = format!(
            "#!/bin/sh\n[ \"$1\" = --protocol-check ] && {{ echo rustman-viz/1; exit 0; }}\n\
             IFS= read -r hello\nprintf '%s\\n' '{ACK}'\n{rest}\n"
        );
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
}
