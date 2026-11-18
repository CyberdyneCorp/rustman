//! The HTTP engine: turns a `ResolvedRequest` into an `Exchange`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use http::header::{COOKIE, PROXY_AUTHORIZATION};
use http::{HeaderMap, Request, StatusCode};
use http_body_util::Full;
use rustman_core::model::{Auth, RequestSettings};
use rustman_core::settings::{Defaults, Settings};
use rustman_core::vars::ResolvedRequest;
use rustman_core::{Redactor, Secret};

use crate::body::{BODIES_DIR, BodyOptions};
use crate::cookies::{self, CookieJar};
use crate::digest::NonceCache;
use crate::dns::Dns;
use crate::error::RequestError;
use crate::exchange::{Exchange, HttpVersion};
use crate::proxy::{ProxyKind, ProxySettings};
use crate::redirect::{self, RedirectHop};
use crate::sigv4::Signer;
use crate::tls::TlsSettings;
use crate::transport::{PhaseProbe, Received, Transport, TransportKey};
use crate::{auth, jwt, tunnel, wire};

/// The connect timeout when neither the request nor the caller sets one.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The effective transport settings for one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendOptions {
    /// The whole exchange: connect, request, response and body.
    pub timeout: Duration,
    /// DNS, TCP connect and TLS handshake together.
    pub connect_timeout: Duration,
    pub http2_prior_knowledge: bool,
    pub follow_redirects: bool,
    pub max_redirects: u32,
    pub tls: TlsSettings,
    pub proxy: ProxySettings,
    /// The jar that supplies and stores cookies; `None` sends none.
    pub cookies: Option<CookieJar>,
    /// Decompression and the in-memory cap of response bodies.
    pub body: BodyOptions,
}

impl Default for SendOptions {
    fn default() -> Self {
        Self::new(&Defaults::default(), &RequestSettings::default())
    }
}

impl SendOptions {
    /// Request settings over the workspace defaults. TLS files and proxies
    /// are left out (direct connections); see [`SendOptions::for_request`].
    pub fn new(defaults: &Defaults, settings: &RequestSettings) -> Self {
        Self {
            timeout: Duration::from_millis(settings.timeout_ms.unwrap_or(defaults.timeout_ms)),
            connect_timeout: settings
                .connect_timeout_ms
                .map_or(DEFAULT_CONNECT_TIMEOUT, Duration::from_millis),
            http2_prior_knowledge: settings.http2_prior_knowledge.unwrap_or(false),
            follow_redirects: settings
                .follow_redirects
                .unwrap_or(defaults.follow_redirects),
            max_redirects: settings.max_redirects.unwrap_or(defaults.max_redirects),
            tls: TlsSettings {
                verify: settings.verify_tls.unwrap_or(defaults.verify_tls),
                ..TlsSettings::default()
            },
            proxy: ProxySettings::default(),
            cookies: None,
            body: BodyOptions {
                decompress: settings.decompress.unwrap_or(true),
                ..BodyOptions::default()
            },
        }
    }

    /// Use `jar`, unless the request sets `settings.cookies: false`.
    pub fn with_cookie_jar(mut self, settings: &RequestSettings, jar: &CookieJar) -> Self {
        self.cookies = settings.cookies.unwrap_or(true).then(|| jar.clone());
        self
    }

    /// The options for `request` in the workspace at `root`: its TLS files
    /// (the workspace `ca_bundle` plus the request's), client certificate,
    /// proxy (request, then workspace, then environment variables) and body
    /// cap (`[http] max_body_bytes`, spilling to `.rustman/bodies/`).
    pub fn for_request(
        settings: &Settings,
        root: &Path,
        request: &ResolvedRequest,
    ) -> Result<Self, RequestError> {
        Self::for_request_with_env(settings, root, request, |name| std::env::var(name).ok())
            .map_err(|e| e.redacted(&Redactor::for_request(request)))
    }

