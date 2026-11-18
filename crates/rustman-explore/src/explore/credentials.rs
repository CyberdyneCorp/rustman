//! Keeping credentials out of findings, whatever their source: a declared
//! secret, or a plain environment variable (the `test-reports` redaction
//! requirement). Reproductions keep every credential as its `{{name}}`
//! reference, evidence shows credential headers as placeholders, and the
//! redactor of each exchange also learns the plain values of the
//! identities' credentials, so a response that echoes one is redacted too.

use rustman_core::Redactor;
use rustman_core::model::{ApiKeyLocation, Auth, AwsCredentials, HttpRequest, VarValue};
use rustman_core::redact::{MIN_HEADER_CREDENTIAL, sensitive_header};
use rustman_core::secret::{placeholder, placeholders};
use rustman_core::vars::ResolvedRequest;

use super::exec::Identity;
use super::plan::escape;

/// The API key header a report never shows, besides the redactor's own
/// sensitive headers.
pub const X_API_KEY: &str = "X-Api-Key";

/// The name a report shows instead of the value of header `name`, when that
/// value is a credential: a sensitive header, `X-Api-Key`, or the header of
/// the API key `auth` sends.
pub fn credential_header(name: &str, auth: Option<&Auth>) -> Option<String> {
    let name = name.trim();
    if let Some(canonical) = sensitive_header(name) {
        return Some(canonical.to_owned());
    }
    if name.eq_ignore_ascii_case(X_API_KEY) {
        return Some(X_API_KEY.to_owned());
    }
    match auth {
        Some(Auth::ApiKey { name: key, .. }) if key.eq_ignore_ascii_case(name) => Some(key.clone()),
        _ => None,
    }
}

/// `template` with every credential left unresolved: its auth fields and
/// the values of its credential headers resolve to their own `{{name}}`
/// references instead of their values.
pub fn keep_references(mut template: HttpRequest) -> HttpRequest {
    let auth = template.auth.clone();
    for header in template.headers.iter_mut().flatten() {
        if credential_header(&header.name, auth.as_ref()).is_some() {
            header.value = escape(&header.value);
        }
    }
    template.auth = template.auth.map(escape_auth);
    template
}

fn escape_auth(auth: Auth) -> Auth {
    match auth {
        Auth::Basic { username, password } => Auth::Basic {
            username: escape(&username),
            password: escape(&password),
        },
        Auth::Bearer { token } => Auth::Bearer {
            token: escape(&token),
        },
        Auth::ApiKey {
            name,
            value,
            location,
        } => Auth::ApiKey {
            name,
            value: escape(&value),
            location,
        },
        Auth::Digest { username, password } => Auth::Digest {
            username: escape(&username),
            password: escape(&password),
        },
        Auth::AwsSigV4(mut sigv4) => {
            if let AwsCredentials::Fields {
                access_key,
                secret_key,
                session_token,
            } = &mut sigv4.credentials
            {
                *access_key = escape(access_key);
                *secret_key = escape(secret_key);
                *session_token = session_token.as_deref().map(escape);
            }
            Auth::AwsSigV4(sigv4)
        }
        Auth::Jwt(mut jwt) => {
            jwt.secret = jwt.secret.as_deref().map(escape);
            jwt.private_key = jwt.private_key.as_deref().map(escape);
            Auth::Jwt(jwt)
        }
        other @ (Auth::None | Auth::OAuth2(_)) => other,
    }
}

/// A credential header value, already `redacted`, as a report shows it:
/// the placeholders of the secrets it carries, or else the header's own
/// placeholder. Never the value, nor the parts the redactor did not know.
pub fn shown_credential(header: &str, redacted: &str) -> String {
    let shown = placeholders(redacted);
    if shown.is_empty() {
        placeholder(header)
    } else {
        shown
    }
}

/// The headers `auth` adds to a request, as a report shows them.
pub fn auth_headers(auth: Option<&Auth>, redactor: &Redactor) -> Vec<(String, String)> {
    const AUTHORIZATION: &str = "Authorization";
    let shown = |value: &str| shown_credential(AUTHORIZATION, &redactor.text(value));
    let header = match auth {
        Some(Auth::Bearer { token }) => {
            (AUTHORIZATION.to_owned(), format!("Bearer {}", shown(token)))
        }
        Some(Auth::Basic { username, password }) => (
            AUTHORIZATION.to_owned(),
            format!("Basic {}", shown(&format!("{username}:{password}"))),
        ),
        Some(Auth::ApiKey {
            name,
            value,
            location: None | Some(ApiKeyLocation::Header),
        }) => (name.clone(), shown_credential(name, &redactor.text(value))),
        _ => return Vec::new(),
    };
    vec![header]
}

/// Teach `redactor` the credentials of `request` it would not know: the
/// API key it sends (whatever the header or query parameter is called) and
/// the values of its `X-Api-Key` headers. Short values are left out, as the
/// redactor leaves out short header credentials.
pub fn add_credentials(redactor: &mut Redactor, request: &ResolvedRequest) {
    if let Some(Auth::ApiKey { name, value, .. }) = &request.auth {
        add_long(redactor, name, value);
    }
    let api_keys = request
        .headers
        .iter()
        .filter(|h| h.name.text.trim().eq_ignore_ascii_case(X_API_KEY));
    for header in api_keys {
        add_long(redactor, X_API_KEY, &header.value.text);
    }
}

