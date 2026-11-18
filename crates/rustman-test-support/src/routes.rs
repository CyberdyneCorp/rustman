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
//! | `/endless` | a chunked body of zero bytes that never ends |
//! | `/stall-after/<n>` | `<n>` of the promised `2n` bytes, then nothing |
//! | `/png` | a small PNG, byte for byte [`PNG`] |
//! | `/login` | `200` with `Set-Cookie: sid=abc; Path=/` |
//! | `/session` | `200` with `Set-Cookie: session=`[`SESSION`]`; Path=/` |
//! | `/token` | `200` JSON `{"access_token": TOKEN, "user": {"id": 42}}` |
//! | `/users/42` | `200` JSON [`USER`] with `X-Request-Id: req-1` |
//! | `/orders` | `201` `created order-123456` with `Location` and `Set-Cookie: cart=c-9` |
//! | `/timed/<ms>?id=<id>` | `200 done` after `<ms>` milliseconds, logging server-side start and end times, or when the client dropped it (see [`timed_hits`]) |
//! | `/mirror?pad=<n>&tail=<m>` | `200` text: `<n>` `A`s, the request body as an ASCII-only JSON string (`"caf\u00e9 \"x\""`), `<m>` `B`s |
//! | `/latin1` | `200` Latin-1 JSON (not valid UTF-8): `{"city":"Bel\xe9m","echo":"<request body>"}` |
//! | `/me` | `200` JSON `{"id": 42}` with `Authorization: Bearer TOKEN`, else `401` |
//! | `/aws-skew` | `403` with an AWS `RequestTimeTooSkewed` error body |
//! | `/aws-credential` | `200` text: the `Credential=` scope and the `SignedHeaders=` list of a SigV4 `Authorization`, one per line (no signature) |
//! | `/openapi/<name>` | an OpenAPI document from [`crate::openapi`] |

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

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

/// The user `/users/42` returns.
pub const USER: &str = r#"{"id":42,"name":"Ana","email":"ana@example.com","roles":["admin","dev"],"address":{"city":"Lisbon"}}"#;

/// The access token `/token` issues and `/me` requires.
pub const TOKEN: &str = "tok-7f3a";

const CHUNK: usize = 64 * 1024;

/// The error AWS returns for a request signed too far from its clock.
const AWS_SKEW: &str = "<Error><Code>RequestTimeTooSkewed</Code><Message>The difference between the request time and the current time is too large.</Message></Error>";

/// One `/timed` request as the server saw it.
#[derive(Clone, Debug)]
pub struct TimedHit {
    pub id: String,
    pub start: Instant,
    /// `None` while the response is pending, or when the client went away.
    pub end: Option<Instant>,
    /// When the server dropped the request unanswered because the client
    /// closed the connection.
    pub dropped: Option<Instant>,
}

/// Marks a `/timed` hit as dropped if its handler is dropped before it
/// answers (the client went away).
struct DropGuard(usize);

impl Drop for DropGuard {
    fn drop(&mut self) {
        let mut hits = TIMED.lock().unwrap_or_else(|e| e.into_inner());
        if hits[self.0].end.is_none() {
            hits[self.0].dropped = Some(Instant::now());
        }
    }
}

static TIMED: Mutex<Vec<TimedHit>> = Mutex::new(Vec::new());

/// Every `/timed` request whose id starts with `prefix`, in arrival order.
pub fn timed_hits(prefix: &str) -> Vec<TimedHit> {
    let hits = TIMED.lock().unwrap_or_else(|e| e.into_inner());
    hits.iter()
        .filter(|h| h.id.starts_with(prefix))
        .cloned()
        .collect()
}

async fn timed(req: &Request<Incoming>, ms: &str) -> Response<TestBody> {
    let id = query_param(req, "id");
    let index = {
        let mut hits = TIMED.lock().unwrap_or_else(|e| e.into_inner());
        hits.push(TimedHit {
            id,
            start: Instant::now(),
            end: None,
            dropped: None,
        });
        hits.len() - 1
    };
    let _guard = DropGuard(index);
    tokio::time::sleep(millis(ms)).await;
    TIMED.lock().unwrap_or_else(|e| e.into_inner())[index].end = Some(Instant::now());
    text(StatusCode::OK, "done")
}

pub async fn handle(req: Request<Incoming>) -> Result<Response<TestBody>, Infallible> {
    let path = req.uri().path().to_owned();
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let response = match segments.as_slice() {
        // `/get` answers like httpbin's, for the request `rustman init` writes.
        ["echo"] | ["get"] => echo(req).await,
        ["mirror"] => Box::pin(mirror(req)).await,
        ["latin1"] => Box::pin(latin1(req)).await,
        ["delay", ms] => {
            tokio::time::sleep(millis(ms)).await;
            text(StatusCode::OK, "done")
        }
        ["slow-body", ms] => slow_body(millis(ms)),
        ["timed", ms] => timed(&req, ms).await,
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
        ["endless"] => Response::new(Endless.boxed()),
        ["stall-after", n] => stall_after(n.parse().unwrap_or(0)),
        ["png"] => png(),
        ["login"] => login(),
        ["session"] => session(),
        ["token"] => json(
            StatusCode::OK,
            &serde_json::json!({"access_token": TOKEN, "user": {"id": 42}}),
        ),
        ["me"] => me(&req),
        ["aws-credential"] => aws_credential(&req),
        ["aws-skew"] => Response::builder()
            .status(StatusCode::FORBIDDEN)
            .header(header::CONTENT_TYPE, "application/xml")
            .body(Full::new(Bytes::from_static(AWS_SKEW.as_bytes())).boxed())
            .unwrap(),
        ["users", "42"] => user(),
        ["orders"] => order(),
        ["openapi", name] => openapi(name),
        _ => text(StatusCode::NOT_FOUND, "no such route"),
    };
    Ok(response)
}

