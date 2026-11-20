//! The loopback redirect target of the authorization-code flow: a one-shot
//! listener on `127.0.0.1` with an ephemeral port (RFC 8252).
//!
//! Connections are served concurrently, each with its own short read
//! timeout, and a callback with the wrong `state` is answered `400` and
//! ignored, so another local process can neither hold the listener nor end
//! the login. At most [`MAX_CONNECTIONS`] are served at once (others wait in
//! the listen queue), and a failing `accept` (out of file descriptors, for
//! instance) is retried with a growing delay instead of in a busy loop.

use std::future::Future;
use std::io;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

use crate::error::RequestError;

/// The path of the redirect URI.
const CALLBACK_PATH: &str = "/callback";

/// The most bytes read from the browser's request head.
const MAX_HEAD: usize = 16 * 1024;

/// How long one connection may take to send its request head.
const HEAD_TIMEOUT: Duration = Duration::from_secs(5);

/// The most connections served at once.
const MAX_CONNECTIONS: usize = 16;

/// The delay before retrying a failed `accept`, doubled on each failure in a
/// row up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_millis(10);
const MAX_BACKOFF: Duration = Duration::from_secs(1);

const DONE_PAGE: &str = "<!doctype html><title>rustman</title>\
    <p>Login complete. You can close this window and return to the terminal.</p>";

const FAILED_PAGE: &str = "<!doctype html><title>rustman</title>\
    <p>Login failed. Check the terminal for details.</p>";

/// What the browser brought back to the redirect URI.
#[derive(Debug, PartialEq, Eq)]
enum Callback {
    /// Not the callback path (a favicon request, for instance).
    Other,
    /// The callback path with another `state`: not the browser we sent.
    Forged,
    Code(String),
    Failed(String),
}

pub struct Loopback {
    listener: TcpListener,
    redirect_uri: String,
    /// The path of `redirect_uri`: only it delivers the code.
    callback_path: String,
}

impl Loopback {
    pub async fn bind() -> Result<Self, RequestError> {
        let (listener, port) = listen(0).await?;
        Ok(Self {
            listener,
            redirect_uri: format!("http://127.0.0.1:{port}{CALLBACK_PATH}"),
            callback_path: CALLBACK_PATH.to_owned(),
        })
    }

    /// A listener on `127.0.0.1:<port>` (0: any free port) whose redirect
    /// URI is `http://localhost:<port><prefix>/callback`, for servers that
    /// only accept a registered redirect URI (Vault OIDC).
    pub async fn bind_on(port: u16, prefix: &str) -> Result<Self, RequestError> {
        let (listener, port) = listen(port).await?;
        Ok(Self {
            listener,
            redirect_uri: format!("http://localhost:{port}{prefix}{CALLBACK_PATH}"),
            callback_path: format!("{prefix}{CALLBACK_PATH}"),
        })
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Wait for the browser to deliver the authorization code for `state`.
    pub async fn wait_for_code(
        &self,
        state: &str,
        timeout: Duration,
    ) -> Result<String, RequestError> {
        let callback = Expected {
            path: &self.callback_path,
            state,
        };
        let accepting = accept_code(&self.listener, &callback, MAX_CONNECTIONS);
        tokio::time::timeout(timeout, accepting)
            .await
            .unwrap_or_else(|_| {
                Err(RequestError::Auth(format!(
                    "oauth2: no authorization response within {} s",
                    timeout.as_secs()
                )))
            })
    }
}

async fn listen(port: u16) -> Result<(TcpListener, u16), RequestError> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| RequestError::Auth(format!("oauth2: cannot listen on 127.0.0.1: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| RequestError::Auth(format!("oauth2: loopback listener: {e}")))?
        .port();
    Ok((listener, port))
}

/// A source of connections: the TCP listener, or a fake one in tests.
trait Accept {
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;

    fn next_connection(&self) -> impl Future<Output = io::Result<Self::Stream>> + Send;
}

impl Accept for TcpListener {
    type Stream = TcpStream;

    async fn next_connection(&self) -> io::Result<TcpStream> {
        TcpListener::accept(self).await.map(|(stream, _)| stream)
    }
}

/// The callback a login waits for: its path and `state` parameter.
struct Expected<'a> {
    path: &'a str,
    state: &'a str,
}

/// Serve connections from `acceptor`, at most `max_connections` at once,
/// until one delivers the outcome `expected`.
async fn accept_code<A: Accept + Sync>(
    acceptor: &A,
    expected: &Expected<'_>,
    max_connections: usize,
) -> Result<String, RequestError> {
    let mut connections = JoinSet::new();
    let mut backoff = Backoff::default();
    loop {
        tokio::select! {
            accepted = acceptor.next_connection(), if connections.len() < max_connections => {
                match accepted {
                    Ok(stream) => {
                        backoff = Backoff::default();
                        let (path, state) = (expected.path.to_owned(), expected.state.to_owned());
                        connections.spawn(serve(stream, path, state));
                    }
                    Err(_) => backoff.wait().await,
                }
            }
            Some(served) = connections.join_next() => {
                if let Ok(Some(outcome)) = served {
                    return outcome;
                }
            }
        }
    }
}

/// The growing delay between failed `accept` calls.
struct Backoff(Duration);

impl Default for Backoff {
    fn default() -> Self {
        Self(FIRST_BACKOFF)
    }
}

impl Backoff {
    async fn wait(&mut self) {
        tokio::time::sleep(self.0).await;
        self.0 = (self.0 * 2).min(MAX_BACKOFF);
    }
}

/// Answer one connection; `Some` when it ends the login.
async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    path: String,
    state: String,
) -> Option<Result<String, RequestError>> {
    let head = tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut stream))
        .await
        .ok()?
        .ok()?;
    let callback = parse_callback(&head, &path, &state);
    let (status, page) = match &callback {
        Callback::Other => ("404 Not Found", "not found"),
        Callback::Code(_) => ("200 OK", DONE_PAGE),
        Callback::Forged | Callback::Failed(_) => ("400 Bad Request", FAILED_PAGE),
    };
    respond(&mut stream, status, page).await;
    match callback {
        Callback::Other | Callback::Forged => None,
        Callback::Code(code) => Some(Ok(code)),
        Callback::Failed(reason) => Some(Err(RequestError::Auth(reason))),
    }
}

