//! Basic, bearer, API-key and OAuth2 credentials on the outgoing request.
//!
//! Auth fields arrive already interpolated. An explicit header of the same
//! name (`Authorization`, or the API key's header) always wins.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use http::header::AUTHORIZATION;
use http::{HeaderName, HeaderValue, Request, Uri};
use http_body_util::Full;
use rustman_core::Secret;
use rustman_core::model::{ApiKeyLocation, Auth, Jwt};

use crate::error::RequestError;
use crate::redirect::Credentials;

/// Add the credentials of `auth` to `request`. `oauth2_token` is the access
/// token for `type: oauth2`, or the signed token for `type: jwt`. Digest
/// answers a challenge and SigV4 signs each hop, so neither adds anything
/// here.
pub(crate) fn apply(
    request: &mut Request<Full<Bytes>>,
    auth: Option<&Auth>,
    oauth2_token: Option<&Secret<String>>,
) -> Result<(), RequestError> {
    match auth {
        None | Some(Auth::None) => Ok(()),
        Some(Auth::Basic { username, password }) => {
            let encoded = STANDARD.encode(format!("{username}:{password}"));
            set_header(request, AUTHORIZATION, &format!("Basic {encoded}"))
        }
        Some(Auth::Bearer { token }) => {
            set_header(request, AUTHORIZATION, &format!("Bearer {token}"))
        }
        Some(Auth::ApiKey {
            name,
            value,
            location,
        }) => match location.unwrap_or(ApiKeyLocation::Header) {
            ApiKeyLocation::Header => {
                let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                    RequestError::Invalid(format!("invalid api_key header name '{name}'"))
                })?;
                set_header(request, name, value)
            }
            ApiKeyLocation::Query => append_query(request, name, value),
        },
        Some(Auth::OAuth2(_)) => {
            let token = oauth2_token
                .ok_or_else(|| RequestError::Auth("oauth2: no access token was acquired".into()))?;
            set_header(
                request,
                AUTHORIZATION,
                &format!("Bearer {}", token.expose()),
            )
        }
        Some(Auth::Jwt(jwt)) => {
            let token = oauth2_token
                .ok_or_else(|| RequestError::Auth("jwt: no token was signed".into()))?;
            place_jwt(request, jwt, token.expose())
        }
        Some(Auth::Digest { .. } | Auth::AwsSigV4(_)) => Ok(()),
    }
}

/// `Authorization: Bearer <jwt>` by default, else the named header or
/// query parameter.
fn place_jwt(
    request: &mut Request<Full<Bytes>>,
    jwt: &Jwt,
    token: &str,
) -> Result<(), RequestError> {
    let name = jwt.name.as_deref();
    match (jwt.location, name) {
        (ApiKeyLocation::Query, name) => append_query(request, name.unwrap_or_default(), token),
        (ApiKeyLocation::Header, None) => {
            set_header(request, AUTHORIZATION, &format!("Bearer {token}"))
        }
        (ApiKeyLocation::Header, Some(name)) => {
            let header = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| RequestError::Invalid(format!("invalid jwt header name '{name}'")))?;
            let value = if header == AUTHORIZATION {
                format!("Bearer {token}")
            } else {
                token.to_owned()
            };
            set_header(request, header, &value)
        }
    }
}

/// The `api_key` header or query parameter of `auth`, which a redirect to
/// another origin drops.
pub(crate) fn credentials(auth: Option<&Auth>) -> Credentials {
    let Some(Auth::ApiKey { name, location, .. }) = auth else {
        return Credentials::default();
    };
    match location.unwrap_or(ApiKeyLocation::Header) {
        ApiKeyLocation::Header => Credentials {
            header: HeaderName::from_bytes(name.as_bytes()).ok(),
            query: None,
        },
        ApiKeyLocation::Query => Credentials {
            header: None,
            query: Some(name.clone()),
        },
    }
}

/// Set `name` unless the request already carries it.
fn set_header(
    request: &mut Request<Full<Bytes>>,
    name: HeaderName,
    value: &str,
) -> Result<(), RequestError> {
    if request.headers().contains_key(&name) {
        return Ok(());
    }
    let value = HeaderValue::from_str(value)
        .map_err(|_| RequestError::Invalid(format!("invalid value for header '{name}'")))?;
    request.headers_mut().insert(name, value);
    Ok(())
}

