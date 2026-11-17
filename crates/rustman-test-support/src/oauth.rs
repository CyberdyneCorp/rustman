//! A mock OAuth2 authorization server with a protected resource.
//!
//! | Route | Behavior |
//! |-------|----------|
//! | `GET /authorize` | records the PKCE challenge, then `302` to `redirect_uri` with a code |
//! | `POST /token` | `client_credentials`, `authorization_code` (checks the verifier) and `refresh_token` |
//! | `GET /api` | `200` echoing the bearer token when it is valid, else `401` |
//!
//! The client is `app`; when a secret is sent it must be `s3cret`.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use http::{Request, Response, StatusCode, header};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

pub const CLIENT_ID: &str = "app";
pub const CLIENT_SECRET: &str = "s3cret";
const CODE: &str = "the-code";

#[derive(Default)]
struct State {
    /// The `grant_type` of every token request, in order.
    grants: Vec<String>,
    /// `expires_in` of the next tokens issued.
    expires_in: u64,
    /// The challenge of the last `/authorize` request.
    challenge: Option<String>,
    /// The parameters of the last `/authorize` request.
    authorize: HashMap<String, String>,
    valid: Vec<String>,
    refresh: Vec<String>,
    issued: u32,
}

pub struct OAuthServer {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl Drop for OAuthServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl OAuthServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State {
            expires_in: 3600,
            ..State::default()
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

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.addr.port())
    }

    /// The `grant_type` of every token request so far.
    pub fn grants(&self) -> Vec<String> {
        self.state.lock().unwrap().grants.clone()
    }

    /// The parameters of the last `/authorize` request.
    pub fn authorize_params(&self) -> HashMap<String, String> {
        self.state.lock().unwrap().authorize.clone()
    }

    /// Issue tokens with this lifetime from now on.
    pub fn set_expires_in(&self, seconds: u64) {
        self.state.lock().unwrap().expires_in = seconds;
    }

    /// Invalidate every access token issued so far.
    pub fn revoke_access_tokens(&self) {
        self.state.lock().unwrap().valid.clear();
    }
}

type Body = Full<Bytes>;

async fn handle(
    state: Arc<Mutex<State>>,
    req: Request<Incoming>,
) -> Result<Response<Body>, Infallible> {
    let path = req.uri().path().to_owned();
    let response = match path.as_str() {
        "/authorize" => authorize(&state, req.uri().query().unwrap_or_default()),
        "/token" => {
            let body = req.into_body().collect().await.unwrap().to_bytes();
            token(&state, &form(&body))
        }
        "/api" => api(&state, &req),
        _ => reply(StatusCode::NOT_FOUND, "no such route".into()),
    };
    Ok(response)
}

fn form(bytes: &[u8]) -> HashMap<String, String> {
    form_urlencoded::parse(bytes).into_owned().collect()
}

fn reply(status: StatusCode, body: String) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap()
}

fn authorize(state: &Mutex<State>, query: &str) -> Response<Body> {
    let params = form(query.as_bytes());
    let mut state = state.lock().unwrap();
    state.challenge = params.get("code_challenge").cloned();
    state.authorize = params.clone();
    let mut location = url::Url::parse(&params["redirect_uri"]).unwrap();
    location
        .query_pairs_mut()
        .append_pair("code", CODE)
        .append_pair("state", &params["state"]);
    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, location.as_str())
        .body(Full::new(Bytes::new()))
        .unwrap()
}

fn token(state: &Mutex<State>, params: &HashMap<String, String>) -> Response<Body> {
    let mut state = state.lock().unwrap();
    let grant = params.get("grant_type").cloned().unwrap_or_default();
    state.grants.push(grant.clone());
    if !client_ok(params) {
        return oauth_error(StatusCode::UNAUTHORIZED, "invalid_client");
    }
    let issue_refresh = match grant.as_str() {
        "client_credentials" => false,
        "authorization_code" if code_ok(&state, params) => true,
        "refresh_token" if state.refresh.contains(&params["refresh_token"]) => false,
        _ => return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant"),
    };
    issue(&mut state, issue_refresh)
}

fn client_ok(params: &HashMap<String, String>) -> bool {
    params.get("client_id").map(String::as_str) == Some(CLIENT_ID)
        && params
            .get("client_secret")
            .is_none_or(|s| s == CLIENT_SECRET)
}

/// The code is ours and the verifier hashes to the recorded challenge.
fn code_ok(state: &State, params: &HashMap<String, String>) -> bool {
    let verifier = params.get("code_verifier").cloned().unwrap_or_default();
    let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
    params.get("code").map(String::as_str) == Some(CODE)
        && state.challenge.as_deref() == Some(URL_SAFE_NO_PAD.encode(digest).as_str())
        && params.get("redirect_uri") == state.authorize.get("redirect_uri")
}

fn issue(state: &mut State, with_refresh: bool) -> Response<Body> {
    state.issued += 1;
    let access = format!("tok-{}", state.issued);
    state.valid.push(access.clone());
    let mut json = serde_json::json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": state.expires_in,
    });
    if with_refresh {
        let refresh = format!("refresh-{}", state.issued);
        state.refresh.push(refresh.clone());
        json["refresh_token"] = refresh.into();
    }
    reply(StatusCode::OK, json.to_string())
}

fn oauth_error(status: StatusCode, error: &str) -> Response<Body> {
    reply(status, serde_json::json!({ "error": error }).to_string())
}

fn api(state: &Mutex<State>, req: &Request<Incoming>) -> Response<Body> {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if state.lock().unwrap().valid.iter().any(|t| t == token) {
        reply(StatusCode::OK, format!("{{\"token\":\"{token}\"}}"))
    } else {
        reply(StatusCode::UNAUTHORIZED, "{}".into())
    }
}
