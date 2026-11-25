//! Downloading URL sources (plugins spec, "URL sources"): `https` only,
//! TLS verification always on, at most [`MAX_DOWNLOAD_BYTES`], through the
//! `http-requests` engine. Redirects are followed by hand, only to `https`
//! on the same host and port. The download runs before the plugin set is
//! trusted (`rustman plugin verify`, a headless load), so the URL a
//! repository commits must not reach this machine or a private network: a
//! host name that resolves to an address that is not public is refused
//! when the connection opens. A URL naming such an address (or
//! `localhost`) itself is downloaded only when the user asks for it
//! ([`FetchOptions::allow_local`]): `rustman plugin add`, whose URL they
//! typed, or `rustman plugin verify --allow-local-sources`, which fills the
//! cache of a clone. Otherwise (`plugin verify`, a headless load) it is
//! refused before anything is sent.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use rustman_core::model::RequestSettings;
use rustman_core::vars::{Resolved, ResolvedBody, ResolvedRequest, TlsFiles};
use rustman_http::{BodyOptions, HttpEngine, SendOptions, TlsSettings};

use crate::error::LoadError;

/// The largest plugin a URL source may serve: 20 MiB.
pub const MAX_DOWNLOAD_BYTES: u64 = 20 * 1024 * 1024;

/// Redirect hops followed at most.
const MAX_REDIRECTS: usize = 5;

/// How downloads connect.
#[derive(Clone, Debug)]
pub struct FetchOptions {
    /// Extra trusted CAs (a workspace's `ca_bundle`). Verification itself
    /// cannot be turned off.
    pub ca_bundle: Option<Vec<u8>>,
    pub timeout: Duration,
    /// Download from a URL that names this machine or a private network
    /// (`localhost`, a literal non-public address). Only the user turns it
    /// on: `plugin add`, whose URL they typed, and `plugin verify
    /// --allow-local-sources`. Off, such a URL is refused before any
    /// connection.
    pub allow_local: bool,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            ca_bundle: None,
            timeout: Duration::from_secs(60),
            allow_local: false,
        }
    }
}

/// Refuse anything but an `https` URL.
pub fn require_https(url: &str) -> Result<url::Url, LoadError> {
    let parsed = url::Url::parse(url).map_err(|_| LoadError::PlainHttp)?;
    if parsed.scheme() == "https" {
        Ok(parsed)
    } else {
        Err(LoadError::PlainHttp)
    }
}

/// Download `url` (named `plugin` in messages).
pub async fn fetch(plugin: &str, url: &str, options: &FetchOptions) -> Result<Vec<u8>, LoadError> {
    let engine = HttpEngine::with_address_guard(Arc::new(check_download_address));
    let send = send_options(options);
    let mut current = require_https(url)?;
    if !options.allow_local {
        refuse_local(plugin, url, &current)?;
    }
    for _ in 0..=MAX_REDIRECTS {
        let exchange = engine
            .send(&get(current.as_str()), &send)
            .await
            .map_err(|e| failed(plugin, url, &e.to_string()))?;
        let location = exchange
            .headers
            .get(http::header::LOCATION)
            .and_then(|v| v.to_str().ok());
        match (exchange.status, location) {
            (200, _) => return Ok(exchange.body.to_vec()),
            (300..=399, Some(location)) => current = redirect(plugin, url, &current, location)?,
            (status, _) => return Err(failed(plugin, url, &format!("HTTP {status}"))),
        }
    }
    Err(failed(plugin, url, "too many redirects"))
}

/// The connect-time check of downloads: a host name may not resolve to an
/// address that is not public ([`rustman_http::address::is_public_ip`]),
/// unless the URL names the address or `localhost` itself.
pub fn check_download_address(host: &str, ip: IpAddr) -> Result<(), String> {
    crate::sandbox::net::check_address(host, ip).map_err(|_| {
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        format!("{bare} resolves to the non-public address {ip} (download from the address itself to reach it)")
    })
}

