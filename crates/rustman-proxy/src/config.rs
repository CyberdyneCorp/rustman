//! What the recording proxy listens on and how it treats traffic (spec:
//! Forward proxy command, Loopback by default, Capture limits), and the
//! `[record]` section of `rustman.toml`.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use rustman_core::LoadError;
use rustman_core::settings::SETTINGS_FILE;
use rustman_http::TlsSettings;
use serde::Deserialize;

use crate::filter::FilterRules;

/// The default listen address.
pub const DEFAULT_HOST: &str = "127.0.0.1";

/// The default listen port.
pub const DEFAULT_PORT: u16 = 8888;

/// The default cap on each stored body: 10 MiB.
pub const DEFAULT_MAX_BODY: usize = 10 * 1024 * 1024;

/// The default number of captures kept.
pub const DEFAULT_MAX_CAPTURES: usize = 2000;

/// The default cap on the bytes all stored bodies take: 512 MiB.
pub const DEFAULT_MAX_BUFFER_BYTES: usize = 512 * 1024 * 1024;

/// How long the proxy waits for an upstream connection (DNS, TCP and TLS).
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a client may take to send a request head or to finish a TLS
/// handshake on an intercepted tunnel.
pub const DEFAULT_CLIENT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long an upstream may take to send a response head, counted from
/// the end of the request body.
pub const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

/// How long an opaque tunnel or an intercepted connection may stay
/// without traffic before it is closed.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// The default number of client connections served at once; more wait.
pub const DEFAULT_MAX_CONNECTIONS: usize = 512;

/// Why the proxy did not start.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BindError {
    #[error("record: invalid host {0:?} (use an IP address or localhost)")]
    InvalidHost(String),
    #[error("record: binding a non-loopback address requires --allow-remote")]
    RemoteNotAllowed,
    #[error("record: cannot bind {addr}: {reason}")]
    Bind { addr: SocketAddr, reason: String },
}

/// The listen address and the traffic rules of one proxy.
#[derive(Clone, Debug)]
pub struct ProxyConfig {
    pub host: String,
    pub port: u16,
    /// Allow a listen address reachable from the network.
    pub allow_remote: bool,
    /// Host globs whose `CONNECT` tunnels are decrypted (needs a CA).
    pub intercept: Vec<String>,
    /// How upstream servers are verified; the `http-requests` rules.
    pub upstream_tls: TlsSettings,
    pub connect_timeout: Duration,
    /// See [`DEFAULT_CLIENT_TIMEOUT`].
    pub client_timeout: Duration,
    /// See [`DEFAULT_RESPONSE_TIMEOUT`]; past it the client gets `504`.
    pub response_timeout: Duration,
    /// See [`DEFAULT_IDLE_TIMEOUT`].
    pub idle_timeout: Duration,
    /// Client connections served at once (tunnels included); more wait.
    pub max_connections: usize,
    /// The largest part of a body stored in a capture.
    pub max_body: usize,
    pub max_captures: usize,
    /// The bytes all stored bodies may take together, and separately the
    /// bytes the copies of all bodies still in flight may hold: past it, a
    /// body being received is proxied in full but stored truncated.
    pub max_buffer_bytes: usize,
    /// Which exchanges are recorded; all are proxied.
    pub filter: FilterRules,
    /// Reverse-proxy mode: the origin every request is forwarded to.
    pub upstream: Option<String>,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_owned(),
            port: DEFAULT_PORT,
            allow_remote: false,
            intercept: Vec::new(),
            upstream_tls: TlsSettings::default(),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            client_timeout: DEFAULT_CLIENT_TIMEOUT,
            response_timeout: DEFAULT_RESPONSE_TIMEOUT,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            max_body: DEFAULT_MAX_BODY,
            max_captures: DEFAULT_MAX_CAPTURES,
            max_buffer_bytes: DEFAULT_MAX_BUFFER_BYTES,
            filter: FilterRules::default(),
            upstream: None,
        }
    }
}

impl ProxyConfig {
    /// The socket address to bind, refused when it is reachable from the
    /// network without `allow_remote`.
    pub fn bind_addr(&self) -> Result<SocketAddr, BindError> {
        let ip = parse_host(&self.host)?;
        if !ip.is_loopback() && !self.allow_remote {
            return Err(BindError::RemoteNotAllowed);
        }
        Ok(SocketAddr::new(ip, self.port))
    }
}

