//! The raw wire writer: the one place in Rustman that writes hand-built
//! HTTP/1.1 bytes. `hyper` refuses to send invalid framing, so wire faults
//! are written here, over a stream from `rustman-http`'s connector
//! ([`rustman_http::open_raw_stream`]: DNS, connect timeout, tunnels, TLS
//! with ALPN `http/1.1`).
//!
//! The writer never parses a response beyond its status line and the
//! connection state: real traffic always goes through `hyper`. Every size
//! here is a fixed constant; nothing a request file says changes them.
//! Callers reach it only through [`super::run`], which needs the stress
//! gate's [`super::WireConsent`].

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{WireFamily, WireOutcome};

/// The size of the `oversized-headers` header value: 70 KiB.
pub const OVERSIZED_HEADER: usize = 70 * 1024;
/// The number of headers of `many-headers`.
pub const MANY_HEADERS: usize = 1000;
/// The longest status line read.
const MAX_STATUS_LINE: usize = 1024;
/// One byte per this interval on a `slow-body` connection.
pub const SLOW_BYTE_INTERVAL: Duration = Duration::from_secs(1);

/// The bytes of one `family` request to `host` (`host[:port]`), or `None`
/// for `slow-body`, which is written over time by [`slow_body`].
pub fn request_bytes(family: WireFamily, host: &str) -> Option<Vec<u8>> {
    let head = |method: &str, extra: &str| {
        format!(
            "{method} / HTTP/1.1\r\nHost: {host}\r\nUser-Agent: rustman-stress\r\n{extra}Connection: close\r\n\r\n"
        )
    };
    let text = match family {
        WireFamily::OversizedHeaders => head(
            "GET",
            &format!("X-Rustman-Fill: {}\r\n", "a".repeat(OVERSIZED_HEADER)),
        ),
        WireFamily::ManyHeaders => {
            let headers: String = (0..MANY_HEADERS)
                .map(|i| format!("X-Rustman-{i}: v\r\n"))
                .collect();
            head("GET", &headers)
        }
        // A body that is complete by `Content-Length` but no valid chunk:
        // only a server that frames by `Content-Length` despite
        // `Transfer-Encoding` (the request-smuggling desync) accepts it.
        WireFamily::ClTeConflict => {
            head(
                "POST",
                "Content-Length: 4\r\nTransfer-Encoding: chunked\r\n",
            ) + "wxyz"
        }
        WireFamily::InvalidChunked => {
            head("POST", "Transfer-Encoding: chunked\r\n") + "zz\r\nhello\r\n0\r\n\r\n"
        }
        WireFamily::InvalidRequestLine => {
            format!("GARBAGE /\u{7f} HTTX/9.9 extra\r\nHost: {host}\r\nConnection: close\r\n\r\n")
        }
        WireFamily::SlowBody => return None,
    };
    Some(text.into_bytes())
}

/// Write `bytes` and read the answer's status line within `timeout`.
pub async fn exchange<S>(mut stream: S, bytes: &[u8], timeout: Duration) -> WireOutcome
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let written = tokio::time::timeout(timeout, stream.write_all(bytes)).await;
    if written.is_err() {
        return WireOutcome::TimedOut;
    }
    // A server may answer (431, 400) and close before reading everything:
    // read whatever came back even when the write failed.
    match tokio::time::timeout(timeout, status_line(&mut stream)).await {
        Ok(outcome) => outcome,
        Err(_) => WireOutcome::TimedOut,
    }
}

/// The status of the first line read, or how the connection ended.
async fn status_line<S: AsyncRead + Unpin>(stream: &mut S) -> WireOutcome {
    let mut line = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) if line.is_empty() => return WireOutcome::Closed,
            Ok(0) => return parse_status(&line),
            Ok(n) => line.extend_from_slice(&buf[..n]),
            Err(_) if line.is_empty() => return WireOutcome::Reset,
            Err(_) => return parse_status(&line),
        }
        if line.windows(2).any(|w| w == b"\r\n") || line.len() >= MAX_STATUS_LINE {
            return parse_status(&line);
        }
    }
}

/// `HTTP/1.x NNN ...` to its status.
pub fn parse_status(line: &[u8]) -> WireOutcome {
    let text = String::from_utf8_lossy(line);
    let mut parts = text.split_whitespace();
    let version = parts.next().unwrap_or_default();
    let code = parts.next().and_then(|c| c.parse::<u16>().ok());
    match code {
        Some(code) if version.starts_with("HTTP/1.") && (100..=599).contains(&code) => {
            WireOutcome::Status(code)
        }
        _ => WireOutcome::Unreadable,
    }
}

