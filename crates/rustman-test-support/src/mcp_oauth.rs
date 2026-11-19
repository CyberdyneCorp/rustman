//! The OAuth side of [`crate::McpHttpServer::oauth`]: the MCP endpoint
//! answers `401` with a `resource_metadata` challenge until a valid bearer
//! token is sent, and the same server plays the authorization server.
//!
//! | Route | Behavior |
//! |-------|----------|
//! | `GET /.well-known/oauth-protected-resource/mcp` | RFC 9728 metadata naming `<base>/as` |
//! | `GET /.well-known/oauth-authorization-server/as` | RFC 8414 metadata (with `registration_endpoint` when registration is on) |
//! | `POST /as/register` | RFC 7591: `201` with client `dyn-client` |
//! | `GET /as/authorize` | records the parameters, then `302` to `redirect_uri` with code `the-code` |
//! | `POST /as/token` | `authorization_code` (checks the PKCE verifier and `resource`) and `refresh_token` |

use std::collections::HashMap;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::graphql_http::RecordedRequest;

/// The client id dynamic registration hands out.
pub const DYNAMIC_CLIENT_ID: &str = "dyn-client";
const CODE: &str = "the-code";

/// What the authorization server knows.
#[derive(Debug, Default)]
pub(crate) struct AuthState {
    pub registration: bool,
    pub expires_in: u64,
    /// `grant_type` of every token request, in order.
    pub grants: Vec<String>,
    /// The parameters of the last `/as/authorize`.
    pub authorize: HashMap<String, String>,
    pub registrations: usize,
    pub valid: Vec<String>,
    pub refresh: Vec<String>,
    /// Replaces the advertised `resource` (default: `<base>/mcp`).
    pub resource: Option<String>,
    /// Replaces the advertised `issuer` (default: `<base>/as`).
    pub issuer: Option<String>,
    /// Starts every access token issued (default: `access-`).
    pub access_prefix: Option<String>,
    issued: u32,
}

/// A JSON answer, `302` redirect or bare status.
pub(crate) enum Reply {
    Json(u16, Value),
    Redirect(String),
    Unauthorized(String),
}

impl AuthState {
    pub fn new(registration: bool) -> Self {
        Self {
            registration,
            expires_in: 3600,
            ..Self::default()
        }
    }

    /// Whether `req` carries a valid bearer token.
    pub fn authorized(&self, req: &RecordedRequest) -> bool {
        req.header("authorization")
            .and_then(|h| h.strip_prefix("Bearer "))
            .is_some_and(|t| self.valid.iter().any(|v| v == t))
    }

    /// The answer to an OAuth route, or `None` for the MCP endpoint itself.
    pub fn route(&mut self, base: &str, req: &RecordedRequest) -> Option<Reply> {
        let path = req.uri.split('?').next().unwrap_or_default();
        let reply = match (req.method.as_str(), path) {
            ("GET", "/.well-known/oauth-protected-resource/mcp") => Reply::Json(
                200,
                json!({
                    "resource": self.resource.clone().unwrap_or_else(|| format!("{base}/mcp")),
                    "authorization_servers": [format!("{base}/as")],
                    "scopes_supported": ["mcp"]
                }),
            ),
            ("GET", "/.well-known/oauth-authorization-server/as") => self.metadata(base),
            ("POST", "/as/register") => self.register(),
            ("GET", "/as/authorize") => self.authorize(req),
            ("POST", "/as/token") => self.token(req),
            _ if self.authorized(req) => return None,
            _ => Reply::Unauthorized(format!(
                r#"Bearer resource_metadata="{base}/.well-known/oauth-protected-resource/mcp""#
            )),
        };
        Some(reply)
    }

    fn metadata(&self, base: &str) -> Reply {
        let mut metadata = json!({
            "issuer": self.issuer.clone().unwrap_or_else(|| format!("{base}/as")),
            "authorization_endpoint": format!("{base}/as/authorize"),
            "token_endpoint": format!("{base}/as/token"),
            "code_challenge_methods_supported": ["S256"]
        });
        if self.registration {
            metadata["registration_endpoint"] = json!(format!("{base}/as/register"));
        }
        Reply::Json(200, metadata)
    }

    fn register(&mut self) -> Reply {
        self.registrations += 1;
        Reply::Json(201, json!({"client_id": DYNAMIC_CLIENT_ID}))
    }

    fn authorize(&mut self, req: &RecordedRequest) -> Reply {
        let query = req.uri.split_once('?').map(|(_, q)| q).unwrap_or_default();
        self.authorize = form(query.as_bytes());
        let redirect = self
            .authorize
            .get("redirect_uri")
            .cloned()
            .unwrap_or_default();
        let state = self.authorize.get("state").cloned().unwrap_or_default();
        Reply::Redirect(format!("{redirect}?code={CODE}&state={state}"))
    }

    fn token(&mut self, req: &RecordedRequest) -> Reply {
        let params = form(&req.body);
        let grant = params.get("grant_type").cloned().unwrap_or_default();
        self.grants.push(grant.clone());
        let ok = match grant.as_str() {
            "authorization_code" => self.code_is_valid(&params),
            "refresh_token" => params
                .get("refresh_token")
                .is_some_and(|r| self.refresh.contains(r)),
            _ => false,
        };
        if !ok || !params.contains_key("resource") {
            return Reply::Json(400, json!({"error": "invalid_grant"}));
        }
        self.issue()
    }

    fn code_is_valid(&self, params: &HashMap<String, String>) -> bool {
        let verifier = params.get("code_verifier").cloned().unwrap_or_default();
        let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
        params.get("code").map(String::as_str) == Some(CODE)
            && self.authorize.get("code_challenge").map(String::as_str)
                == Some(URL_SAFE_NO_PAD.encode(digest).as_str())
    }

    fn issue(&mut self) -> Reply {
        self.issued += 1;
        let prefix = self.access_prefix.as_deref().unwrap_or("access-");
        let access = format!("{prefix}{}", self.issued);
        let refresh = format!("refresh-{}", self.issued);
        self.valid.push(access.clone());
        self.refresh.push(refresh.clone());
        Reply::Json(
            200,
            json!({
                "access_token": access,
                "refresh_token": refresh,
                "token_type": "Bearer",
                "expires_in": self.expires_in
            }),
        )
    }
}

fn form(bytes: &[u8]) -> HashMap<String, String> {
    form_urlencoded::parse(bytes).into_owned().collect()
}

/// GET `url` as a browser would, without following redirects: the status
/// and the `Location` header.
pub async fn browse(url: &str) -> (u16, Option<String>) {
    let url = url::Url::parse(url).expect("a valid URL");
    let host = url.host_str().expect("a host");
    let port = url.port_or_known_default().expect("a port");
    let target = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    };
    let mut stream = TcpStream::connect((host, port)).await.expect("reachable");
    let head = format!("GET {target} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(head.as_bytes()).await.expect("sent");
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response).await;
    let text = String::from_utf8_lossy(&response);
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let location = text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("location")
            .then(|| value.trim().to_owned())
    });
    (status, location)
}