fn parse_host(host: &str) -> Result<IpAddr, BindError> {
    if host.eq_ignore_ascii_case("localhost") {
        return Ok(IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    bare.parse()
        .map_err(|_| BindError::InvalidHost(host.to_owned()))
}

/// The reason shown after `cannot bind <addr>:`, without OS error numbers.
pub fn bind_reason(err: &io::Error) -> String {
    match err.kind() {
        io::ErrorKind::AddrInUse => "address in use".to_owned(),
        io::ErrorKind::AddrNotAvailable => "address not available".to_owned(),
        io::ErrorKind::PermissionDenied => "permission denied".to_owned(),
        _ => {
            let text = err.to_string();
            match text.find(" (os error") {
                Some(at) => text[..at].to_owned(),
                None => text,
            }
        }
    }
}

/// The warning printed when the proxy is reachable from the network.
pub fn remote_warning(addr: SocketAddr) -> String {
    format!(
        "warning: the recording proxy on {addr} is reachable from the network; \
         anyone who can reach it can use it and see intercepted traffic"
    )
}

/// `[record]` in `rustman.toml`. `rustman-core` accepts the section; flags
/// override it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecordSettings {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub intercept: Vec<String>,
    /// Bytes of each body kept in a capture.
    pub max_body: Option<usize>,
    pub max_captures: Option<usize>,
    /// Bytes all kept bodies may take together.
    pub max_buffer_bytes: Option<usize>,
    /// Milliseconds an upstream may take to send a response head.
    pub response_timeout_ms: Option<u64>,
    /// Milliseconds a tunnel or intercepted connection may stay idle.
    pub idle_timeout_ms: Option<u64>,
}

/// A positive number of milliseconds; `0` keeps the default.
fn millis(ms: Option<u64>) -> Option<Duration> {
    ms.filter(|ms| *ms > 0).map(Duration::from_millis)
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    record: RecordSettings,
}

impl RecordSettings {
    /// The `[record]` section of `text`, the contents of `rustman.toml`.
    pub fn parse(text: &str) -> Result<Self, LoadError> {
        toml::from_str::<File>(text)
            .map(|file| file.record)
            .map_err(|e| {
                let message = format!("[record]: {}", e.message().trim_end());
                LoadError::new(Path::new(SETTINGS_FILE), None, message)
            })
    }

    /// The proxy configuration these settings describe, with the defaults
    /// for what they leave out (front-ends without flags, the TUI).
    pub fn proxy_config(&self) -> ProxyConfig {
        let defaults = ProxyConfig::default();
        ProxyConfig {
            host: self.host.clone().unwrap_or(defaults.host),
            port: self.port.unwrap_or(defaults.port),
            intercept: self.intercept.clone(),
            max_body: self.max_body.unwrap_or(defaults.max_body),
            max_captures: self.max_captures.unwrap_or(defaults.max_captures),
            max_buffer_bytes: self.max_buffer_bytes.unwrap_or(defaults.max_buffer_bytes),
            response_timeout: millis(self.response_timeout_ms).unwrap_or(defaults.response_timeout),
            idle_timeout: millis(self.idle_timeout_ms).unwrap_or(defaults.idle_timeout),
            ..defaults
        }
    }

    /// Warnings for what these settings widen: the hosts decrypted (unless
    /// `--intercept` flags replace them) and memory limits above the
    /// defaults. `rustman.toml` is committed, so a cloned workspace may
    /// set them without the user choosing to.
    pub fn warnings(&self, intercept_flags: bool) -> Vec<String> {
        let mut warnings = Vec::new();
        if !intercept_flags && !self.intercept.is_empty() {
            warnings.push(format!(
                "warning: [record] intercept in {SETTINGS_FILE} decrypts HTTPS to {}; \
                 pass --intercept to choose the hosts yourself",
                self.intercept.join(", ")
            ));
        }
        let limits = [
            ("max_body", self.max_body, DEFAULT_MAX_BODY),
            ("max_captures", self.max_captures, DEFAULT_MAX_CAPTURES),
            (
                "max_buffer_bytes",
                self.max_buffer_bytes,
                DEFAULT_MAX_BUFFER_BYTES,
            ),
        ];
        for (name, value, default) in limits {
            if let Some(value) = value.filter(|v| *v > default) {
                warnings.push(format!(
                    "warning: [record] {name} = {value} in {SETTINGS_FILE} is above the default \
                     ({default}); memory use grows with it"
                ));
            }
        }
        warnings
    }

    /// The `[record]` section of the workspace at `root`.
    pub fn load(root: &Path) -> Result<Self, LoadError> {
        let text = std::fs::read_to_string(root.join(SETTINGS_FILE))
            .map_err(|e| LoadError::new(Path::new(SETTINGS_FILE), None, e.to_string()))?;
        Self::parse(&text)
    }
}

/// How the workspace at `root` verifies upstream servers (http-requests:
/// the platform trust store plus `[http] ca_bundle`, a path relative to
/// the root). `[defaults] verify_tls = false` is deliberately not applied:
/// the recorder turns verification off only for one named host
/// (`--insecure` with `--upstream`), never for every intercepted host.
pub fn workspace_upstream_tls(root: &Path) -> Result<TlsSettings, String> {
    let (settings, _) = rustman_core::settings::Settings::load(root).map_err(|e| e.to_string())?;
    let Some(bundle) = settings.http.ca_bundle else {
        return Ok(TlsSettings::default());
    };
    let path = root.join(&bundle);
    let pem = std::fs::read(&path).map_err(|e| {
        format!(
            "record: cannot read [http] ca_bundle {}: {e}",
            path.display()
        )
    })?;
    let tls = TlsSettings {
        ca_bundle: Some(pem),
        ..TlsSettings::default()
    };
    tls.client_config(rustman_http::ALPN_HTTP)
        .map_err(|e| format!("record: [http] ca_bundle {bundle}: {e}"))?;
    Ok(tls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_loopback_port_8888() {
        let config = ProxyConfig::default();
        assert_eq!(config.bind_addr().unwrap().to_string(), "127.0.0.1:8888");
        assert_eq!(config.max_body, 10 * 1024 * 1024);
        assert_eq!(config.max_captures, 2000);
    }

    #[test]
    fn a_network_address_needs_allow_remote() {
        let mut config = ProxyConfig {
            host: "0.0.0.0".into(),
            ..ProxyConfig::default()
        };
        let err = config.bind_addr().unwrap_err();
        assert_eq!(
            err.to_string(),
            "record: binding a non-loopback address requires --allow-remote"
        );
        config.allow_remote = true;
        assert_eq!(config.bind_addr().unwrap().to_string(), "0.0.0.0:8888");
    }

    #[test]
    fn localhost_and_ipv6_loopback_are_loopback() {
        for host in ["localhost", "::1", "[::1]", "127.0.0.2"] {
            let config = ProxyConfig {
                host: host.into(),
                ..ProxyConfig::default()
            };
            assert!(config.bind_addr().is_ok(), "{host}");
        }
        let config = ProxyConfig {
            host: "example.com".into(),
            ..ProxyConfig::default()
        };
        assert!(matches!(config.bind_addr(), Err(BindError::InvalidHost(_))));
    }

    #[test]
    fn bind_reasons_have_no_os_error_numbers() {
        let busy = io::Error::from(io::ErrorKind::AddrInUse);
        assert_eq!(bind_reason(&busy), "address in use");
        let other = io::Error::other("boom (os error 99)");
        assert_eq!(bind_reason(&other), "boom");
    }

    #[test]
    fn settings_that_widen_interception_or_memory_are_warned_about() {
        let settings = RecordSettings {
            intercept: vec!["*".into()],
            max_body: Some(DEFAULT_MAX_BODY * 2),
            max_captures: Some(10),
            ..RecordSettings::default()
        };
        let warnings = settings.warnings(false);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings[0].contains("decrypts HTTPS to *"),
            "{}",
            warnings[0]
        );
        assert!(warnings[1].contains("[record] max_body"), "{}", warnings[1]);
        assert_eq!(settings.warnings(true).len(), 1);
        assert!(RecordSettings::default().warnings(false).is_empty());
    }

    #[test]
    fn record_settings_read_the_section() {
        let text = "[workspace]\nname = \"a\"\nversion = 1\n[record]\nport = 9999\n\
                    intercept = [\"*.example.com\"]\nmax_body = 1024\n";
        let settings = RecordSettings::parse(text).unwrap();
        assert_eq!(settings.port, Some(9999));
        assert_eq!(settings.max_body, Some(1024));
        assert_eq!(settings.intercept, ["*.example.com"]);
        let config = settings.proxy_config();
        assert_eq!(config.port, 9999);
        assert_eq!(config.max_body, 1024);
        assert_eq!(config.host, DEFAULT_HOST);
        assert_eq!(config.max_captures, DEFAULT_MAX_CAPTURES);
        assert_eq!(config.response_timeout, DEFAULT_RESPONSE_TIMEOUT);
        assert_eq!(config.idle_timeout, DEFAULT_IDLE_TIMEOUT);
        assert!(!config.allow_remote);
        let timeouts =
            RecordSettings::parse("[record]\nresponse_timeout_ms = 1500\nidle_timeout_ms = 0\n")
                .unwrap();
        let config = timeouts.proxy_config();
        assert_eq!(config.response_timeout, Duration::from_millis(1500));
        assert_eq!(
            config.idle_timeout, DEFAULT_IDLE_TIMEOUT,
            "0 keeps the default"
        );
        let err = RecordSettings::parse("[record]\nbogus = 1\n").unwrap_err();
        assert!(err.to_string().contains("[record]"), "{err}");
    }

    #[test]
    fn the_workspace_ca_bundle_verifies_upstreams() {
        let dir = tempfile::tempdir().unwrap();
        let toml = "[workspace]\nname = \"a\"\nversion = 1\n";
        std::fs::write(dir.path().join(SETTINGS_FILE), toml).unwrap();
        let plain = workspace_upstream_tls(dir.path()).unwrap();
        assert!(plain.verify && plain.ca_bundle.is_none());

        let ca = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        std::fs::write(dir.path().join("ca.pem"), ca.cert.pem()).unwrap();
        let with_bundle =
            format!("{toml}[defaults]\nverify_tls = false\n[http]\nca_bundle = \"ca.pem\"\n");
        std::fs::write(dir.path().join(SETTINGS_FILE), with_bundle).unwrap();
        let tls = workspace_upstream_tls(dir.path()).unwrap();
        assert_eq!(tls.ca_bundle.as_deref(), Some(ca.cert.pem().as_bytes()));
        assert!(
            tls.verify,
            "verify_tls = false is not applied to every upstream"
        );

        std::fs::write(dir.path().join("ca.pem"), "not a certificate").unwrap();
        let err = workspace_upstream_tls(dir.path()).unwrap_err();
        assert!(err.contains("ca_bundle"), "{err}");
    }
}