/// Teach `redactor` the plain values the identities' environments give the
/// credential variables `names` (a secret is learned from the store).
pub fn add_plain_credentials<'a>(
    redactor: &mut Redactor,
    identities: impl Iterator<Item = Identity<'a>>,
    names: &[String],
) {
    let environments = identities.filter_map(|i| i.environment);
    for env in environments {
        for name in names {
            if let Some(VarValue::String(value)) = env.variables.get(name) {
                add_long(redactor, name, value);
            }
        }
    }
}

fn add_long(redactor: &mut Redactor, name: &str, value: &str) {
    if value.chars().count() >= MIN_HEADER_CREDENTIAL {
        redactor.add_value(name, value);
    }
}

#[cfg(test)]
mod tests {
    use rustman_core::model::{Method, NameValue, REQUEST_VERSION};

    use super::*;

    fn template(auth: Auth, headers: &[(&str, &str)]) -> HttpRequest {
        let headers = headers
            .iter()
            .map(|(n, v)| NameValue::new(*n, *v))
            .collect();
        HttpRequest {
            version: REQUEST_VERSION,
            kind: "http".into(),
            name: "t".into(),
            description: None,
            tags: None,
            method: Method::parse("GET").ok(),
            url: "http://h/x".into(),
            query: None,
            headers: Some(headers),
            body: None,
            auth: Some(auth),
            settings: None,
            variables: None,
            assertions: None,
            captures: None,
        }
    }

    #[test]
    fn credentials_stay_references_and_other_values_resolve() {
        let auth = Auth::Bearer {
            token: "{{token}}".into(),
        };
        let kept = keep_references(template(
            auth,
            &[("X-Api-Key", "{{key}}"), ("X-Trace", "{{trace}}")],
        ));
        assert_eq!(
            kept.auth,
            Some(Auth::Bearer {
                token: "\\{{token}}".into()
            })
        );
        let headers: Vec<&str> = kept
            .headers
            .iter()
            .flatten()
            .map(|h| h.value.as_str())
            .collect();
        assert_eq!(headers, ["\\{{key}}", "{{trace}}"]);
    }

    #[test]
    fn signature_auth_credentials_stay_references() {
        let digest = Auth::Digest {
            username: "{{user}}".into(),
            password: "{{pass}}".into(),
        };
        assert_eq!(
            keep_references(template(digest, &[])).auth,
            Some(Auth::Digest {
                username: "\\{{user}}".into(),
                password: "\\{{pass}}".into(),
            })
        );
        let sigv4 = |key: &str, secret: &str| {
            Auth::AwsSigV4(Box::new(rustman_core::model::AwsSigV4 {
                region: "us-east-1".into(),
                service: "s3".into(),
                credentials: AwsCredentials::Fields {
                    access_key: key.into(),
                    secret_key: secret.into(),
                    session_token: None,
                },
                unsigned_payload: false,
            }))
        };
        assert_eq!(
            keep_references(template(sigv4("{{ak}}", "{{sk}}"), &[])).auth,
            Some(sigv4("\\{{ak}}", "\\{{sk}}"))
        );
    }

    #[test]
    fn credential_headers_include_the_api_key_header() {
        let key = Auth::ApiKey {
            name: "X-Token".into(),
            value: "v".into(),
            location: None,
        };
        assert_eq!(credential_header("cookie", None).as_deref(), Some("Cookie"));
        assert_eq!(
            credential_header("x-api-key", None).as_deref(),
            Some(X_API_KEY)
        );
        assert_eq!(
            credential_header("x-token", Some(&key)).as_deref(),
            Some("X-Token")
        );
        assert_eq!(credential_header("X-Token", None), None);
    }

    #[test]
    fn auth_headers_never_show_a_value() {
        let mut redactor = Redactor::new();
        redactor.add_value("bearerAuth_token", "s3cr3t-token");
        let bearer = |token: &str| {
            let auth = Auth::Bearer {
                token: token.into(),
            };
            auth_headers(Some(&auth), &redactor)
        };
        assert_eq!(
            bearer("s3cr3t-token"),
            [(
                "Authorization".to_owned(),
                "Bearer «redacted:bearerAuth_token»".to_owned()
            )]
        );
        assert_eq!(bearer("tok")[0].1, "Bearer «redacted:Authorization»");
        let key = Auth::ApiKey {
            name: "X-Api-Key".into(),
            value: "abc".into(),
            location: Some(ApiKeyLocation::Header),
        };
        assert_eq!(
            auth_headers(Some(&key), &redactor),
            [("X-Api-Key".to_owned(), "«redacted:X-Api-Key»".to_owned())]
        );
        assert!(auth_headers(Some(&Auth::None), &redactor).is_empty());
    }
}
