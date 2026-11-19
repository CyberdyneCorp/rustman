//! A live session with the companion (design D2, D8; spec "Fault
//! isolation"): messages go through a bounded queue to a writer thread, so
//! sending never blocks the producer. When the companion stops reading,
//! the queue fills: progress messages are then dropped while findings and
//! final events are kept, and after 5 seconds of back-pressure the producer
//! detaches from the companion with a warning. A companion that exits or
//! crashes is noticed (broken pipe, or its exit) with the warning
//! `Viz companion exited`; the producer carries on either way.

use std::collections::VecDeque;
use std::io::Write;
use std::process::{Child, ChildStdin};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rustman_viz_protocol::{Close, Message, encode_line};

use crate::connect::Connection;

/// Shows a warning to the user (stderr for the CLI).
pub type Warn = Arc<dyn Fn(&str) + Send + Sync>;

/// The warning when the companion exits during a session.
pub const EXITED: &str = "Viz companion exited; continuing without visuals";

/// Limits of a stream.
#[derive(Debug, Clone, Copy)]
pub struct StreamOptions {
    /// Lines queued before progress messages are dropped.
    pub capacity: usize,
    /// How long the queue may stay full before the producer detaches.
    pub detach_after: Duration,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            capacity: 1024,
            detach_after: Duration::from_secs(5),
        }
    }
}

/// A live session; [`Stream::close`] (or dropping it) sends `close` and
/// lets the companion keep its windows open.
pub struct Stream {
    shared: Arc<Shared>,
    viewer: String,
    closed: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    warn: Warn,
    options: StreamOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Link {
    Live,
    /// The companion stopped reading; nothing more is sent.
    Detached,
    /// The companion exited or closed its stdin.
    Gone,
}

struct Queue {
    lines: VecDeque<String>,
    link: Link,
    /// `close` was queued; the writer ends once the queue is empty.
    closing: bool,
    writer_done: bool,
    /// Since when the queue has been full.
    pressure_since: Option<Instant>,
    dropped: u64,
    warned: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Stop talking to the companion, warning once.
    fn cut(&self, queue: &mut Queue, link: Link, warning: &str) {
        if queue.link != Link::Live {
            return;
        }
        queue.link = link;
        queue.lines.clear();
        if !queue.warned {
            queue.warned = true;
            (self.warn)(warning);
        }
        self.wake.notify_all();
    }
}

impl Stream {
    /// Stream to a companion that accepted the handshake.
    pub fn start(connection: Connection, options: StreamOptions, warn: Warn) -> Self {
        let Connection {
            child,
            stdin,
            viewer,
            ..
        } = connection;
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                lines: VecDeque::new(),
                link: Link::Live,
                closing: false,
                writer_done: false,
                pressure_since: None,
                dropped: 0,
                warned: false,
            }),
            wake: Condvar::new(),
            warn,
            options,
        });
        let writer = Arc::clone(&shared);
        std::thread::spawn(move || write_loop(&writer, stdin));
        let watcher = Arc::clone(&shared);
        std::thread::spawn(move || watch(&watcher, child));
        Self {
            shared,
            viewer,
            closed: false,
        }
    }

    /// The companion's name and version.
    pub fn viewer(&self) -> &str {
        &self.viewer
    }

    /// Whether messages still reach the companion.
    pub fn is_live(&self) -> bool {
        self.shared.lock().link == Link::Live
    }

    /// How many progress messages were dropped under back-pressure.
    pub fn dropped(&self) -> u64 {
        self.shared.lock().dropped
    }

    /// Queue `message` without blocking. Returns whether it was queued: a
    /// progress message is dropped while the queue is full, and nothing is
    /// queued once the companion is gone or detached.
    pub fn send(&self, message: &Message) -> bool {
        let Ok(line) = encode_line(message) else {
            return false;
        };
        let shared = &self.shared;
        let mut queue = shared.lock();
        if queue.link != Link::Live || queue.closing {
            return false;
        }
        if queue.lines.len() < shared.options.capacity {
            queue.pressure_since = None;
        } else {
            let since = *queue.pressure_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= shared.options.detach_after {
                let warning = format!(
                    "Viz companion stopped reading for {} s; detached from it, continuing without visuals",
                    shared.options.detach_after.as_secs()
                );
                shared.cut(&mut queue, Link::Detached, &warning);
                return false;
            }
            if !message.is_priority() {
                queue.dropped += 1;
                return false;
            }
        }
        queue.lines.push_back(line);
        shared.wake.notify_all();
        true
    }

    /// Send `close` and wait (at most the detach time) until everything
    /// queued is written. The companion keeps running with its windows.
    pub fn close(&mut self) {
        if std::mem::replace(&mut self.closed, true) {
            return;
        }
        let close = encode_line(&Message::Close(Close::default())).ok();
        let shared = &self.shared;
        let mut queue = shared.lock();
        if queue.link != Link::Live {
            return;
        }
        queue.lines.extend(close);
        queue.closing = true;
        shared.wake.notify_all();
        let deadline = Instant::now() + shared.options.detach_after;
        while !queue.writer_done && queue.link == Link::Live {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                let warning = format!(
                    "Viz companion did not read the last updates within {} s; detached from it",
                    shared.options.detach_after.as_secs()
                );
                shared.cut(&mut queue, Link::Detached, &warning);
                return;
            }
            queue = shared
                .wake
                .wait_timeout(queue, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.close();
    }
}

