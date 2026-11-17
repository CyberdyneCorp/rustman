//! Request and collection auth: `auth: {type: none|basic|bearer|api_key|oauth2, ...}`.

use std::fmt;

use serde::Deserialize;

use super::common::{Seen, check_fields, seen};
use crate::format::{Located, error_at};
use crate::secret::Hidden;

pub const AUTH_TYPES: &[&str] = &["none", "basic", "bearer", "api_key", "oauth2"];

/// `Debug` never shows credentials (passwords, tokens, key values, client
/// secrets), whether written literally or resolved from a secret.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(try_from = "RawAuth")]
pub enum Auth {
    /// Explicitly no auth; stops inheritance from collections.
    None,
    Basic {
        username: String,
        password: String,
    },
    Bearer {
        token: String,
    },
    ApiKey {
        name: String,
        value: String,
        location: Option<ApiKeyLocation>,
    },
    OAuth2(OAuth2),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyLocation {
    Header,
    Query,
}

impl ApiKeyLocation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::Query => "query",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuth2Grant {
    ClientCredentials,
    AuthorizationCode,
}

impl OAuth2Grant {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClientCredentials => "client_credentials",
            Self::AuthorizationCode => "authorization_code",
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct OAuth2 {
    pub grant: OAuth2Grant,
    pub token_url: String,
    pub auth_url: Option<String>,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scopes: Option<Vec<String>>,
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("None"),
            Self::Basic { username, .. } => f
                .debug_struct("Basic")
                .field("username", username)
                .field("password", &Hidden("password"))
                .finish(),
            Self::Bearer { .. } => f
                .debug_struct("Bearer")
                .field("token", &Hidden("token"))
                .finish(),
            Self::ApiKey { name, location, .. } => f
                .debug_struct("ApiKey")
                .field("name", name)
                .field("value", &Hidden("value"))
                .field("location", location)
                .finish(),
            Self::OAuth2(oauth) => f.debug_tuple("OAuth2").field(oauth).finish(),
        }
    }
}

impl fmt::Debug for OAuth2 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuth2")
            .field("grant", &self.grant)
            .field("token_url", &self.token_url)
            .field("auth_url", &self.auth_url)
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| Hidden("client_secret")),
            )
            .field("scopes", &self.scopes)
            .finish()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAuth {
    #[serde(rename = "type")]
    kind: Located<String>,
    username: Field<String>,
    password: Field<String>,
    token: Field<String>,
    name: Field<String>,
    value: Field<String>,
    #[serde(rename = "in")]
    location: Field<ApiKeyLocation>,
    grant: Field<OAuth2Grant>,
    token_url: Field<String>,
    auth_url: Field<String>,
    client_id: Field<String>,
    client_secret: Field<String>,
    scopes: Field<Vec<String>>,
}

/// An optional field, with the position it was read from.
type Field<T> = Option<Located<T>>;

impl RawAuth {
    fn present(&self) -> [(&'static str, Seen); 12] {
        [
            ("username", seen(&self.username)),
            ("password", seen(&self.password)),
            ("token", seen(&self.token)),
            ("name", seen(&self.name)),
            ("value", seen(&self.value)),
            ("in", seen(&self.location)),
            ("grant", seen(&self.grant)),
            ("token_url", seen(&self.token_url)),
            ("auth_url", seen(&self.auth_url)),
            ("client_id", seen(&self.client_id)),
            ("client_secret", seen(&self.client_secret)),
            ("scopes", seen(&self.scopes)),
        ]
    }
}

impl TryFrom<RawAuth> for Auth {
    type Error = String;

