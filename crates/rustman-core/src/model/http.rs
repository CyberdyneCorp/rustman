//! The `kind: http` request file.

use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer};

use super::assertion::Assertion;
use super::auth::Auth;
use super::body::{Body, PartSource};
use super::capture::Capture;
use super::common::{Checked, NameValue, Variables};
use crate::format::Located;
use crate::secret::{Hidden, placeholder};

/// A `kind: http` request file. Optional fields stay `None` when absent, so a
/// file that is loaded and saved again keeps exactly the keys it had.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpRequest {
    pub version: u64,
    pub kind: String,
    pub name: String,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    /// How the request appears in `rustman docs` output.
    pub docs: Option<super::docs::DocsMeta>,
    /// Defaults to `GET` when absent.
    pub method: Option<Method>,
    pub url: String,
    pub query: Option<Vec<NameValue>>,
    pub headers: Option<Vec<NameValue>>,
    pub body: Option<Body>,
    pub auth: Option<Auth>,
    pub settings: Option<RequestSettings>,
    pub variables: Option<Variables>,
    #[serde(default, deserialize_with = "super::snapshot::unique_snapshot_names")]
    pub assertions: Option<Vec<Assertion>>,
    pub captures: Option<Vec<Checked<Capture>>>,
}

impl HttpRequest {
    /// The method to send: the file's `method`, or `GET`.
    pub fn effective_method(&self) -> &str {
        self.method.as_ref().map_or("GET", Method::as_str)
    }

    /// Every workspace file this request refers to, with its source position.
    pub fn file_paths(&self) -> Vec<&Located<String>> {
        let mut paths = Vec::new();
        match &self.body {
            Some(Body::File(path)) => paths.push(path),
            Some(Body::Multipart(parts)) => {
                paths.extend(parts.iter().filter_map(|p| match &p.source {
                    PartSource::File(path) => Some(path),
                    PartSource::Text(_) => None,
                }))
            }
            _ => {}
        }
        if let Some(settings) = &self.settings {
            paths.extend(settings.file_paths());
        }
        for assertion in self.assertions.iter().flatten() {
            if let Assertion::Schema(schema) = assertion {
                paths.extend(schema.file.as_ref());
            }
        }
        paths
    }
}

/// An HTTP method: any RFC 9110 token, so custom methods such as `PURGE` work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Method(String);

impl Method {
    pub fn parse(value: &str) -> Result<Self, String> {
        if !value.is_empty() && value.bytes().all(is_tchar) {
            Ok(Self(value.to_owned()))
        } else {
            Err(format!("invalid HTTP method '{value}'"))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `tchar` from RFC 9110 section 5.6.2.
fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

impl<'de> Deserialize<'de> for Method {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

/// Per-request overrides for timeouts, redirects, TLS and transport.
///
/// `Debug` hides `client_pkcs12_password` and any credentials in `proxy`.
#[derive(Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestSettings {
    pub timeout_ms: Option<u64>,
    pub connect_timeout_ms: Option<u64>,
    pub follow_redirects: Option<bool>,
    pub max_redirects: Option<u32>,
    pub verify_tls: Option<bool>,
    pub http2_prior_knowledge: Option<bool>,
    /// A proxy URL, or `none` to bypass every proxy.
    pub proxy: Option<String>,
    pub cookies: Option<bool>,
    pub decompress: Option<bool>,
    pub ca_bundle: Option<Located<String>>,
    pub client_cert: Option<Located<String>>,
    pub client_key: Option<Located<String>>,
    pub client_pkcs12: Option<Located<String>>,
    /// Passphrase for `client_pkcs12`; meant to reference a secret variable.
    pub client_pkcs12_password: Option<String>,
}

impl fmt::Debug for RequestSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestSettings")
            .field("timeout_ms", &self.timeout_ms)
            .field("connect_timeout_ms", &self.connect_timeout_ms)
            .field("follow_redirects", &self.follow_redirects)
            .field("max_redirects", &self.max_redirects)
            .field("verify_tls", &self.verify_tls)
            .field("http2_prior_knowledge", &self.http2_prior_knowledge)
            .field("proxy", &self.proxy.as_deref().map(without_userinfo))
            .field("cookies", &self.cookies)
            .field("decompress", &self.decompress)
            .field("ca_bundle", &self.ca_bundle)
            .field("client_cert", &self.client_cert)
            .field("client_key", &self.client_key)
            .field("client_pkcs12", &self.client_pkcs12)
            .field(
                "client_pkcs12_password",
                &self
                    .client_pkcs12_password
                    .as_ref()
                    .map(|_| Hidden("client_pkcs12_password")),
            )
            .finish()
    }
}

/// `url` with any `user:password@` replaced by a placeholder.
fn without_userinfo(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(authority_end);
    let Some((_, host)) = authority.rsplit_once('@') else {
        return url.to_owned();
    };
    let prefix = if scheme.is_empty() {
        String::new()
    } else {
        format!("{scheme}://")
    };
    format!("{prefix}{}@{host}{path}", placeholder("proxy_userinfo"))
}

impl RequestSettings {
    fn file_paths(&self) -> impl Iterator<Item = &Located<String>> {
        [
            &self.ca_bundle,
            &self.client_cert,
            &self.client_key,
            &self.client_pkcs12,
        ]
        .into_iter()
        .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_debug_hides_credentials() {
        let settings = RequestSettings {
            proxy: Some("http://user:pw-x@proxy:8080/p".into()),
            client_pkcs12_password: Some("pkcs-x".into()),
            ..RequestSettings::default()
        };
        let debug = format!("{settings:?}");
        assert!(
            !debug.contains("pw-x") && !debug.contains("pkcs-x"),
            "{debug}"
        );
        assert!(
            debug.contains("http://«redacted:proxy_userinfo»@proxy:8080/p"),
            "{debug}"
        );
        assert_eq!(without_userinfo("http://proxy:1"), "http://proxy:1");
        assert_eq!(without_userinfo("none"), "none");
        assert_eq!(without_userinfo("u:p@h"), "«redacted:proxy_userinfo»@h");
    }

    #[test]
    fn method_tokens() {
        assert_eq!(Method::parse("PURGE").unwrap().as_str(), "PURGE");
        assert_eq!(Method::parse("M-SEARCH").unwrap().as_str(), "M-SEARCH");
        assert_eq!(
            Method::parse("GET USERS").unwrap_err(),
            "invalid HTTP method 'GET USERS'"
        );
        assert!(Method::parse("").is_err());
        assert!(Method::parse("GET\n").is_err());
    }
}