/// Write queued lines until `close` is written, the companion is gone or
/// the producer detached. Dropping `stdin` at the end tells the companion
/// the producer is done.
fn write_loop(shared: &Shared, mut stdin: ChildStdin) {
    loop {
        let line = {
            let mut queue = shared.lock();
            loop {
                if queue.link != Link::Live {
                    break None;
                }
                if let Some(line) = queue.lines.pop_front() {
                    break Some(line);
                }
                if queue.closing {
                    break None;
                }
                queue = shared.wake.wait(queue).unwrap_or_else(|e| e.into_inner());
            }
        };
        let Some(line) = line else { break };
        if stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .is_err()
        {
            let mut queue = shared.lock();
            shared.cut(&mut queue, Link::Gone, EXITED);
            break;
        }
    }
    drop(stdin);
    let mut queue = shared.lock();
    queue.writer_done = true;
    shared.wake.notify_all();
}

/// Wait for the companion; one that exits while the session is live is
/// reported. (After `close`, exiting is the user closing its windows.)
fn watch(shared: &Shared, mut child: Child) {
    let _ = child.wait();
    let mut queue = shared.lock();
    if !queue.closing {
        shared.cut(&mut queue, Link::Gone, EXITED);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::{Path, PathBuf};

    use rustman_viz_protocol::{
        Assertions, ExchangeCompleted, Finding, Outcome, RunFinished, RunState, RunSummary,
        Severity,
    };

    use super::*;
    use crate::{ConnectOptions, connect};

    const ACK: &str =
        r#"{"type":"hello_ack","protocol":"rustman-viz/1","viewer":"stub 1","views":[]}"#;

    /// A companion that answers `hello`, then runs `rest` (sh).
    fn companion(dir: &Path, rest: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("stub-viz");
        let text = format!("#!/bin/sh\nIFS= read -r hello\nprintf '%s\\n' '{ACK}'\n{rest}\n");
        std::fs::write(&script, text).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    fn warnings() -> (Warn, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let warn: Warn = Arc::new(move |w: &str| sink.lock().unwrap().push(w.to_owned()));
        (warn, seen)
    }

    fn start(program: &Path, options: StreamOptions) -> (Stream, Arc<Mutex<Vec<String>>>) {
        let connection = connect(program, &ConnectOptions::new("test")).unwrap();
        let (warn, seen) = warnings();
        (Stream::start(connection, options, warn), seen)
    }

    fn exchange(url_len: usize) -> Message {
        Message::ExchangeCompleted(Box::new(ExchangeCompleted {
            request: "users/get".into(),
            method: "GET".into(),
            url: format!("https://api.test/{}", "x".repeat(url_len)),
            outcome: Outcome::Passed,
            status: Some(200),
            duration_ms: 5,
            assertions: Assertions::default(),
            error: None,
            iteration: None,
        }))
    }

    fn finding() -> Message {
        Message::Finding(Finding {
            severity: Severity::High,
            operation: "POST /users".into(),
            title: "500 on empty body".into(),
            detail: None,
        })
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn messages_arrive_in_order_and_end_with_close() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("received");
        let program = companion(
            tmp.path(),
            &format!("cat > {log}.tmp\nmv {log}.tmp {log}", log = log.display()),
        );
        let (mut stream, warnings) = start(&program, StreamOptions::default());
        assert_eq!(stream.viewer(), "stub 1");
        assert!(stream.send(&exchange(1)));
        assert!(stream.send(&finding()));
        let finished = Message::RunFinished(RunFinished {
            state: RunState::Completed,
            summary: RunSummary::default(),
        });
        assert!(stream.send(&finished));
        stream.close();
        wait_until("the companion never saw stdin end", || log.exists());
        let text = std::fs::read_to_string(&log).unwrap();
        let kinds: Vec<_> = text
            .lines()
            .map(|l| {
                rustman_viz_protocol::decode_line(l)
                    .unwrap()
                    .kind()
                    .unwrap()
            })
            .collect();
        assert_eq!(
            kinds,
            ["exchange_completed", "finding", "run_finished", "close"]
        );
        assert!(warnings.lock().unwrap().is_empty());
    }

    #[test]
    fn a_full_queue_drops_progress_but_keeps_findings() {
        let tmp = tempfile::tempdir().unwrap();
        let program = companion(tmp.path(), "exec sleep 60");
        let options = StreamOptions {
            capacity: 4,
            detach_after: Duration::from_secs(60),
        };
        let (stream, _) = start(&program, options);
        // Larger than any pipe buffer: the writer blocks on it.
        assert!(stream.send(&exchange(4 << 20)));
        wait_until("the writer never took the first line", || {
            stream.shared.lock().lines.is_empty()
        });
        for _ in 0..4 {
            assert!(stream.send(&exchange(1)));
        }
        assert!(!stream.send(&exchange(1)), "progress is dropped");
        assert!(stream.send(&finding()), "findings are never dropped");
        assert_eq!(stream.dropped(), 1);
        assert_eq!(stream.shared.lock().lines.len(), 5);
        assert!(stream.is_live());
        stream.shared.lock().link = Link::Detached; // let drop return at once
    }

    #[test]
    fn a_companion_that_stops_reading_is_detached_without_blocking() {
        let tmp = tempfile::tempdir().unwrap();
        let program = companion(tmp.path(), "exec sleep 60");
        let options = StreamOptions {
            capacity: 8,
            detach_after: Duration::from_millis(500),
        };
        let (mut stream, warnings) = start(&program, options);
        let started = Instant::now();
        let mut slowest = Duration::ZERO;
        while stream.is_live() {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "never detached"
            );
            let before = Instant::now();
            stream.send(&exchange(16 * 1024));
            slowest = slowest.max(before.elapsed());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            // A blocking send would hang here; 1 s leaves room for a loaded runner.
            slowest < Duration::from_secs(1),
            "send blocked {slowest:?}"
        );
        stream.close();
        let warnings = warnings.lock().unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("detached"), "{warnings:?}");
        assert!(!stream.send(&finding()));
    }

    #[test]
    fn a_companion_that_exits_is_reported_once() {
        let tmp = tempfile::tempdir().unwrap();
        let program = companion(tmp.path(), "exit 0");
        let (mut stream, warnings) = start(&program, StreamOptions::default());
        wait_until("the exit was not noticed", || !stream.is_live());
        assert!(!stream.send(&exchange(1)));
        stream.close();
        assert_eq!(*warnings.lock().unwrap(), [EXITED.to_owned()]);
    }

    #[test]
    fn a_killed_companion_never_stops_the_producer() {
        let tmp = tempfile::tempdir().unwrap();
        let pid = tmp.path().join("pid");
        let program = companion(
            tmp.path(),
            // Written then renamed: the test must never read a half-written
            // pid (an empty read used to kill nothing on a slow runner).
            &format!(
                "echo $$ > {pid}.tmp\nmv {pid}.tmp {pid}\nexec cat > /dev/null",
                pid = pid.display()
            ),
        );
        let (mut stream, warnings) = start(&program, StreamOptions::default());
        wait_until("no pid", || pid.exists());
        let id = std::fs::read_to_string(&pid).unwrap();
        assert!(!id.trim().is_empty(), "no pid");
        let killed = std::process::Command::new("kill")
            .args(["-9", id.trim()])
            .status()
            .unwrap();
        assert!(killed.success(), "kill {id} failed");
        let started = Instant::now();
        for _ in 0..2000 {
            stream.send(&exchange(64));
        }
        stream.close();
        assert!(started.elapsed() < Duration::from_secs(10));
        wait_until("no warning", || !warnings.lock().unwrap().is_empty());
        assert_eq!(*warnings.lock().unwrap(), [EXITED.to_owned()]);
    }
}