    fn for_request_with_env(
        settings: &Settings,
        root: &Path,
        request: &ResolvedRequest,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, RequestError> {
        let mut options = Self::new(&settings.defaults, &request.settings);
        options.body.max_in_memory = settings.http.max_body_bytes;
        options.body.max_download = settings.http.max_download_bytes;
        options.body.spill_dir = root.join(BODIES_DIR);
        options.body.workspace = Some(root.to_path_buf());
        let password = request.settings.client_pkcs12_password.as_deref();
        options.tls = TlsSettings::load(&request.tls, password, options.tls.verify)?;
        if let Some(raw) = &settings.http.ca_bundle {
            let mut bundle = std::fs::read(root.join(raw))
                .map_err(|e| RequestError::Io(format!("tls: cannot read ca_bundle {raw}: {e}")))?;
            bundle.extend(options.tls.ca_bundle.take().unwrap_or_default());
            options.tls.ca_bundle = Some(bundle);
        }
        options.proxy = ProxySettings::select(
            request.settings.proxy.as_deref(),
            settings.http.proxy.as_deref(),
            env,
        )?;
        Ok(options)
    }

    fn transport_key(&self) -> TransportKey {
        TransportKey {
            tls: self.tls.clone(),
            connect_timeout: self.connect_timeout,
            http2_prior_knowledge: self.http2_prior_knowledge,
            proxy: self.proxy.clone(),
        }
    }
}

/// Sends requests, keeping one connection pool per distinct transport setting.
#[derive(Default)]
pub struct HttpEngine {
    dns: Arc<Dns>,
    transports: Mutex<HashMap<TransportKey, Arc<Transport>>>,
    /// Digest nonces, reused by later requests to the same host.
    pub(crate) nonces: NonceCache,
}

impl HttpEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Send `request`, following redirects as `options` allow, within the
    /// total timeout. Error messages are redacted.
    pub async fn send(
        &self,
        request: &ResolvedRequest,
        options: &SendOptions,
    ) -> Result<Exchange, RequestError> {
        self.send_with_token(request, options, None).await
    }

    /// [`HttpEngine::send`] with the access token for `auth: {type: oauth2}`.
    pub(crate) async fn send_with_token(
        &self,
        request: &ResolvedRequest,
        options: &SendOptions,
        token: Option<&Secret<String>>,
    ) -> Result<Exchange, RequestError> {
        self.send_unredacted(request, options, token)
            .await
            .map_err(|e| e.redacted(&Redactor::for_request(request)))
    }

    async fn send_unredacted(
        &self,
        request: &ResolvedRequest,
        options: &SendOptions,
        token: Option<&Secret<String>>,
    ) -> Result<Exchange, RequestError> {
        let prepared = Self::prepare(request, token).await?;
        self.dispatch(request, prepared, options, &Ungated).await
    }

    /// The request as it leaves the process (before any hop), with the
    /// warnings building it produced and the SigV4 signer of its hops.
    /// `token` is the OAuth2 access token or the JWT; without one, a JWT
    /// is signed here. Unredacted.
    pub(crate) async fn prepare(
        request: &ResolvedRequest,
        token: Option<&Secret<String>>,
    ) -> Result<Prepared, RequestError> {
        let (mut outgoing, warnings) = wire::build(request).await?;
        let signed;
        let token = match (&request.auth, token) {
            (Some(Auth::Jwt(config)), None) => {
                signed = Secret::new(jwt::SECRET_NAME, jwt::sign(config, jwt::now())?);
                Some(&signed)
            }
            (_, token) => token,
        };
        auth::apply(&mut outgoing, request.auth.as_ref(), token)?;
        let signer = Signer::for_auth(request.auth.as_ref())?;
        Ok(Prepared {
            outgoing,
            warnings,
            signer,
        })
    }

    /// Send a prepared request and follow its redirects within the total
    /// timeout, letting `gate` stop each redirect hop and see every response.
    /// Errors are unredacted.
    pub(crate) async fn dispatch<G: Gate>(
        &self,
        request: &ResolvedRequest,
        prepared: Prepared,
        options: &SendOptions,
        gate: &G,
    ) -> Result<Exchange, G::Error> {
        let transport = self.transport(options)?;
        let probe = PhaseProbe::default();
        let hops = Hops {
            transport: &transport,
            options,
            credentials: auth::credentials(request.auth.as_ref()),
            secrets: Redactor::for_request(request),
            signer: prepared.signer.as_ref(),
            probe: &probe,
            gate,
        };
        let chain = hops.follow(prepared.outgoing);
        tokio::pin!(chain);
        // The phase is read while the request is still alive: dropping it
        // first would also drop a connection attempt in progress.
        let mut exchange = tokio::select! {
            result = &mut chain => result?,
            () = tokio::time::sleep(options.timeout) => {
                let phase = probe.phase();
                let after = options.timeout;
                return Err(RequestError::Timeout { after, phase }.into());
            }
        };
        let redactor = Redactor::for_request(request);
        let mut all = prepared.warnings;
        all.append(&mut exchange.warnings);
        all.extend(
            prepared
                .signer
                .as_ref()
                .and_then(|s| s.skew_hint(&exchange)),
        );
        exchange.warnings = all.iter().map(|w| redactor.text(w).into_owned()).collect();
        Ok(exchange)
    }

