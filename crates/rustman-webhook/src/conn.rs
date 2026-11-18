//! One accepted connection: the I/O wrapper that keeps a copy of the
//! request head and tracks activity, the idle watchdog, and the mapping of
//! connection errors to logged rejections.
//!
//! HTTP/1 connections carry one request each (keep-alive is off, design
//! D1), so the first bytes of the connection are that request's head.

use std::error::Error as StdError;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant};

use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use crate::event::{Event, Rejection};
use crate::listener::Shared;
use crate::service;

/// How long a refused connection keeps reading (and discarding) what the
/// client still sends, so the client reads the error response instead of a
/// connection reset.
const LINGER: Duration = Duration::from_secs(2);

/// The reason logged and sent when a request head does not arrive in time.
const HEADER_TIMEOUT: &str = "request header timeout";

/// How an HTTP/2 connection starts; such a connection gets no HTTP/1 408.
const H2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// State shared by a connection's I/O wrapper, its requests and its watchdog.
#[derive(Debug)]
pub(crate) struct ConnState {
    pub remote: SocketAddr,
    /// The first bytes read: the request head of an HTTP/1 connection.
    tap: Mutex<Vec<u8>>,
    tap_cap: usize,
    linger: AtomicBool,
    /// A request head arrived (the header timeout no longer applies).
    requested: AtomicBool,
    /// The head did not arrive in time: answer 408 once hyper let go.
    head_timed_out: AtomicBool,
    started: Instant,
    last_activity_ms: AtomicU64,
    in_flight: AtomicUsize,
}

impl ConnState {
    pub fn new(remote: SocketAddr, tap_cap: usize) -> Self {
        Self {
            remote,
            tap: Mutex::new(Vec::new()),
            tap_cap,
            linger: AtomicBool::new(false),
            requested: AtomicBool::new(false),
            head_timed_out: AtomicBool::new(false),
            started: Instant::now(),
            last_activity_ms: AtomicU64::new(0),
            in_flight: AtomicUsize::new(0),
        }
    }

    fn touch(&self) {
        let now = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_activity_ms.store(now, Ordering::Relaxed);
    }

    fn record(&self, bytes: &[u8]) {
        self.touch();
        let mut tap = self
            .tap
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let room = self.tap_cap.saturating_sub(tap.len());
        tap.extend_from_slice(&bytes[..room.min(bytes.len())]);
    }

    /// Run `f` over the bytes read so far (up to the tap's capacity).
    pub fn with_tap<T>(&self, f: impl FnOnce(&[u8]) -> T) -> T {
        f(&self
            .tap
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    /// Keep draining the client's input briefly once the connection closes.
    pub fn linger(&self) {
        self.linger.store(true, Ordering::Relaxed);
    }

    fn idle_for(&self) -> Duration {
        let last = Duration::from_millis(self.last_activity_ms.load(Ordering::Relaxed));
        self.started.elapsed().saturating_sub(last)
    }

    /// Mark a request in flight until the guard drops.
    pub fn begin(self: &Arc<Self>) -> InFlight {
        self.requested.store(true, Ordering::Relaxed);
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        InFlight(Arc::clone(self))
    }
}

pub(crate) struct InFlight(Arc<ConnState>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::Relaxed);
        self.0.touch();
    }
}

/// The TCP stream, recording what is read into the connection's tap.
struct ConnIo {
    stream: Option<TcpStream>,
    state: Arc<ConnState>,
}

impl ConnIo {
    fn stream(&mut self) -> io::Result<Pin<&mut TcpStream>> {
        self.stream
            .as_mut()
            .map(Pin::new)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotConnected))
    }
}

impl AsyncRead for ConnIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        ready!(self.stream()?.poll_read(cx, buf))?;
        self.state.record(&buf.filled()[before..]);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for ConnIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.state.touch();
        self.stream()?.poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.stream()?.poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.stream()?.poll_shutdown(cx)
    }
}

impl Drop for ConnIo {
    fn drop(&mut self) {
        let timed_out = self.state.head_timed_out.load(Ordering::Relaxed);
        if !timed_out && !self.state.linger.load(Ordering::Relaxed) {
            return;
        }
        if let (Some(stream), Ok(runtime)) =
            (self.stream.take(), tokio::runtime::Handle::try_current())
        {
            runtime.spawn(async move {
                let mut stream = stream;
                if timed_out {
                    let reply = header_timeout_response();
                    let _ = tokio::time::timeout(LINGER, stream.write_all(reply.as_bytes())).await;
                }
                drain(stream).await;
            });
        }
    }
}