/// A `slow-body` connection: a request announcing a body, then one byte
/// per [`SLOW_BYTE_INTERVAL`] for at most `hold`. Returns when the server
/// ended the connection (an answer, a close or a reset), else `None`
/// after `hold` (or on `stop`).
pub async fn slow_body<S>(
    stream: S,
    host: &str,
    hold: Duration,
    stop: &CancellationToken,
) -> Option<Duration>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let started = Instant::now();
    let announced = hold.as_secs() + 16;
    let head = format!(
        "POST / HTTP/1.1\r\nHost: {host}\r\nUser-Agent: rustman-stress\r\nContent-Type: text/plain\r\nContent-Length: {announced}\r\n\r\n"
    );
    let (mut reader, mut writer) = tokio::io::split(stream);
    if writer.write_all(head.as_bytes()).await.is_err() {
        return Some(started.elapsed());
    }
    let deadline = started + hold;
    let mut ticks = tokio::time::interval_at(started + SLOW_BYTE_INTERVAL, SLOW_BYTE_INTERVAL);
    let mut buf = [0u8; 64];
    loop {
        tokio::select! {
            () = stop.cancelled() => return None,
            () = tokio::time::sleep_until(deadline) => return None,
            _ = reader.read(&mut buf) => return Some(started.elapsed()),
            _ = ticks.tick() => {
                if writer.write_all(b"a").await.is_err() {
                    return Some(started.elapsed());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(family: WireFamily) -> String {
        String::from_utf8(request_bytes(family, "h:1").unwrap()).unwrap()
    }

    #[test]
    fn requests_carry_their_fault_and_nothing_else() {
        let conflict = text(WireFamily::ClTeConflict);
        assert!(conflict.starts_with("POST / HTTP/1.1\r\nHost: h:1\r\n"));
        assert!(conflict.contains("Content-Length: 4\r\nTransfer-Encoding: chunked\r\n"));
        assert!(conflict.ends_with("\r\n\r\nwxyz"));
        let chunked = text(WireFamily::InvalidChunked);
        assert!(chunked.ends_with("\r\n\r\nzz\r\nhello\r\n0\r\n\r\n"));
        assert!(text(WireFamily::InvalidRequestLine).starts_with("GARBAGE /\u{7f} HTTX/9.9"));
        let oversized = text(WireFamily::OversizedHeaders);
        assert!(oversized.len() > OVERSIZED_HEADER && oversized.len() < OVERSIZED_HEADER + 200);
        let many = text(WireFamily::ManyHeaders);
        assert_eq!(many.matches("X-Rustman-").count(), MANY_HEADERS);
        assert_eq!(request_bytes(WireFamily::SlowBody, "h"), None);
    }

    #[test]
    fn status_lines_parse_or_are_unreadable() {
        assert_eq!(
            parse_status(b"HTTP/1.1 431 Too Large\r\n"),
            WireOutcome::Status(431)
        );
        assert_eq!(parse_status(b"HTTP/1.0 200 OK"), WireOutcome::Status(200));
        assert_eq!(parse_status(b"SSH-2.0-x\r\n"), WireOutcome::Unreadable);
        assert_eq!(parse_status(b"HTTP/1.1 999 x"), WireOutcome::Unreadable);
    }

    #[tokio::test]
    async fn an_exchange_reads_the_status_or_the_close() {
        let (client, mut server) = tokio::io::duplex(1 << 20);
        let answer = tokio::spawn(async move {
            let mut buf = vec![0u8; 16];
            let _ = server.read(&mut buf).await;
            server
                .write_all(b"HTTP/1.1 400 Bad\r\nx: y\r\n\r\n")
                .await
                .unwrap();
        });
        let outcome = exchange(client, b"GET / HTTP/1.1\r\n\r\n", Duration::from_secs(10)).await;
        assert_eq!(outcome, WireOutcome::Status(400));
        answer.await.unwrap();
        let (client, server) = tokio::io::duplex(1 << 20);
        drop(server);
        let outcome = exchange(client, b"x", Duration::from_secs(10)).await;
        assert!(
            matches!(outcome, WireOutcome::Closed | WireOutcome::Reset),
            "{outcome:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_server_times_out() {
        let (client, _server) = tokio::io::duplex(1 << 20);
        let outcome = exchange(client, b"GET", Duration::from_secs(10)).await;
        assert_eq!(outcome, WireOutcome::TimedOut);
    }

    #[tokio::test(start_paused = true)]
    async fn slow_body_sends_a_byte_a_second_until_the_server_closes() {
        let (client, mut server) = tokio::io::duplex(1 << 20);
        let server_side = tokio::spawn(async move {
            let deadline = Instant::now() + Duration::from_millis(3500);
            let mut received = Vec::new();
            let mut buf = [0u8; 512];
            loop {
                tokio::select! {
                    n = server.read(&mut buf) => received.extend_from_slice(&buf[..n.unwrap()]),
                    () = tokio::time::sleep_until(deadline) => break,
                }
            }
            drop(server);
            received
        });
        let stop = CancellationToken::new();
        let closed = slow_body(client, "h", Duration::from_secs(60), &stop).await;
        let received = server_side.await.unwrap();
        let body_start = received.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        assert_eq!(&received[body_start..], b"aaa");
        let closed = closed.unwrap();
        assert!(
            closed >= Duration::from_secs(3) && closed < Duration::from_secs(5),
            "{closed:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn slow_body_stops_after_its_hold() {
        let (client, _server) = tokio::io::duplex(1 << 20);
        let stop = CancellationToken::new();
        assert_eq!(
            slow_body(client, "h", Duration::from_secs(5), &stop).await,
            None
        );
    }
}