    pub(crate) fn transport(&self, options: &SendOptions) -> Result<Arc<Transport>, RequestError> {
        let key = options.transport_key();
        let mut transports = self
            .transports
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(transport) = transports.get(&key) {
            return Ok(transport.clone());
        }
        let transport = Arc::new(Transport::new(self.dns.clone(), &key)?);
        transports.insert(key, transport.clone());
        Ok(transport)
    }
}

/// A request built for the wire, before its first hop.
pub(crate) struct Prepared {
    pub outgoing: Request<Full<Bytes>>,
    pub warnings: Vec<String>,
    /// Signs every hop (`aws_sigv4`).
    pub signer: Option<Signer>,
}

/// Decides, for a governed sender, whether each redirect hop may be sent,
/// and sees every response. The first hop is the caller's to check before
/// [`HttpEngine::dispatch`].
pub(crate) trait Gate: Sync {
    type Error: From<RequestError>;

    /// Called before every redirect hop is sent.
    fn before_redirect(
        &self,
        request: &Request<Full<Bytes>>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Called with the head of every response, redirects included.
    fn after_hop(&self, status: StatusCode, headers: &HeaderMap);
}

/// No gate: requests sent with `rustman send` and `rustman run`.
pub(crate) struct Ungated;

impl Gate for Ungated {
    type Error = RequestError;

    async fn before_redirect(&self, _: &Request<Full<Bytes>>) -> Result<(), RequestError> {
        Ok(())
    }

    fn after_hop(&self, _: StatusCode, _: &HeaderMap) {}
}

/// What every hop of one request shares.
struct Hops<'a, G> {
    transport: &'a Transport,
    options: &'a SendOptions,
    /// Dropped when a redirect leaves the origin.
    credentials: redirect::Credentials,
    /// Finds the request's secrets in the headers and body of a redirect
    /// that leaves the origin.
    secrets: Redactor,
    signer: Option<&'a Signer>,
    probe: &'a PhaseProbe,
    gate: &'a G,
}

impl<G: Gate> Hops<'_, G> {
    /// Send `request` and every redirect hop after it. Every exchange made
    /// with TLS verification off is flagged insecure.
    async fn follow(&self, mut request: Request<Full<Bytes>>) -> Result<Exchange, G::Error> {
        let options = self.options;
        let mut redirects = Vec::new();
        let mut warnings = Vec::new();
        let mut credentials = Redactor::new();
        // SigV4 signs each hop for its own URL, but only while the redirects
        // stay on the request's origin: once one leaves it, no later hop is
        // signed, as `Authorization` is dropped for every other auth.
        let origin = request.uri().clone();
        let mut signing = self.signer.is_some();
        loop {
            let id = uuid::Uuid::new_v4().to_string();
            signing &= redirect::same_origin(&origin, request.uri());
            let signer = self.signer.filter(|_| signing);
            let received = self.send(&request, &id, signer, &mut credentials).await?;
            let head = &received.head;
            self.gate.after_hop(head.status, &head.headers);
            let next = if options.follow_redirects {
                let (status, headers) = (head.status, &head.headers);
                redirect::next_request(&request, status, headers, &self.credentials, &self.secrets)
                    .await?
            } else {
                None
            };
            let Some(next) = next else {
                let url = request.uri().to_string();
                let mut exchange = exchange(id, url, redirects, received, !options.tls.verify);
                exchange.credentials = credentials;
                warnings.append(&mut exchange.warnings);
                exchange.warnings = warnings;
                return Ok(exchange);
            };
            discard_spill(&received);
            if redirects.len() >= options.max_redirects as usize {
                return Err(RequestError::TooManyRedirects(options.max_redirects).into());
            }
            redirects.push(RedirectHop {
                status: received.head.status.as_u16(),
                url: request.uri().to_string(),
            });
            self.gate.before_redirect(&next.request).await?;
            warnings.extend(next.warning);
            request = next.request;
        }
    }

    /// Send one hop, with the jar's cookies and signed by `signer`, and
    /// store the cookies it sets.
    /// The sensitive header values sent and received go to `credentials`.
    async fn send(
        &self,
        request: &Request<Full<Bytes>>,
        id: &str,
        signer: Option<&Signer>,
        credentials: &mut Redactor,
    ) -> Result<Received, RequestError> {
        let options = self.options;
        let mut wire = on_the_wire(request, options);
        if let Some(signer) = signer {
            signer.sign(&mut wire, SystemTime::now())?;
        }
        add_credentials(credentials, wire.headers());
        let received = self
            .transport
            .send(wire, &options.body, id, self.probe)
            .await?;
        add_credentials(credentials, &received.head.headers);
        if let Some(jar) = &options.cookies {
            jar.store(request.uri(), &received.head.headers).await?;
        }
        Ok(received)
    }
}