    fn try_from(raw: RawAuth) -> Result<Self, String> {
        let present = raw.present();
        let what = format!("auth type '{}'", *raw.kind);
        let check = |required: &[&str], optional: &[&str]| {
            check_fields(&what, &present, required, optional)
        };
        match raw.kind.as_str() {
            "none" => check(&[], &[]).map(|()| Self::None),
            "basic" => check(&["username", "password"], &[]).map(|()| Self::Basic {
                username: inner(raw.username),
                password: inner(raw.password),
            }),
            "bearer" => check(&["token"], &[]).map(|()| Self::Bearer {
                token: inner(raw.token),
            }),
            "api_key" => check(&["name", "value"], &["in"]).map(|()| Self::ApiKey {
                name: inner(raw.name),
                value: inner(raw.value),
                location: optional(raw.location),
            }),
            "oauth2" => {
                check(
                    &["grant", "token_url", "client_id"],
                    &["auth_url", "client_secret", "scopes"],
                )?;
                oauth2(raw).map(Self::OAuth2)
            }
            other => Err(error_at(
                raw.kind.position(),
                format!(
                    "unknown auth type '{other}' (expected one of: {})",
                    AUTH_TYPES.join(", ")
                ),
            )),
        }
    }
}

fn inner<T: Default>(field: Field<T>) -> T {
    field.map(Located::into_inner).unwrap_or_default()
}

fn optional<T>(field: Field<T>) -> Option<T> {
    field.map(Located::into_inner)
}

fn oauth2(raw: RawAuth) -> Result<OAuth2, String> {
    let grant_at = raw.grant.as_ref().and_then(Located::position);
    let grant = optional(raw.grant).unwrap_or(OAuth2Grant::ClientCredentials);
    if grant == OAuth2Grant::AuthorizationCode && raw.auth_url.is_none() {
        return Err(error_at(
            grant_at,
            "auth type 'oauth2' with grant 'authorization_code' requires 'auth_url'",
        ));
    }
    Ok(OAuth2 {
        grant,
        token_url: inner(raw.token_url),
        auth_url: optional(raw.auth_url),
        client_id: inner(raw.client_id),
        client_secret: optional(raw.client_secret),
        scopes: optional(raw.scopes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn auth(text: &str) -> Result<Auth, crate::error::LoadError> {
        crate::format::from_yaml(Path::new("a.yaml"), text)
    }

    #[test]
    fn every_auth_type_loads() {
        assert_eq!(auth("type: none").unwrap(), Auth::None);
        assert_eq!(
            auth("type: basic\nusername: ana\npassword: pw").unwrap(),
            Auth::Basic {
                username: "ana".into(),
                password: "pw".into()
            }
        );
        assert_eq!(
            auth("type: bearer\ntoken: t").unwrap(),
            Auth::Bearer { token: "t".into() }
        );
        assert_eq!(
            auth("{type: api_key, name: key, value: '{{k}}', in: query}").unwrap(),
            Auth::ApiKey {
                name: "key".into(),
                value: "{{k}}".into(),
                location: Some(ApiKeyLocation::Query)
            }
        );
        let Auth::OAuth2(o) = auth(
            "type: oauth2\ngrant: authorization_code\ntoken_url: t\nauth_url: a\nclient_id: c\nscopes: [read]",
        )
        .unwrap() else {
            panic!("expected oauth2");
        };
        assert_eq!(o.grant, OAuth2Grant::AuthorizationCode);
        assert_eq!(o.scopes, Some(vec!["read".to_owned()]));
    }

    #[test]
    fn auth_fields_are_checked() {
        assert_eq!(
            auth("type: bearer\ntoken: t\nusername: x")
                .unwrap_err()
                .message,
            "auth type 'bearer' does not accept 'username'"
        );
        assert_eq!(
            auth("type: basic\nusername: x").unwrap_err().message,
            "auth type 'basic' requires 'password'"
        );
        assert_eq!(
            auth("type: oauth2\ngrant: authorization_code\ntoken_url: t\nclient_id: c")
                .unwrap_err()
                .message,
            "auth type 'oauth2' with grant 'authorization_code' requires 'auth_url'"
        );
        assert!(
            auth("type: digest")
                .unwrap_err()
                .message
                .starts_with("unknown auth type 'digest'")
        );
    }

    #[test]
    fn field_errors_point_at_the_offending_key() {
        let err = auth("type: bearer\ntoken: t\nusername: a").unwrap_err();
        assert_eq!((err.line(), err.column()), (Some(3), Some(11)));
        let err = auth("{type: bearer, username: a}").unwrap_err();
        assert_eq!((err.line(), err.column()), (Some(1), Some(26)));
        let err = auth("type: digest").unwrap_err();
        assert_eq!((err.line(), err.column()), (Some(1), Some(7)));
    }
}
