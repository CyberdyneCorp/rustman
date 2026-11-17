//! HTTP execution for Rustman: transport, TLS, auth helpers, timings and curl
//! interop. Builds on the request model from `rustman-core`.

mod auth;
mod body;
mod client_auth;
mod connect;
mod cookies;
mod curl;
mod dns;
mod engine;
mod error;
mod exchange;
mod multipart;
mod oauth2;
mod proxy;
mod redirect;
mod timing;
mod tls;
mod transport;
mod tunnel;
mod wire;

pub use body::{BODIES_DIR, BodyOptions, DEFAULT_MAX_BODY_BYTES, PREVIEW_BYTES};
pub use client_auth::ClientAuth;
pub use cookies::{COOKIES_DIR, CookieJar, NO_ENVIRONMENT};
pub use curl::{
    COMPRESSED_ACCEPT_ENCODING, CurlError, Exported, Imported, export_curl, import_curl,
    import_curl_words,
};
pub use engine::{DEFAULT_CONNECT_TIMEOUT, HttpEngine, SendOptions};
pub use error::{Phase, RequestError};
pub use exchange::{Exchange, HttpVersion};
pub use oauth2::{
    LOGIN_TIMEOUT, Login, OAuth2Context, cache_key as oauth2_cache_key, interactive_login_required,
};
pub use proxy::{NoProxy, ProxyKind, ProxySettings, ProxyUrl};
pub use redirect::RedirectHop;
pub use timing::{Timings, millis};
pub use tls::{ALPN_HTTP, TlsSettings};
pub use wire::USER_AGENT_VALUE;
