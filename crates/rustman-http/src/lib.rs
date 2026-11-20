//! HTTP execution for Rustman: transport, TLS, auth helpers, timings and curl
//! interop. Builds on the request model from `rustman-core`.

mod auth;
mod body;
mod client_auth;
mod connect;
mod cookies;
mod curl;
mod digest;
mod dns;
mod engine;
mod error;
mod exchange;
mod governed;
mod jwt;
mod multipart;
mod oauth2;
#[cfg(feature = "otlp")]
mod otlp;
mod proxy;
mod raw_stream;
mod redirect;
pub mod runner;
mod shown;
mod signed;
mod sigv4;
pub mod snippet;
mod sse;
mod stream;
mod timing;
mod tls;
mod trace;
mod transport;
mod tunnel;
mod websocket;
mod wire;

pub use body::{
    BODIES_DIR, BodyOptions, DEFAULT_MAX_BODY_BYTES, DEFAULT_MAX_DOWNLOAD_BYTES, PREVIEW_BYTES,
    default_spill_dir,
};
pub use client_auth::ClientAuth;
pub use cookies::{COOKIES_DIR, CookieJar, NO_ENVIRONMENT};
pub use curl::{
    COMPRESSED_ACCEPT_ENCODING, CurlError, Exported, Imported, export_curl, import_curl,
    import_curl_words,
};
pub use engine::{BodyOverride, DEFAULT_CONNECT_TIMEOUT, HttpEngine, SendOptions, header_pairs};
pub use error::{Phase, RequestError};
pub use exchange::{Exchange, HttpVersion};
pub use governed::{GovernedError, GovernedSender};
pub use oauth2::{
    LOGIN_TIMEOUT, Login, Loopback, OAuth2Context, Pkce, TOKEN_FILE_WARNING,
    cache_key as oauth2_cache_key, interactive_login_required,
    login_warnings as oauth2_login_warnings,
};
pub use proxy::{NoProxy, ProxyKind, ProxySettings, ProxyUrl};
pub use raw_stream::{ALPN_HTTP1, RawStream, open_raw_stream};
pub use redirect::RedirectHop;
pub use snippet::{Lang, Snippet, SnippetError, SnippetOptions, snippet};
pub use sse::{EVENT_STREAM, SseEvent, SseParser, SseTooLarge, is_event_stream};
pub use stream::{StreamedResponse, request_head};
pub use timing::{Timings, millis};
pub use tls::{ALPN_HTTP, TlsSettings};
pub use websocket::{
    HandshakeFailure, MAX_REFUSED_BODY, OpenedWebSocket, RefusedUpgrade, UrlScheme, WebSocket,
    WebSocketError, WsIo, open_websocket, with_scheme,
};
pub use wire::USER_AGENT_VALUE;