/// Add the credential of every sensitive header in `headers`.
pub(crate) fn add_credentials(credentials: &mut Redactor, headers: &http::HeaderMap) {
    for (name, value) in headers {
        if let Ok(value) = value.to_str() {
            credentials.add_header_value(name.as_str(), value);
        }
    }
}

/// Remove the spill file of a redirect hop's body, which nothing refers to.
fn discard_spill(received: &Received) {
    if let Some(path) = &received.body.spill {
        // Best effort: a file that cannot be removed is only left behind.
        let _ = std::fs::remove_file(path);
    }
}

/// `request` as sent: the jar's cookies (unless the request sets `Cookie`
/// itself), and, for plain HTTP forwarded by an HTTP proxy, the proxy's
/// credentials (tunnels send them in `CONNECT` instead).
pub(crate) fn on_the_wire(
    request: &Request<Full<Bytes>>,
    options: &SendOptions,
) -> Request<Full<Bytes>> {
    let mut wire = wire::duplicate(request);
    let cookie = options
        .cookies
        .as_ref()
        .filter(|_| !cookies::has_explicit_cookie(request.headers()))
        .and_then(|jar| jar.header_for(request.uri()));
    if let Some(cookie) = cookie {
        wire.headers_mut().insert(COOKIE, cookie);
    }
    let forwarded_by = match request.uri().scheme_str() {
        Some("http") if !options.http2_prior_knowledge => options
            .proxy
            .route(false, request.uri().host().unwrap_or_default())
            .filter(|p| p.kind != ProxyKind::Socks5),
        _ => None,
    };
    if let Some(auth) = forwarded_by.and_then(tunnel::proxy_authorization) {
        wire.headers_mut().insert(PROXY_AUTHORIZATION, auth);
    }
    wire
}

