//! The test server's routes.
//!
//! | Route | Response |
//! |-------|----------|
//! | `/echo` | JSON with the method, path, version, headers and body received |
//! | `/delay/<ms>` | `200 done` after `<ms>` milliseconds |
//! | `/slow-body/<ms>` | headers at once, the body after `<ms>` milliseconds |
//! | `/status/<code>` | an empty response with that status |
//! | `/redirect/<code>?to=<url>[&pad=<n>]` | a redirect with that status to `<url>` (and an `<n>`-byte body) |
//! | `/loop` | `302` back to `/loop` |
//! | `/bytes/<n>` | `<n>` bytes streamed in 64 KiB chunks |
//! | `/compressed/<gzip\|br\|zstd\|deflate>` | [`COMPRESSED_TEXT`] encoded |
//! | `/zeros/<br\|zstd>/<n>` | `<n>` zero bytes, encoded (a decompression bomb) |
//! | `/stall-after/<n>` | `<n>` of the promised `2n` bytes, then nothing |
//! | `/png` | a small PNG, byte for byte [`PNG`] |
//! | `/login` | `200` with `Set-Cookie: sid=abc; Path=/` |

use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use async_compression::tokio::bufread::{BrotliEncoder, GzipEncoder, ZlibEncoder, ZstdEncoder};
use bytes::Bytes;
use http::{Request, Response, StatusCode, header};
use http_body::{Body, Frame, SizeHint};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use tokio::io::AsyncReadExt;
use tokio::time::Sleep;

pub type TestBody = BoxBody<Bytes, Infallible>;

/// The plain text behind every `/compressed/*` route.
pub const COMPRESSED_TEXT: &str = "rustman decompresses this text. rustman decompresses this text.";

/// A PNG signature and IHDR chunk start: bytes that are not valid UTF-8.
pub const PNG: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D', b'R', 0xff,
    0xfe, 0x00, 0x80,
];

const CHUNK: usize = 64 * 1024;

pub async fn handle(req: Request<Incoming>) -> Result<Response<TestBody>, Infallible> {
    let path = req.uri().path().to_owned();
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let response = match segments.as_slice() {
        ["echo"] => echo(req).await,
        ["delay", ms] => {
            tokio::time::sleep(millis(ms)).await;
            text(StatusCode::OK, "done")
        }
        ["slow-body", ms] => slow_body(millis(ms)),
        ["status", code] => status(code.parse().unwrap_or(500)),
        ["redirect", code] => {
            let pad = query_param(&req, "pad").parse().unwrap_or(0);
            let response = redirect(code.parse().unwrap_or(302), &query_param(&req, "to"));
            response.map(|_| Full::new(Bytes::from(vec![b'r'; pad])).boxed())
        }
        ["loop"] => redirect(302, "/loop"),
        ["bytes", n] => bytes(n.parse().unwrap_or(0)),
        ["compressed", encoding] => compressed(encoding).await,
        ["zeros", encoding, n] => zeros(encoding, n.parse().unwrap_or(0)).await,
        ["stall-after", n] => stall_after(n.parse().unwrap_or(0)),
        ["png"] => png(),
        ["login"] => login(),
        _ => text(StatusCode::NOT_FOUND, "no such route"),
    };
    Ok(response)
}

fn millis(ms: &str) -> Duration {
    Duration::from_millis(ms.parse().unwrap_or(0))
}

fn text(status: StatusCode, body: &'static str) -> Response<TestBody> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Full::new(Bytes::from_static(body.as_bytes())).boxed())
        .unwrap()
}

fn status(code: u16) -> Response<TestBody> {
    Response::builder()
        .status(code)
        .body(Empty::new().boxed())
        .unwrap()
}

fn redirect(code: u16, to: &str) -> Response<TestBody> {
    Response::builder()
        .status(code)
        .header(header::LOCATION, to)
        .body(Empty::new().boxed())
        .unwrap()
}

fn query_param(req: &Request<Incoming>, name: &str) -> String {
    let query = req.uri().query().unwrap_or_default();
    form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default()
}

async fn echo(req: Request<Incoming>) -> Response<TestBody> {
    let (parts, body) = req.into_parts();
    let body = body
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    let headers: Vec<(String, String)> = parts
        .headers
        .iter()
        .map(|(name, value)| {
            let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
            (name.as_str().to_owned(), value)
        })
        .collect();
    let json = serde_json::json!({
        "method": parts.method.as_str(),
        "path": parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/"),
        "version": format!("{:?}", parts.version),
        "headers": headers,
        "body": String::from_utf8_lossy(&body),
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(json.to_string())).boxed())
        .unwrap()
}