/// The 408 written when the request head did not arrive in time, shaped
/// like the listener's other refusals.
fn header_timeout_response() -> String {
    let body = serde_json::json!({ "error": HEADER_TIMEOUT, "status": 408 }).to_string();
    format!(
        "HTTP/1.1 408 Request Timeout\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

async fn drain(mut stream: TcpStream) {
    let mut sink = vec![0u8; 16 * 1024];
    let read_all = async { while matches!(stream.read(&mut sink).await, Ok(n) if n > 0) {} };
    let _ = tokio::time::timeout(LINGER, read_all).await;
}

/// How a connection is told to end: `drain` lets the request in flight
/// finish (hyper's graceful shutdown), `stop` closes it at once.
#[derive(Clone, Debug, Default)]
pub(crate) struct Stops {
    pub drain: CancellationToken,
    pub stop: CancellationToken,
}

impl Stops {
    /// Tokens cancelled with these, for one connection.
    pub fn child(&self) -> Self {
        Self {
            drain: self.drain.child_token(),
            stop: self.stop.child_token(),
        }
    }
}

/// Serve one accepted connection until it closes, the listener stops or
/// drains, or it stays idle past the limit.
pub(crate) async fn serve(
    stream: TcpStream,
    remote: SocketAddr,
    shared: Arc<Shared>,
    stops: Stops,
) {
    let state = Arc::new(ConnState::new(remote, shared.tap_cap()));
    let io = TokioIo::new(ConnIo {
        stream: Some(stream),
        state: Arc::clone(&state),
    });
    let service = {
        let (shared, state) = (Arc::clone(&shared), Arc::clone(&state));
        service_fn(move |req| service::handle(req, Arc::clone(&shared), Arc::clone(&state)))
    };
    let conn = shared.builder.serve_connection(io, service);
    tokio::pin!(conn);
    let result = tokio::select! {
        result = conn.as_mut() => result,
        () = head_deadline(&state, shared.limits.header_timeout) => {
            head_timed_out(&state, &shared);
            return;
        }
        () = idle(&state, shared.limits.idle_timeout) => {
            conn.as_mut().graceful_shutdown();
            conn.await
        }
        () = stops.drain.cancelled() => {
            conn.as_mut().graceful_shutdown();
            tokio::select! {
                result = conn.as_mut() => result,
                () = stops.stop.cancelled() => return,
            }
        }
        () = stops.stop.cancelled() => return,
    };
    if let Err(err) = result
        && let Some(rejection) = rejection(err.as_ref(), &state, shared.limits.max_headers)
    {
        shared.store.emit(Event::Rejected(rejection));
    }
}

/// Resolves when no request head arrived within `limit` of accepting the
/// connection; never once one did. Counting from the accept (not from the
/// first byte) also closes connections that send nothing.
async fn head_deadline(state: &ConnState, limit: Duration) {
    tokio::time::sleep_until((state.started + limit).into()).await;
    if state.requested.load(Ordering::Relaxed) {
        std::future::pending::<()>().await;
    }
}

/// The head did not arrive in time; the connection is about to close. A
/// partial HTTP/1 head gets a 408 and a log line; a connection that sent
/// nothing, or speaks HTTP/2, is just closed.
fn head_timed_out(state: &ConnState, shared: &Shared) {
    let (sent_bytes, h2) = state.with_tap(|tap| {
        let n = tap.len().min(H2_PREFACE.len());
        (!tap.is_empty(), tap[..n] == H2_PREFACE[..n])
    });
    if !sent_bytes || h2 {
        return;
    }
    state.head_timed_out.store(true, Ordering::Relaxed);
    shared.store.emit(Event::Rejected(Rejection {
        remote: state.remote,
        status: 408,
        reason: HEADER_TIMEOUT.to_owned(),
    }));
}

/// Resolves once the connection has had no traffic and no request in flight
/// for `limit`.
async fn idle(state: &ConnState, limit: Duration) {
    loop {
        let quiet = state.idle_for();
        if quiet >= limit && state.in_flight.load(Ordering::Relaxed) == 0 {
            return;
        }
        let wait = limit.saturating_sub(quiet).max(Duration::from_millis(50));
        tokio::time::sleep(wait).await;
    }
}

/// The rejection hyper answered for a connection error, if it was one.
fn rejection(
    err: &(dyn StdError + 'static),
    state: &ConnState,
    max_headers: usize,
) -> Option<Rejection> {
    let err = hyper_error(err)?;
    let (status, reason) = if err.is_parse_too_large() {
        (431, header_overflow_reason(state, max_headers))
    } else if err.is_parse() {
        (400, format!("malformed request: {err}"))
    } else {
        return None;
    };
    Some(Rejection {
        remote: state.remote,
        status,
        reason,
    })
}

fn hyper_error<'a>(err: &'a (dyn StdError + 'static)) -> Option<&'a hyper::Error> {
    let mut current = Some(err);
    while let Some(err) = current {
        if let Some(found) = err.downcast_ref::<hyper::Error>() {
            return Some(found);
        }
        current = err.source();
    }
    None
}

/// Why hyper refused a head as too large: more header lines than allowed
/// in what was read, or too many bytes.
fn header_overflow_reason(state: &ConnState, max_headers: usize) -> String {
    let lines = state.with_tap(|tap| tap.windows(2).filter(|w| w == b"\r\n").count());
    // The first line is the request line.
    if lines.saturating_sub(1) > max_headers {
        crate::service::TOO_MANY_HEADERS.to_owned()
    } else {
        crate::service::HEADERS_TOO_LARGE.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tap_keeps_only_its_capacity() {
        let state = ConnState::new("127.0.0.1:1".parse().unwrap(), 4);
        state.record(b"abc");
        state.record(b"def");
        assert_eq!(state.with_tap(<[u8]>::to_vec), b"abcd");
    }

    #[test]
    fn overflow_reason_counts_header_lines() {
        let state = ConnState::new("127.0.0.1:1".parse().unwrap(), 1024);
        state.record(b"GET / HTTP/1.1\r\na: 1\r\nb: 2\r\nc: 3\r\n");
        assert_eq!(header_overflow_reason(&state, 2), "too many header fields");
        assert_eq!(header_overflow_reason(&state, 3), "header fields too large");
    }
}