/// The `Credential=<key>/<date>/<region>/<service>/aws4_request` scope and
/// the `SignedHeaders=` list of a SigV4 `Authorization` header, which hold
/// no secret.
fn aws_credential(req: &Request<Incoming>) -> Response<TestBody> {
    let authorization = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let field = |name: &str| {
        authorization
            .split_once(name)
            .and_then(|(_, rest)| rest.split(',').next())
            .unwrap_or_default()
            .to_owned()
    };
    let text = format!("{}\n{}", field("Credential="), field("SignedHeaders="));
    Response::new(Full::new(Bytes::from(text)).boxed())
}

fn openapi(name: &str) -> Response<TestBody> {
    match crate::openapi::document(name) {
        Some((body, content_type)) => Response::builder()
            .header(header::CONTENT_TYPE, content_type)
            .body(Full::new(Bytes::from_static(body.as_bytes())).boxed())
            .unwrap(),
        None => text(StatusCode::NOT_FOUND, "no such document"),
    }
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

/// `/mirror`: the request body as a JSON string with every non-ASCII
/// character `\u`-escaped, the way Python's `json.dumps` writes it, between
/// `pad` `A`s and `tail` `B`s.
async fn mirror(req: Request<Incoming>) -> Response<TestBody> {
    let count = |name| query_param(&req, name).parse().unwrap_or(0);
    let (pad, tail) = (count("pad"), count("tail"));
    let body = req.into_body().collect().await.map(|b| b.to_bytes());
    let text = String::from_utf8_lossy(&body.unwrap_or_default()).into_owned();
    let mut escaped = String::new();
    for ch in serde_json::to_string(&text).unwrap_or_default().chars() {
        if ch.is_ascii() {
            escaped.push(ch);
        } else {
            let mut units = [0; 2];
            for unit in ch.encode_utf16(&mut units) {
                escaped.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    let out = format!("{}{escaped}{}", "A".repeat(pad), "B".repeat(tail));
    Response::builder()
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Full::new(Bytes::from(out)).boxed())
        .unwrap()
}

/// `/latin1`: a JSON body in Latin-1, so not valid UTF-8, with the request
/// body inside it as is.
async fn latin1(req: Request<Incoming>) -> Response<TestBody> {
    let body = req.into_body().collect().await.map(|b| b.to_bytes());
    let mut out = b"{\"city\":\"Bel\xe9m\",\"echo\":\"".to_vec();
    out.extend_from_slice(&body.unwrap_or_default());
    out.extend_from_slice(b"\"}");
    Response::builder()
        .header(header::CONTENT_TYPE, "application/json; charset=iso-8859-1")
        .body(Full::new(Bytes::from(out)).boxed())
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

/// The session cookie `/session` sets, long enough to be redacted wherever
/// a server echoes it.
pub const SESSION: &str = "sess-CANARY-4f9Qz";

fn session() -> Response<TestBody> {
    Response::builder()
        .header(header::SET_COOKIE, format!("session={SESSION}; Path=/"))
        .body(Full::new(Bytes::from_static(b"session started")).boxed())
        .unwrap()
}

fn json(status: StatusCode, value: &serde_json::Value) -> Response<TestBody> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(value.to_string())).boxed())
        .unwrap()
}

fn me(req: &Request<Incoming>) -> Response<TestBody> {
    let expected = format!("Bearer {TOKEN}");
    let authorized = req
        .headers()
        .get(header::AUTHORIZATION)
        .is_some_and(|v| v.as_bytes() == expected.as_bytes());
    if authorized {
        json(StatusCode::OK, &serde_json::json!({"id": 42}))
    } else {
        json(
            StatusCode::UNAUTHORIZED,
            &serde_json::json!({"error": "unauthorized"}),
        )
    }
}

fn user() -> Response<TestBody> {
    Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .header("X-Request-Id", "req-1")
        .body(Full::new(Bytes::from_static(USER.as_bytes())).boxed())
        .unwrap()
}

fn order() -> Response<TestBody> {
    Response::builder()
        .status(StatusCode::CREATED)
        .header(header::CONTENT_TYPE, "text/plain")
        .header(header::LOCATION, "/orders/123456")
        .header(header::SET_COOKIE, "cart=c-9; Path=/")
        .body(Full::new(Bytes::from_static(b"created order-123456")).boxed())
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

/// Zero bytes in 64 KiB chunks, forever, with no length known up front.
struct Endless;

impl Body for Endless {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(&[0; CHUNK])))))
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
