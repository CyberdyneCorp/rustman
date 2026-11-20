//! Servers for the stress wire-fault tests.
//!
//! - [`WireServer`]: a deliberately simple raw HTTP/1.1 server whose
//!   leniency is configured: it can accept broken framing (conflicting
//!   `Content-Length` and `Transfer-Encoding`, bad chunk sizes, garbage
//!   request lines), drop connections whose head is too large without a
//!   response, and time out slow bodies, or never. It counts the
//!   connections open at once.
//! - [`HyperWireServer`]: a hyper HTTP/1.1 server that reads every body
//!   before answering (`400` when the body is broken).

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// The largest head the raw server reads.
const MAX_HEAD: usize = 512 * 1024;

/// How a [`WireServer`] behaves.
#[derive(Clone, Copy, Debug, Default)]
pub struct WireConfig {
    /// Answer `200` to broken framing instead of `400`.
    pub lax_framing: bool,
    /// Close without a response once the head grows past this many bytes.
    pub drop_heads_over: Option<usize>,
    /// Answer `408` and close when a body is incomplete after this long;
    /// `None` waits for as long as the client sends.
    pub body_timeout: Option<Duration>,
}

#[derive(Default)]
struct Counters {
    open: AtomicUsize,
    max_open: AtomicUsize,
    connections: AtomicU64,
}

/// Decrements the open count when a connection ends.
struct Open(Arc<Counters>);

impl Drop for Open {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A raw HTTP/1.1 server on `127.0.0.1`; stops when dropped.
pub struct WireServer {
    addr: SocketAddr,
    counters: Arc<Counters>,
    task: JoinHandle<()>,
}

impl Drop for WireServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl WireServer {
    pub async fn start(config: WireConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let counters = Arc::new(Counters::default());
        let shared = counters.clone();
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                shared.connections.fetch_add(1, Ordering::SeqCst);
                let now = shared.open.fetch_add(1, Ordering::SeqCst) + 1;
                shared.max_open.fetch_max(now, Ordering::SeqCst);
                let open = Open(shared.clone());
                tokio::spawn(async move {
                    let _open = open;
                    serve(tcp, config).await;
                });
            }
        });
        Self {
            addr,
            counters,
            task,
        }
    }

    /// `http://127.0.0.1:<port>`.
    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.addr.port())
    }

    /// The most connections open at once so far.
    pub fn max_open(&self) -> usize {
        self.counters.max_open.load(Ordering::SeqCst)
    }

    /// Connections accepted so far.
    pub fn connections(&self) -> u64 {
        self.counters.connections.load(Ordering::SeqCst)
    }
}

async fn serve(mut tcp: TcpStream, config: WireConfig) {
    let Some((head, rest)) = read_head(&mut tcp, config.drop_heads_over).await else {
        // Too large, or the client went away: close without a response.
        return;
    };
    let status = judge(&head, rest, &mut tcp, config).await;
    if let Some(status) = status {
        let reply =
            format!("HTTP/1.1 {status}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        let _ = tcp.write_all(reply.as_bytes()).await;
        let _ = tcp.shutdown().await;
    }
}

/// The head (up to the blank line) and the bytes read after it; `None`
/// when the head grew past `limit` or the connection ended first.
async fn read_head(tcp: &mut TcpStream, limit: Option<usize>) -> Option<(String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let end = buf.windows(4).position(|w| w == b"\r\n\r\n");
        if end.map_or(buf.len(), |e| e + 4) > limit.unwrap_or(MAX_HEAD).min(MAX_HEAD) {
            return None;
        }
        if let Some(end) = end {
            let rest = buf.split_off(end + 4);
            return Some((String::from_utf8_lossy(&buf).into_owned(), rest));
        }
        match tcp.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

/// The status to answer, or `None` to close without one.
async fn judge(
    head: &str,
    rest: Vec<u8>,
    tcp: &mut TcpStream,
    config: WireConfig,
) -> Option<&'static str> {
    let ok = "200 OK";
    let refused = if config.lax_framing {
        ok
    } else {
        "400 Bad Request"
    };
    let mut lines = head.split("\r\n");
    if !valid_request_line(lines.next().unwrap_or_default()) {
        return Some(refused);
    }
    let header = |name: &str| {
        head.split("\r\n").skip(1).find_map(|line| {
            let (n, v) = line.split_once(':')?;
            n.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_owned())
        })
    };
    let length = header("content-length");
    let chunked = header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked"));
    if length.is_some() && chunked {
        return Some(refused);
    }
    if chunked {
        let size = String::from_utf8_lossy(&rest);
        let first = size.split("\r\n").next().unwrap_or_default();
        return Some(if usize::from_str_radix(first.trim(), 16).is_ok() {
            ok
        } else {
            refused
        });
    }
    let length: usize = length.and_then(|l| l.parse().ok()).unwrap_or(0);
    if read_body(tcp, rest.len(), length, config.body_timeout).await {
        Some(ok)
    } else {
        Some("408 Request Timeout")
    }
}

fn valid_request_line(line: &str) -> bool {
    let parts: Vec<&str> = line.split(' ').collect();
    matches!(parts.as_slice(), [method, target, version]
        if !method.is_empty()
            && method.bytes().all(|b| b.is_ascii_uppercase())
            && target.starts_with('/')
            && target.bytes().all(|b| b.is_ascii_graphic())
            && (*version == "HTTP/1.1" || *version == "HTTP/1.0"))
}

/// Read the rest of a `length`-byte body (`have` bytes already read);
/// `false` when `timeout` elapsed first. A client that closes ends it.
async fn read_body(
    tcp: &mut TcpStream,
    have: usize,
    length: usize,
    timeout: Option<Duration>,
) -> bool {
    let read = async {
        let mut have = have;
        let mut chunk = [0u8; 4096];
        while have < length {
            match tcp.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => have += n,
            }
        }
    };
    match timeout {
        Some(limit) => tokio::time::timeout(limit, read).await.is_ok(),
        None => {
            read.await;
            true
        }
    }
}

/// A hyper HTTP/1.1 server that reads each body before answering `200`,
/// or `400` when the body cannot be read; stops when dropped.
pub struct HyperWireServer {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl Drop for HyperWireServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl HyperWireServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let service = service_fn(read_then_answer);
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(tcp), service)
                        .await;
                });
            }
        });
        Self { addr, task }
    }

    /// `http://127.0.0.1:<port>`.
    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.addr.port())
    }
}

async fn read_then_answer(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let status = match req.into_body().collect().await {
        Ok(_) => StatusCode::OK,
        Err(_) => StatusCode::BAD_REQUEST,
    };
    Ok(Response::builder()
        .status(status)
        .body(Full::new(Bytes::from_static(b"ok")))
        .unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_lines_are_checked() {
        assert!(valid_request_line("GET / HTTP/1.1"));
        assert!(valid_request_line("POST /a?b=1 HTTP/1.0"));
        assert!(!valid_request_line("GARBAGE /\u{7f} HTTX/9.9 extra"));
        assert!(!valid_request_line("get / HTTP/1.1"));
    }
}