/// Refuse a committed URL source whose host names this machine or a
/// private network itself (`localhost`, `10.1.2.3`, `[::1]`): verifying an
/// untrusted clone must not send a request there. Redirects stay on this
/// host, so checking the first URL covers them.
fn refuse_local(plugin: &str, url: &str, parsed: &url::Url) -> Result<(), LoadError> {
    let host = parsed.host_str().unwrap_or_default();
    if !crate::sandbox::net::is_local_host(host) {
        return Ok(());
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let message = format!(
        "{bare} is this machine or a private network, which a committed source \
         may not name (if you trust this source, `rustman plugin verify \
         --allow-local-sources` downloads and caches it)"
    );
    Err(failed(plugin, url, &message))
}

/// The next hop of a redirect from `current` to `location`: `https`, on
/// the same host and port.
fn redirect(
    plugin: &str,
    url: &str,
    current: &url::Url,
    location: &str,
) -> Result<url::Url, LoadError> {
    let next = current
        .join(location)
        .map_err(|e| failed(plugin, url, &e.to_string()))?;
    let next = require_https(next.as_str())?;
    if next.host() != current.host()
        || next.port_or_known_default() != current.port_or_known_default()
    {
        let message = format!("redirect to another host refused ({next})");
        return Err(failed(plugin, url, &message));
    }
    Ok(next)
}

fn failed(plugin: &str, url: &str, message: &str) -> LoadError {
    LoadError::invalid(plugin, format!("download of {url} failed: {message}"))
}

fn send_options(options: &FetchOptions) -> SendOptions {
    SendOptions {
        timeout: options.timeout,
        follow_redirects: false,
        tls: TlsSettings {
            verify: true,
            ca_bundle: options.ca_bundle.clone(),
            client_auth: None,
        },
        cookies: None,
        // Kept in memory whole: nothing larger than the cap is accepted, so
        // nothing is spilled to disk.
        body: BodyOptions {
            max_in_memory: MAX_DOWNLOAD_BYTES + 1,
            max_download: MAX_DOWNLOAD_BYTES,
            max_decoded: MAX_DOWNLOAD_BYTES,
            ..BodyOptions::default()
        },
        ..SendOptions::default()
    }
}

fn get(url: &str) -> ResolvedRequest {
    ResolvedRequest {
        method: "GET".to_owned(),
        url: Resolved::literal(url),
        query: Vec::new(),
        headers: Vec::new(),
        body: ResolvedBody::None,
        auth: None,
        settings: RequestSettings::default(),
        tls: TlsFiles::default(),
        secrets: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_urls_are_fetched() {
        assert!(require_https("https://example.com/p.wasm").is_ok());
        for refused in ["http://example.com/p.wasm", "ftp://x/p.wasm", "nonsense"] {
            assert_eq!(require_https(refused), Err(LoadError::PlainHttp));
        }
        let options = send_options(&FetchOptions::default());
        assert!(options.tls.verify);
        assert!(!options.follow_redirects);
        assert_eq!(options.body.max_download, 20 * 1024 * 1024);
    }

    /// A plugin URL committed by a repository is downloaded before trust:
    /// a name resolving to this machine, a private network or the cloud
    /// metadata address is refused; a URL naming the address is not.
    #[test]
    fn downloads_refuse_non_public_addresses() {
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        let refused = check_download_address("plugins.example.com", ip("169.254.169.254"));
        assert_eq!(
            refused.unwrap_err(),
            "plugins.example.com resolves to the non-public address 169.254.169.254 \
             (download from the address itself to reach it)"
        );
        for private in ["127.0.0.1", "10.1.2.3", "100.64.0.1", "::1", "fd00::1"] {
            assert!(
                check_download_address("plugins.example.com", ip(private)).is_err(),
                "{private}"
            );
        }
        assert!(check_download_address("plugins.example.com", ip("93.184.216.34")).is_ok());
        assert!(check_download_address("localhost", ip("127.0.0.1")).is_ok());
        assert!(check_download_address("10.1.2.3", ip("10.1.2.3")).is_ok());
    }

    /// A committed URL naming this machine or a private network is refused
    /// before anything is sent; a public host is not.
    #[test]
    fn committed_sources_may_not_name_local_addresses() {
        let check = |url: &str| refuse_local("p", url, &url::Url::parse(url).unwrap());
        for local in [
            "https://localhost:8443/p.wasm",
            "https://10.1.2.3/p.wasm",
            "https://169.254.169.254/p.wasm",
            "https://[::1]:9/p.wasm",
            "https://[fd00::1]/p.wasm",
        ] {
            let err = check(local).unwrap_err().to_string();
            assert!(
                err.contains("is this machine or a private network"),
                "{local}: {err}"
            );
        }
        assert!(check("https://plugins.example.com/p.wasm").is_ok());
        assert!(check("https://93.184.216.34/p.wasm").is_ok());
        assert!(!FetchOptions::default().allow_local);
    }

    #[test]
    fn redirects_stay_on_the_same_https_host() {
        let current = url::Url::parse("https://plugins.example.com/p.wasm").unwrap();
        let next = redirect("p", "u", &current, "/v2/p.wasm").unwrap();
        assert_eq!(next.as_str(), "https://plugins.example.com/v2/p.wasm");
        let same = redirect("p", "u", &current, "https://plugins.example.com:443/x").unwrap();
        assert_eq!(same.path(), "/x");
        for refused in [
            "https://evil.example.com/p.wasm",
            "https://127.0.0.1/p.wasm",
            "https://plugins.example.com:8443/p.wasm",
        ] {
            let err = redirect("p", "u", &current, refused).unwrap_err();
            assert!(
                err.to_string().contains("redirect to another host refused"),
                "{err}"
            );
        }
        let err = redirect("p", "u", &current, "http://plugins.example.com/p.wasm");
        assert_eq!(err.unwrap_err(), LoadError::PlainHttp);
    }
}
