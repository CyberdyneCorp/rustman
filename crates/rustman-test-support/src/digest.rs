//! A server protected by RFC 7616 Digest auth, which checks every answer
//! with its own implementation and records what it received.
//!
//! Every path answers `200 authorized` to a valid `Authorization: Digest`
//! for [`USER`] / [`PASSWORD`] in realm [`REALM`], else `401` with a
//! challenge. A nonce replaced by [`DigestServer::rotate_nonce`] gets a
//! `401` with `stale=true`.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http::{Request, Response, StatusCode, header};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use md5::{Digest as _, Md5};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

pub const USER: &str = "Mufasa";
pub const PASSWORD: &str = "Circle of Life";
pub const REALM: &str = "api";

/// One request the server received.
#[derive(Clone, Debug)]
pub struct DigestHit {
    pub path: String,
    pub authorization: Option<String>,
    pub status: u16,
}

struct State {
    algorithm: String,
    qop: String,
    nonce: String,
    retired: Vec<String>,
    hits: Vec<DigestHit>,
    body_bytes: u64,
}

pub struct DigestServer {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl Drop for DigestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl DigestServer {
    /// A server challenging with `algorithm` (`MD5`, `SHA-256-sess`, ...)
    /// and `qop="auth"`.
    pub async fn start(algorithm: &str) -> Self {
        Self::start_with(algorithm, "auth").await
    }

    /// A server offering `qop` (such as `auth-int`).
    pub async fn start_with(algorithm: &str, qop: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State {
            algorithm: algorithm.to_owned(),
            qop: qop.to_owned(),
            nonce: "nonce-1".into(),
            retired: Vec::new(),
            hits: Vec::new(),
            body_bytes: 0,
        }));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let state = shared.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| handle(state.clone(), req));
                    let _ = auto::Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(tcp), service)
                        .await;
                });
            }
        });
        Self { addr, state, task }
    }

    /// `http://127.0.0.1:<port><path>`.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    pub fn hits(&self) -> Vec<DigestHit> {
        self.state.lock().unwrap().hits.clone()
    }

    /// Request body bytes received so far, over every request.
    pub fn body_bytes(&self) -> u64 {
        self.state.lock().unwrap().body_bytes
    }

    /// Issue a new nonce; the old one gets `stale=true`.
    pub fn rotate_nonce(&self) {
        let mut state = self.state.lock().unwrap();
        let next = format!("nonce-{}", state.retired.len() + 2);
        let old = std::mem::replace(&mut state.nonce, next);
        state.retired.push(old);
    }
}

async fn handle(
    state: Arc<Mutex<State>>,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let (parts, body) = req.into_parts();
    let body = body
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    let authorization = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let mut state = state.lock().unwrap();
    state.body_bytes += body.len() as u64;
    let verdict = authorization.as_deref().map_or(Verdict::Missing, |a| {
        verify(&state, parts.method.as_str(), a)
    });
    let status = if verdict == Verdict::Valid { 200 } else { 401 };
    state.hits.push(DigestHit {
        path: parts.uri.path().to_owned(),
        authorization,
        status,
    });
    let response = match verdict {
        Verdict::Valid => Response::new(Full::new(Bytes::from_static(b"authorized"))),
        other => challenge(&state, other == Verdict::Stale),
    };
    Ok(response)
}

fn challenge(state: &State, stale: bool) -> Response<Full<Bytes>> {
    let mut value = format!(
        "Digest realm=\"{REALM}\", qop=\"{}\", algorithm={}, nonce=\"{}\", opaque=\"op-1\"",
        state.qop, state.algorithm, state.nonce
    );
    if stale {
        value.push_str(", stale=true");
    }
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(header::WWW_AUTHENTICATE, value)
        .body(Full::new(Bytes::from_static(b"unauthorized")))
        .unwrap()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Missing,
    Invalid,
    Stale,
    Valid,
}

fn verify(state: &State, method: &str, authorization: &str) -> Verdict {
    let Some(params) = authorization.strip_prefix("Digest ").map(params) else {
        return Verdict::Invalid;
    };
    let get = |k: &str| params.get(k).map(String::as_str).unwrap_or_default();
    if state.retired.iter().any(|n| n == get("nonce")) {
        return Verdict::Stale;
    }
    let expected = response(
        &state.algorithm,
        &[get("nonce"), get("nc"), get("cnonce"), get("uri")],
        method,
    );
    let valid = get("username") == USER
        && get("realm") == REALM
        && get("nonce") == state.nonce
        && get("qop") == "auth"
        && get("opaque") == "op-1"
        && get("algorithm").eq_ignore_ascii_case(&state.algorithm)
        && get("response") == expected;
    if valid {
        Verdict::Valid
    } else {
        Verdict::Invalid
    }
}

/// `response` for `[nonce, nc, cnonce, uri]` with `qop=auth`.
fn response(algorithm: &str, fields: &[&str; 4], method: &str) -> String {
    let [nonce, nc, cnonce, uri] = *fields;
    let hash = |s: &str| hash(algorithm, s);
    let mut ha1 = hash(&format!("{USER}:{REALM}:{PASSWORD}"));
    if algorithm.to_ascii_lowercase().ends_with("-sess") {
        ha1 = hash(&format!("{ha1}:{nonce}:{cnonce}"));
    }
    let ha2 = hash(&format!("{method}:{uri}"));
    hash(&format!("{ha1}:{nonce}:{nc}:{cnonce}:auth:{ha2}"))
}

fn hash(algorithm: &str, data: &str) -> String {
    let bytes: Vec<u8> = if algorithm.to_ascii_uppercase().starts_with("MD5") {
        Md5::digest(data.as_bytes()).to_vec()
    } else {
        ring::digest::digest(&ring::digest::SHA256, data.as_bytes())
            .as_ref()
            .to_vec()
    };
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `name=value` or `name="value"` pairs, split on commas outside quotes.
fn params(text: &str) -> HashMap<String, String> {
    let mut fields = Vec::new();
    let (mut current, mut quoted) = (String::new(), false);
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(std::mem::take(&mut current)),
            c => current.push(c),
        }
    }
    fields.push(current);
    fields
        .iter()
        .filter_map(|f| f.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect()
}
