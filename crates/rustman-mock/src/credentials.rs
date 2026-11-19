//! Which request headers and query parameters carry credentials, so the
//! request log can redact them (spec: Request log): the names the
//! document's `apiKey` security schemes declare, and names that read like
//! a credential (`X-Api-Key`, `X-Auth-Token`, `client_secret`, ...).
//! `Authorization` and `Cookie` are always redacted by the `Redactor`.

use rustman_core::data_rows::secret_by_name;
use rustman_explore::ApiModel;
use rustman_explore::openapi::model::{ParameterLocation, SecurityScheme};

/// Credential header and query parameter names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Credentials {
    /// Declared `apiKey` header names, lowercase.
    headers: Vec<String>,
    /// Declared `apiKey` query parameter names.
    query: Vec<String>,
}

impl Credentials {
    /// The credential names `model` declares.
    pub fn new(model: &ApiModel) -> Self {
        let mut out = Self::default();
        for (_, scheme) in &model.security_schemes {
            let SecurityScheme::ApiKey { name, location } = scheme else {
                continue;
            };
            match location {
                ParameterLocation::Header => out.headers.push(name.to_ascii_lowercase()),
                ParameterLocation::Query => out.query.push(name.clone()),
                ParameterLocation::Path | ParameterLocation::Cookie => {}
            }
        }
        out
    }

    /// Whether the header `name` carries a credential.
    pub fn is_header(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        self.headers.contains(&lower) || credential_name(&lower)
    }

    /// Whether the query parameter `name` carries a credential.
    pub fn is_query(&self, name: &str) -> bool {
        self.query.iter().any(|q| q == name) || credential_name(name)
    }

    /// The credential values `path_and_query` carries, by parameter name.
    pub fn query_values(&self, path_and_query: &str) -> Vec<(String, String)> {
        let Some((_, query)) = path_and_query.split_once('?') else {
            return Vec::new();
        };
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(name, value)| (decode(name), decode(value)))
            .filter(|(name, value)| !value.is_empty() && self.is_query(name))
            .collect()
    }
}

/// A name that reads like a credential: one [`secret_by_name`] accepts
/// (`password`, `token`, `secret`, `api_key`), or one with `apikey` in it
/// or ending in `key` after a separator.
fn credential_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase().replace('-', "_");
    secret_by_name(&lower) || lower.contains("apikey") || lower == "key" || lower.ends_with("_key")
}

fn decode(text: &str) -> String {
    let spaced = text.replace('+', " ");
    percent_encoding::percent_decode_str(&spaced)
        .decode_utf8_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared() -> Credentials {
        Credentials {
            headers: vec!["x-partner".into()],
            query: vec!["sig".into()],
        }
    }

    #[test]
    fn declared_and_credential_like_names() {
        let c = declared();
        assert!(c.is_header("X-Partner"));
        assert!(c.is_header("X-Api-Key"));
        assert!(c.is_header("x-auth-token"));
        assert!(c.is_header("apikey"));
        assert!(!c.is_header("accept"));
        assert!(c.is_query("sig"));
        assert!(c.is_query("api_key"));
        assert!(!c.is_query("limit"));
    }

    #[test]
    fn query_values_are_decoded() {
        let values = declared().query_values("/p?limit=2&api_key=a%2Bb&sig=x+y&token=");
        assert_eq!(
            values,
            [
                ("api_key".into(), "a+b".into()),
                ("sig".into(), "x y".into())
            ]
        );
        assert!(declared().query_values("/p").is_empty());
    }
}