fn exchange(
    id: String,
    url: String,
    redirects: Vec<RedirectHop>,
    received: Received,
    insecure: bool,
) -> Exchange {
    Exchange {
        id,
        url,
        redirects,
        http_version: HttpVersion::from_http(received.head.version),
        status: received.head.status.as_u16(),
        headers: received.head.headers,
        body: received.body.bytes,
        size_bytes: received.body.size,
        spill: received.body.spill,
        binary: received.body.binary,
        timings: received.timings,
        insecure,
        warnings: received.body.warnings,
        credentials: Redactor::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_thirty_and_ten_seconds() {
        let options = SendOptions::default();
        assert_eq!(options.timeout, Duration::from_secs(30));
        assert_eq!(options.connect_timeout, Duration::from_secs(10));
        assert!(!options.http2_prior_knowledge);
        assert!(options.tls.verify);
        assert!(options.follow_redirects);
        assert_eq!(options.max_redirects, 10);
        assert_eq!(options.proxy, ProxySettings::default());
        assert!(options.cookies.is_none());
    }

    #[test]
    fn request_settings_override_workspace_defaults() {
        let defaults = Defaults {
            timeout_ms: 5_000,
            verify_tls: false,
            ..Defaults::default()
        };
        let settings = RequestSettings {
            timeout_ms: Some(500),
            connect_timeout_ms: Some(200),
            http2_prior_knowledge: Some(true),
            follow_redirects: Some(false),
            max_redirects: Some(2),
            ..RequestSettings::default()
        };
        let options = SendOptions::new(&defaults, &settings);
        assert!(!options.follow_redirects);
        assert_eq!(options.max_redirects, 2);
        assert_eq!(options.timeout, Duration::from_millis(500));
        assert_eq!(options.connect_timeout, Duration::from_millis(200));
        assert!(options.http2_prior_knowledge);
        assert!(!options.tls.verify);
        let options = SendOptions::new(&defaults, &RequestSettings::default());
        assert_eq!(options.timeout, Duration::from_millis(5_000));
    }

    fn request(settings: RequestSettings, tls: rustman_core::vars::TlsFiles) -> ResolvedRequest {
        ResolvedRequest {
            method: "GET".into(),
            url: rustman_core::vars::Resolved::literal("https://h/"),
            query: Vec::new(),
            headers: Vec::new(),
            body: rustman_core::vars::ResolvedBody::None,
            auth: None,
            settings,
            tls,
            secrets: Vec::new(),
        }
    }

    fn workspace(extra: &str) -> Settings {
        let text = format!("[workspace]\nname = \"t\"\nversion = 1\n{extra}");
        Settings::parse(Path::new("rustman.toml"), &text).unwrap().0
    }

    #[test]
    fn workspace_and_request_bundles_are_combined() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ws.pem"), "WS\n").unwrap();
        std::fs::write(dir.path().join("req.pem"), "REQ\n").unwrap();
        let tls = rustman_core::vars::TlsFiles {
            ca_bundle: Some(rustman_core::vars::ResolvedFile {
                raw: "req.pem".into(),
                path: dir.path().join("req.pem"),
            }),
            ..Default::default()
        };
        let settings = workspace("[http]\nca_bundle = \"ws.pem\"\n");
        let request = request(RequestSettings::default(), tls);
        let options =
            SendOptions::for_request_with_env(&settings, dir.path(), &request, |_| None).unwrap();
        assert_eq!(options.tls.ca_bundle.as_deref(), Some(&b"WS\nREQ\n"[..]));
    }

    #[test]
    fn the_environment_proxy_applies_without_explicit_ones() {
        let dir = tempfile::tempdir().unwrap();
        let env = |name: &str| (name == "HTTPS_PROXY").then(|| "http://env:3128".to_owned());
        let plain = request(RequestSettings::default(), Default::default());
        let options =
            SendOptions::for_request_with_env(&workspace(""), dir.path(), &plain, env).unwrap();
        assert_eq!(options.proxy.route(true, "h").unwrap().host, "env");
        let explicit = request(
            RequestSettings {
                proxy: Some("socks5://req:1080".into()),
                ..RequestSettings::default()
            },
            Default::default(),
        );
        let options =
            SendOptions::for_request_with_env(&workspace(""), dir.path(), &explicit, env).unwrap();
        assert_eq!(options.proxy.route(true, "h").unwrap().host, "req");
    }

    #[test]
    fn body_options_come_from_the_workspace_and_request() {
        let dir = tempfile::tempdir().unwrap();
        let settings = workspace("[http]\nmax_body_bytes = 1024\nmax_download_bytes = 2048\n");
        let off = RequestSettings {
            decompress: Some(false),
            ..RequestSettings::default()
        };
        let options = SendOptions::for_request_with_env(
            &settings,
            dir.path(),
            &request(off, Default::default()),
            |_| None,
        )
        .unwrap();
        assert_eq!(options.body.max_in_memory, 1024);
        assert_eq!(options.body.max_download, 2048);
        assert_eq!(options.body.spill_dir, dir.path().join(".rustman/bodies"));
        assert!(!options.body.decompress);
        let defaults = SendOptions::default().body;
        assert_eq!(defaults.max_in_memory, 10 * 1024 * 1024);
        assert_eq!(defaults.max_download, 1024 * 1024 * 1024);
        assert!(defaults.decompress);
    }

    #[test]
    fn cookies_false_disables_the_jar() {
        let jar = CookieJar::in_memory();
        let on = SendOptions::default().with_cookie_jar(&RequestSettings::default(), &jar);
        assert_eq!(on.cookies, Some(jar.clone()));
        let off = RequestSettings {
            cookies: Some(false),
            ..RequestSettings::default()
        };
        assert!(
            SendOptions::default()
                .with_cookie_jar(&off, &jar)
                .cookies
                .is_none()
        );
    }
}
