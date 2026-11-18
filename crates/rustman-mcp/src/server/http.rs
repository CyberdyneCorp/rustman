//! The Streamable HTTP transport (design D2): the MCP service mounted at
//! `/mcp` in an axum router whose middleware checks the Origin and the
//! bearer token before a request reaches MCP. With the loopback-only bind,
//! that makes three independent locks.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, ORIGIN, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rustman_core::safety::Origin;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use super::Sessions;
use super::token;

/// The MCP endpoint.
pub const MCP_PATH: &str = "/mcp";

/// Where `--http` listens without an address.
pub const DEFAULT_ADDR: &str = "127.0.0.1:8765";

/// Why `--http <addr>` was refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum BindError {
    #[error("invalid address '{0}': expected <ip>:<port>, such as 127.0.0.1:8765")]
    Invalid(String),
    #[error(
        "refusing to serve on non-loopback address {0}: --allow-remote is required (anyone who can reach it could drive this workspace)"
    )]
    NotLoopback(SocketAddr),
}

/// The address to listen on: `addr` (default [`DEFAULT_ADDR`]), which must
/// be a loopback address unless `allow_remote`.
pub fn bind_address(addr: Option<&str>, allow_remote: bool) -> Result<SocketAddr, BindError> {
    let text = addr.unwrap_or(DEFAULT_ADDR);
    let addr: SocketAddr = text
        .parse()
        .map_err(|_| BindError::Invalid(text.to_owned()))?;
    if !addr.ip().is_loopback() && !allow_remote {
        return Err(BindError::NotLoopback(addr));
    }
    Ok(addr)
}

/// Whether rmcp's Host allowlist (the DNS-rebinding check) is dropped:
/// only when `--allow-remote` serves a non-loopback address, where clients
/// name the server by hosts the allowlist cannot know.
pub fn accepts_any_host(bound: SocketAddr, allow_remote: bool) -> bool {
    allow_remote && !bound.ip().is_loopback()
}

/// What the middleware checks.
#[derive(Clone, Debug)]
pub struct HttpGuard {
    token: String,
    allowed_origins: Vec<Origin>,
}

impl HttpGuard {
    pub fn new(token: String, allowed_origins: Vec<Origin>) -> Self {
        Self {
            token,
            allowed_origins,
        }
    }

    /// Whether a request with these headers may reach MCP: `Err` is the
    /// status to answer with instead.
    pub fn check(&self, headers: &HeaderMap) -> Result<(), StatusCode> {
        if let Some(origin) = headers.get(ORIGIN) {
            let allowed = origin
                .to_str()
                .is_ok_and(|o| origin_allowed(o, &self.allowed_origins));
            if !allowed {
                return Err(StatusCode::FORBIDDEN);
            }
        }
        let given = headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(token::bearer);
        match given {
            Some(given) if token::same(given, &self.token) => Ok(()),
            _ => Err(StatusCode::UNAUTHORIZED),
        }
    }
}

/// Whether a browser `Origin` is a loopback origin or listed in
/// `[agent].allowed_origins`.
pub fn origin_allowed(origin: &str, allowed: &[Origin]) -> bool {
    let Ok(parsed) = Origin::of(origin) else {
        return false;
    };
    is_loopback_host(parsed.host()) || allowed.contains(&parsed)
}

fn is_loopback_host(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    bare == "localhost"
        || bare.ends_with(".localhost")
        || bare.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

async fn guard(State(guard): State<Arc<HttpGuard>>, request: Request, next: Next) -> Response {
    match guard.check(request.headers()) {
        Ok(()) => next.run(request).await,
        Err(StatusCode::UNAUTHORIZED) => (
            StatusCode::UNAUTHORIZED,
            [(WWW_AUTHENTICATE, "Bearer")],
            "missing or invalid bearer token\n",
        )
            .into_response(),
        Err(status) => (status, "origin not allowed\n").into_response(),
    }
}

/// The router: `/mcp` behind the guard, a session per MCP client.
/// `any_host` comes from [`accepts_any_host`].
pub fn router(
    sessions: Sessions,
    guard: HttpGuard,
    any_host: bool,
    cancel: CancellationToken,
) -> axum::Router {
    let mut config = StreamableHttpServerConfig::default().with_cancellation_token(cancel);
    if any_host {
        config = config.disable_allowed_hosts();
    }
    let service = StreamableHttpService::new(
        move || Ok(sessions.open()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    axum::Router::new()
        .route_service(MCP_PATH, service)
        .layer(middleware::from_fn_with_state(Arc::new(guard), self::guard))
}

/// Serve `router` on `listener` until `cancel` is cancelled.
pub async fn serve(
    listener: TcpListener,
    router: axum::Router,
    cancel: CancellationToken,
) -> std::io::Result<()> {
    axum::serve(listener, router)
        .with_graceful_shutdown(async move { cancel.cancelled().await })
        .await
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    #[test]
    fn loopback_addresses_bind_and_others_need_allow_remote() {
        assert_eq!(
            bind_address(None, false).unwrap().to_string(),
            "127.0.0.1:8765"
        );
        assert!(bind_address(Some("[::1]:9000"), false).is_ok());
        let err = bind_address(Some("0.0.0.0:8765"), false).unwrap_err();
        assert!(err.to_string().contains("--allow-remote"), "{err}");
        assert!(bind_address(Some("0.0.0.0:8765"), true).is_ok());
        assert_eq!(
            bind_address(Some("localhost"), false),
            Err(BindError::Invalid("localhost".into()))
        );
    }

    #[test]
    fn the_host_check_stays_on_loopback_even_with_allow_remote() {
        let loopback: SocketAddr = "127.0.0.1:8765".parse().unwrap();
        let wildcard: SocketAddr = "0.0.0.0:8765".parse().unwrap();
        assert!(!accepts_any_host(loopback, false));
        assert!(!accepts_any_host(loopback, true));
        assert!(!accepts_any_host("[::1]:8765".parse().unwrap(), true));
        assert!(!accepts_any_host(wildcard, false));
        assert!(accepts_any_host(wildcard, true));
    }

    #[test]
    fn loopback_and_listed_origins_are_allowed() {
        let listed = [Origin::parse("https://inspector.example.com").unwrap()];
        for origin in [
            "http://localhost:6274",
            "http://127.0.0.1:3000",
            "http://[::1]:80",
            "http://app.localhost",
            "https://inspector.example.com",
        ] {
            assert!(origin_allowed(origin, &listed), "{origin}");
        }
        for origin in [
            "https://evil.example",
            "null",
            "http://127.0.0.1.evil.example",
        ] {
            assert!(!origin_allowed(origin, &listed), "{origin}");
        }
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn the_guard_checks_origin_then_token() {
        let guard = HttpGuard::new("t0ken".into(), Vec::new());
        let ok = headers(&[("authorization", "Bearer t0ken")]);
        assert_eq!(guard.check(&ok), Ok(()));
        assert_eq!(guard.check(&headers(&[])), Err(StatusCode::UNAUTHORIZED));
        let wrong = headers(&[("authorization", "Bearer nope")]);
        assert_eq!(guard.check(&wrong), Err(StatusCode::UNAUTHORIZED));
        let evil = headers(&[
            ("authorization", "Bearer t0ken"),
            ("origin", "https://evil.example"),
        ]);
        assert_eq!(guard.check(&evil), Err(StatusCode::FORBIDDEN));
        let local = headers(&[
            ("authorization", "Bearer t0ken"),
            ("origin", "http://localhost:5173"),
        ]);
        assert_eq!(guard.check(&local), Ok(()));
    }
}