fn slow_body(delay: Duration) -> Response<TestBody> {
    let body = Delayed {
        sleep: Box::pin(tokio::time::sleep(delay)),
        data: Some(Bytes::from_static(b"finally")),
    };
    Response::builder()
        .header(header::CONTENT_LENGTH, "7")
        .body(body.boxed())
        .unwrap()
}

fn bytes(len: u64) -> Response<TestBody> {
    Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, len)
        .body(Repeat { remaining: len }.boxed())
        .unwrap()
}

async fn compressed(encoding: &str) -> Response<TestBody> {
    let input = COMPRESSED_TEXT.as_bytes();
    let mut out = Vec::new();
    let read = match encoding {
        "gzip" => GzipEncoder::new(input).read_to_end(&mut out).await,
        "br" => BrotliEncoder::new(input).read_to_end(&mut out).await,
        "zstd" => ZstdEncoder::new(input).read_to_end(&mut out).await,
        "deflate" => ZlibEncoder::new(input).read_to_end(&mut out).await,
        _ => return text(StatusCode::NOT_FOUND, "unknown encoding"),
    };
    read.unwrap();
    Response::builder()
        .header(header::CONTENT_TYPE, "text/plain")
        .header(header::CONTENT_ENCODING, encoding)
        .body(Full::new(Bytes::from(out)).boxed())
        .unwrap()
}

async fn zeros(encoding: &str, len: u64) -> Response<TestBody> {
    use async_compression::Level;
    let input = tokio::io::BufReader::new(tokio::io::repeat(0).take(len));
    let mut out = Vec::new();
    let read = match encoding {
        "br" => {
            let mut encoder = BrotliEncoder::with_quality(input, Level::Fastest);
            encoder.read_to_end(&mut out).await
        }
        "zstd" => ZstdEncoder::new(input).read_to_end(&mut out).await,
        _ => return text(StatusCode::NOT_FOUND, "unknown encoding"),
    };
    read.unwrap();
    Response::builder()
        .header(header::CONTENT_ENCODING, encoding)
        .body(Full::new(Bytes::from(out)).boxed())
        .unwrap()
}

fn stall_after(len: u64) -> Response<TestBody> {
    let body = Repeat { remaining: len }.chain_pending();
    Response::builder()
        .header(header::CONTENT_LENGTH, 2 * len)
        .body(body)
        .unwrap()
}

fn login() -> Response<TestBody> {
    Response::builder()
        .header(header::SET_COOKIE, "sid=abc; Path=/")
        .body(Full::new(Bytes::from_static(b"welcome")).boxed())
        .unwrap()
}

fn png() -> Response<TestBody> {
    Response::builder()
        .header(header::CONTENT_TYPE, "image/png")
        .body(Full::new(Bytes::from_static(PNG)).boxed())
        .unwrap()
}

/// `remaining` bytes of a repeating pattern, without holding them in memory.
struct Repeat {
    remaining: u64,
}

impl Body for Repeat {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        if self.remaining == 0 {
            return Poll::Ready(None);
        }
        let len = self.remaining.min(CHUNK as u64);
        self.remaining -= len;
        let chunk: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        Poll::Ready(Some(Ok(Frame::data(Bytes::from(chunk)))))
    }

    fn is_end_stream(&self) -> bool {
        self.remaining == 0
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.remaining)
    }
}

impl Repeat {
    /// These bytes, then a body that never ends.
    fn chain_pending(self) -> TestBody {
        Stalling { head: self }.boxed()
    }
}

/// `head`, then pending forever.
struct Stalling {
    head: Repeat,
}

impl Body for Stalling {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        match Pin::new(&mut self.head).poll_frame(cx) {
            Poll::Ready(None) => Poll::Pending,
            other => other,
        }
    }
}

/// One chunk of data, sent after a delay.
struct Delayed {
    sleep: Pin<Box<Sleep>>,
    data: Option<Bytes>,
}

impl Body for Delayed {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        if self.sleep.as_mut().poll(cx).is_pending() {
            return Poll::Pending;
        }
        Poll::Ready(self.data.take().map(|data| Ok(Frame::data(data))))
    }
}
