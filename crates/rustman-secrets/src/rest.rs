//! A small JSON-over-HTTP client on `rustman-http` for the REST providers
//! (design D3, D4): the engine's TLS stack verifying against the system
//! trust store, with direct connections. No proxy (neither the workspace's
//! nor `HTTPS_PROXY`), CA file, client certificate or `insecure` setting is
//! applied, so a cloned workspace cannot weaken them. Credentials are known
//! to the request's redactor so no error message can carry them.

use std::time::Duration;

use rustman_core::model::{Auth, RequestSettings};
use rustman_core::secret::Secret;
use rustman_core::vars::{Resolved, ResolvedBody, ResolvedPair, ResolvedRequest, TlsFiles};
use rustman_http::{HttpEngine, SendOptions};
use serde_json::Value;

/// One response.
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(&self) -> Option<Value> {
        serde_json::from_slice(&self.body).ok()
    }
}

/// A request to build.
pub struct Call<'a> {
    pub method: &'a str,
    pub url: String,
    pub headers: Vec<(&'a str, String)>,
    /// Header values that are credentials.
    pub secrets: Vec<Secret<String>>,
    pub body: ResolvedBody,
    /// Auth the engine applies (AWS SigV4 signing).
    pub auth: Option<Auth>,
}

impl<'a> Call<'a> {
    pub fn get(url: String) -> Self {
        Self {
            method: "GET",
            url,
            headers: vec![("Accept", "application/json".to_owned())],
            secrets: Vec::new(),
            body: ResolvedBody::None,
            auth: None,
        }
    }

    pub fn post(url: String, body: Value) -> Self {
        Self {
            method: "POST",
            body: ResolvedBody::Json(body),
            ..Self::get(url)
        }
    }

    /// A form POST (cloud token requests); `secrets` are the field values that are credentials.
    #[cfg_attr(
        not(any(feature = "secrets-gcp", feature = "secrets-azure")),
        allow(dead_code, reason = "used by the GCP and Azure providers")
    )]
    pub fn form(url: String, fields: &[(&str, &str)], secrets: Vec<Secret<String>>) -> Self {
        let pair = |(name, value): &(&str, &str)| pair(name, value);
        Self {
            method: "POST",
            body: ResolvedBody::Form(fields.iter().map(pair).collect()),
            secrets,
            ..Self::get(url)
        }
    }

    /// The same call with `method`.
    #[must_use]
    #[cfg_attr(
        not(any(
            feature = "secrets-aws",
            feature = "secrets-gcp",
            feature = "secrets-azure"
        )),
        allow(dead_code, reason = "used by the cloud providers")
    )]
    pub fn method(mut self, method: &'a str) -> Self {
        self.method = method;
        self
    }

    /// Add header `name` whose value is the credential `value`.
    #[must_use]
    pub fn credential(mut self, name: &'a str, value: &Secret<String>) -> Self {
        self.headers.push((name, value.expose().clone()));
        self.secrets.push(value.clone());
        self
    }

    #[must_use]
    pub fn header(mut self, name: &'a str, value: String) -> Self {
        self.headers.push((name, value));
        self
    }
}

/// Sends calls with one engine (one connection pool).
pub struct Rest {
    engine: HttpEngine,
    options: SendOptions,
}

impl Rest {
    pub fn new(timeout: Duration) -> Self {
        // A margin over the provider timeout: the fetch timeout, with its
        // documented message, always fires first.
        Self::exactly(timeout + Duration::from_secs(1))
    }

    /// A client whose calls give up after `timeout` (metadata endpoints
    /// that may not exist, probed with a short timeout).
    pub fn exactly(timeout: Duration) -> Self {
        let mut options = SendOptions::default();
        options.timeout = timeout;
        options.connect_timeout = options.connect_timeout.min(timeout);
        options.follow_redirects = false;
        Self {
            engine: HttpEngine::new(),
            options,
        }
    }

    /// Send `call`; the error text is redacted and never holds a credential.
    pub async fn send(&self, call: Call<'_>) -> Result<Reply, String> {
        let request = ResolvedRequest {
            method: call.method.to_owned(),
            url: Resolved::literal(call.url),
            query: Vec::new(),
            headers: call.headers.iter().map(|(n, v)| pair(n, v)).collect(),
            body: call.body,
            auth: call.auth,
            settings: RequestSettings::default(),
            tls: TlsFiles::default(),
            secrets: call.secrets,
        };
        let exchange = self
            .engine
            .send(&request, &self.options)
            .await
            .map_err(|e| e.to_string())?;
        Ok(Reply {
            status: exchange.status,
            body: exchange.body.to_vec(),
        })
    }
}

fn pair(name: &str, value: &str) -> ResolvedPair {
    ResolvedPair {
        name: Resolved::literal(name),
        value: Resolved::literal(value),
    }
}

/// What a path segment keeps as is: RFC 3986's unreserved characters.
const SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Encode one path segment.
pub fn segment(text: &str) -> String {
    percent_encoding::utf8_percent_encode(text, SEGMENT).to_string()
}
