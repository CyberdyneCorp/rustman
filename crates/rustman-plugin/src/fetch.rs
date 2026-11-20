//! Downloading URL sources (plugins spec, "URL sources"): `https` only,
//! TLS verification always on, at most [`MAX_DOWNLOAD_BYTES`], through the
//! `http-requests` engine. Redirects are followed by hand so that every hop
//! must be `https` too.

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
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            ca_bundle: None,
            timeout: Duration::from_secs(60),
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
    let engine = HttpEngine::new();
    let send = send_options(options);
    let mut current = require_https(url)?;
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
            (300..=399, Some(location)) => {
                let next = current
                    .join(location)
                    .map_err(|e| failed(plugin, url, &e.to_string()))?;
                current = require_https(next.as_str())?;
            }
            (status, _) => return Err(failed(plugin, url, &format!("HTTP {status}"))),
        }
    }
    Err(failed(plugin, url, "too many redirects"))
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
}