fn append_query(
    request: &mut Request<Full<Bytes>>,
    name: &str,
    value: &str,
) -> Result<(), RequestError> {
    let uri = request.uri().to_string();
    let pair = form_urlencoded::Serializer::new(String::new())
        .append_pair(name, value)
        .finish();
    let separator = match uri.split_once('?') {
        None => "?",
        Some((_, "")) => "",
        Some(_) => "&",
    };
    let next: Uri = format!("{uri}{separator}{pair}")
        .parse()
        .map_err(|e| RequestError::Invalid(format!("invalid URL with api_key: {e}")))?;
    *request.uri_mut() = next;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::model::{OAuth2, OAuth2Grant};

    fn request(uri: &str) -> Request<Full<Bytes>> {
        Request::get(uri).body(Full::new(Bytes::new())).unwrap()
    }

    fn header(request: &Request<Full<Bytes>>, name: &str) -> Option<String> {
        request
            .headers()
            .get(name)
            .map(|v| v.to_str().unwrap().to_owned())
    }

    fn api_key(location: Option<ApiKeyLocation>) -> Auth {
        Auth::ApiKey {
            name: "key".into(),
            value: "1 2".into(),
            location,
        }
    }

    #[test]
    fn basic_is_base64_of_user_and_password() {
        let mut r = request("http://h/");
        let auth = Auth::Basic {
            username: "ana".into(),
            password: "pw".into(),
        };
        apply(&mut r, Some(&auth), None).unwrap();
        assert_eq!(header(&r, "authorization").unwrap(), "Basic YW5hOnB3");
    }

    #[test]
    fn bearer_sets_the_token() {
        let mut r = request("http://h/");
        apply(&mut r, Some(&Auth::Bearer { token: "t".into() }), None).unwrap();
        assert_eq!(header(&r, "authorization").unwrap(), "Bearer t");
    }

    #[test]
    fn an_explicit_authorization_header_wins() {
        let mut r = request("http://h/");
        r.headers_mut()
            .insert(AUTHORIZATION, HeaderValue::from_static("Custom x"));
        apply(&mut r, Some(&Auth::Bearer { token: "t".into() }), None).unwrap();
        assert_eq!(header(&r, "authorization").unwrap(), "Custom x");
    }

    #[test]
    fn api_keys_go_in_a_header_by_default() {
        let mut r = request("http://h/");
        apply(&mut r, Some(&api_key(None)), None).unwrap();
        assert_eq!(header(&r, "key").unwrap(), "1 2");
        let mut r = request("http://h/");
        r.headers_mut()
            .insert("key", HeaderValue::from_static("mine"));
        apply(&mut r, Some(&api_key(Some(ApiKeyLocation::Header))), None).unwrap();
        assert_eq!(header(&r, "key").unwrap(), "mine");
    }

    #[test]
    fn api_keys_in_the_query_are_encoded_and_appended() {
        let query = Some(ApiKeyLocation::Query);
        for (uri, expected) in [
            ("http://h/p", "http://h/p?key=1+2"),
            ("http://h/p?a=1", "http://h/p?a=1&key=1+2"),
            ("http://h/p?", "http://h/p?key=1+2"),
        ] {
            let mut r = request(uri);
            apply(&mut r, Some(&api_key(query)), None).unwrap();
            assert_eq!(r.uri().to_string(), expected);
        }
    }

    #[test]
    fn api_keys_are_credentials_that_redirects_drop() {
        let header = credentials(Some(&api_key(None)));
        assert_eq!(header.header.as_ref().map(HeaderName::as_str), Some("key"));
        assert_eq!(header.query, None);
        let query = credentials(Some(&api_key(Some(ApiKeyLocation::Query))));
        assert_eq!((query.header, query.query.as_deref()), (None, Some("key")));
        let bearer = Auth::Bearer { token: "t".into() };
        assert_eq!(credentials(Some(&bearer)), Credentials::default());
        assert_eq!(credentials(None), Credentials::default());
    }

    #[test]
    fn invalid_values_are_reported() {
        let mut r = request("http://h/");
        let auth = Auth::Bearer {
            token: "a\nb".into(),
        };
        let err = apply(&mut r, Some(&auth), None).unwrap_err();
        assert_eq!(err.to_string(), "invalid value for header 'authorization'");
        let auth = Auth::ApiKey {
            name: "bad name".into(),
            value: "v".into(),
            location: None,
        };
        let err = apply(&mut r, Some(&auth), None).unwrap_err();
        assert_eq!(err.to_string(), "invalid api_key header name 'bad name'");
    }

    #[test]
    fn oauth2_uses_the_acquired_token() {
        let auth = Auth::OAuth2(OAuth2 {
            grant: OAuth2Grant::ClientCredentials,
            token_url: "http://h/token".into(),
            auth_url: None,
            client_id: "c".into(),
            client_secret: None,
            scopes: None,
        });
        let mut r = request("http://h/");
        let err = apply(&mut r, Some(&auth), None).unwrap_err();
        assert_eq!(err.to_string(), "oauth2: no access token was acquired");
        let token = Secret::new("oauth2_token", "tok".to_owned());
        apply(&mut r, Some(&auth), Some(&token)).unwrap();
        assert_eq!(header(&r, "authorization").unwrap(), "Bearer tok");
    }

    #[test]
    fn none_adds_nothing() {
        let mut r = request("http://h/");
        apply(&mut r, Some(&Auth::None), None).unwrap();
        apply(&mut r, None, None).unwrap();
        assert!(r.headers().is_empty());
    }
}