async fn read_head(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<String> {
    let mut head = Vec::new();
    let mut buf = [0u8; 2048];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < MAX_HEAD {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        head.extend_from_slice(&buf[..n]);
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

async fn respond(stream: &mut (impl AsyncWrite + Unpin), status: &str, page: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{page}",
        page.len()
    );
    // The code is already received; a browser that went away is no failure.
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// The outcome carried by the request line of `head`, when it targets the
/// callback `expected_path` exactly.
fn parse_callback(head: &str, expected_path: &str, state: &str) -> Callback {
    let target = head
        .lines()
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or_default();
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != expected_path {
        return Callback::Other;
    }
    let param = |name: &str| {
        form_urlencoded::parse(query.as_bytes())
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    if param("state").as_deref() != Some(state) {
        return Callback::Forged;
    }
    if let Some(error) = param("error") {
        let detail = param("error_description").map_or(String::new(), |d| format!(": {d}"));
        return Callback::Failed(format!("oauth2: authorization denied ({error}{detail})"));
    }
    match param("code") {
        Some(code) if !code.is_empty() => Callback::Code(code),
        _ => Callback::Failed("oauth2: the authorization response has no code".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED: Expected<'static> = Expected {
        path: CALLBACK_PATH,
        state: "st",
    };

    fn head(target: &str) -> String {
        format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
    }

    #[test]
    fn callbacks_are_parsed() {
        assert_eq!(
            parse_callback(&head("/callback?code=abc&state=s1"), CALLBACK_PATH, "s1"),
            Callback::Code("abc".into())
        );
        assert_eq!(
            parse_callback(&head("/favicon.ico"), CALLBACK_PATH, "s1"),
            Callback::Other
        );
        assert_eq!(
            parse_callback(&head("/callback?code=abc&state=evil"), CALLBACK_PATH, "s1"),
            Callback::Forged
        );
        assert_eq!(
            parse_callback(
                &head("/callback?error=access_denied&error_description=no+way&state=s1"),
                CALLBACK_PATH,
                "s1"
            ),
            Callback::Failed("oauth2: authorization denied (access_denied: no way)".into())
        );
        assert_eq!(
            parse_callback(&head("/callback?state=s1"), CALLBACK_PATH, "s1"),
            Callback::Failed("oauth2: the authorization response has no code".into())
        );
        // A prefixed callback (Vault OIDC) matches exactly, and only it.
        let vault = "/oidc/callback";
        assert_eq!(
            parse_callback(&head("/oidc/callback?code=abc&state=s1"), vault, "s1"),
            Callback::Code("abc".into())
        );
        for other in ["/callback", "/x/oidc/callback", "/evil/callback"] {
            let target = format!("{other}?code=abc&state=s1");
            assert_eq!(parse_callback(&head(&target), vault, "s1"), Callback::Other);
        }
        assert_eq!(
            parse_callback(
                &head("/oidc/callback?code=abc&state=s1"),
                CALLBACK_PATH,
                "s1"
            ),
            Callback::Other
        );
    }

    async fn browse(uri: &str, target: &str) -> String {
        let addr = uri
            .trim_start_matches("http://")
            .trim_end_matches(CALLBACK_PATH)
            .to_owned();
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(head(target).as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    #[tokio::test]
    async fn the_listener_waits_for_the_callback() {
        let loopback = Loopback::bind().await.unwrap();
        let uri = loopback.redirect_uri().to_owned();
        assert!(uri.starts_with("http://127.0.0.1:"), "{uri}");
        let browser = tokio::spawn(async move {
            let other = browse(&uri, "/favicon.ico").await;
            let done = browse(&uri, "/callback?code=c0de&state=st").await;
            (other, done)
        });
        let code = loopback
            .wait_for_code("st", Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(code, "c0de");
        let (other, done) = browser.await.unwrap();
        assert!(other.starts_with("HTTP/1.1 404"), "{other}");
        assert!(done.starts_with("HTTP/1.1 200") && done.contains("Login complete"));
    }

    #[tokio::test]
    async fn silent_or_forged_connections_do_not_block_the_login() {
        let loopback = Loopback::bind().await.unwrap();
        let uri = loopback.redirect_uri().to_owned();
        let addr = uri
            .trim_start_matches("http://")
            .trim_end_matches(CALLBACK_PATH)
            .to_owned();
        let browser = tokio::spawn(async move {
            let silent = TcpStream::connect(&addr).await.unwrap();
            let forged = browse(&uri, "/callback?code=evil&state=wrong").await;
            let done = browse(&uri, "/callback?code=c0de&state=st").await;
            drop(silent);
            (forged, done)
        });
        let code = loopback
            .wait_for_code("st", Duration::from_secs(3))
            .await
            .unwrap();
        assert_eq!(code, "c0de");
        let (forged, done) = browser.await.unwrap();
        assert!(forged.starts_with("HTTP/1.1 400"), "{forged}");
        assert!(done.starts_with("HTTP/1.1 200"), "{done}");
    }

    /// A fake listener: hands out in-memory connections that never send
    /// anything, or fails every `accept`, and counts the calls.
    struct FakeListener {
        fail: bool,
        calls: std::sync::atomic::AtomicUsize,
        peers: std::sync::Mutex<Vec<tokio::io::DuplexStream>>,
    }

    impl FakeListener {
        fn new(fail: bool) -> Self {
            Self {
                fail,
                calls: 0.into(),
                peers: Vec::new().into(),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl Accept for FakeListener {
        type Stream = tokio::io::DuplexStream;

        async fn next_connection(&self) -> io::Result<Self::Stream> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail {
                return Err(io::Error::other("too many open files"));
            }
            let (ours, theirs) = tokio::io::duplex(1024);
            self.peers.lock().unwrap().push(theirs);
            Ok(ours)
        }
    }

    #[tokio::test]
    async fn failing_accepts_back_off_instead_of_spinning() {
        let listener = FakeListener::new(true);
        let accepting = accept_code(&listener, &EXPECTED, MAX_CONNECTIONS);
        let waited = tokio::time::timeout(Duration::from_millis(500), accepting).await;
        assert!(waited.is_err(), "the login cannot end without a callback");
        // 10, 20, 40, 80, 160 ms of delay fit in 500 ms: about 6 calls, where
        // an immediate retry would make millions.
        let calls = listener.calls();
        assert!((2..=10).contains(&calls), "{calls} accept calls");
    }

    #[tokio::test]
    async fn concurrent_connections_are_capped() {
        let listener = FakeListener::new(false);
        let accepting = accept_code(&listener, &EXPECTED, 3);
        let waited = tokio::time::timeout(Duration::from_millis(300), accepting).await;
        assert!(waited.is_err(), "the login cannot end without a callback");
        assert_eq!(listener.calls(), 3, "silent connections hold every slot");
    }

    #[tokio::test]
    async fn the_listener_gives_up_after_the_timeout() {
        let loopback = Loopback::bind().await.unwrap();
        let err = loopback
            .wait_for_code("st", Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("oauth2: no authorization response within"),
            "{err}"
        );
    }
}
