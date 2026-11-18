//! Request and collection auth: `auth: {type: none|basic|bearer|api_key|oauth2|digest|aws_sigv4|jwt, ...}`.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::common::{Seen, check_fields, exactly_one, seen};
use super::signed_auth::{
    AwsCredentialSource, AwsCredentials, AwsSigV4, DEFAULT_JWT_TTL_SECS, Jwt, JwtAlgorithm,
    json_object, parse_ttl,
};
use crate::format::{Located, error_at};
use crate::secret::Hidden;

pub const AUTH_TYPES: &[&str] = &[
    "none",
    "basic",
    "bearer",
    "api_key",
    "oauth2",
    "digest",
    "aws_sigv4",
    "jwt",
];

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
    /// RFC 7616 Digest: answered after the server's `401` challenge.
    Digest {
        username: String,
        password: String,
    },
    AwsSigV4(Box<AwsSigV4>),
    Jwt(Box<Jwt>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
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
            Self::Digest { username, .. } => f
                .debug_struct("Digest")
                .field("username", username)
                .field("password", &Hidden("password"))
                .finish(),
            Self::AwsSigV4(sigv4) => f.debug_tuple("AwsSigV4").field(sigv4).finish(),
            Self::Jwt(jwt) => f.debug_tuple("Jwt").field(jwt).finish(),
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
    region: Field<String>,
    service: Field<String>,
    access_key: Field<String>,
    secret_key: Field<String>,
    session_token: Field<String>,
    from: Field<AwsCredentialSource>,
    profile: Field<String>,
    unsigned_payload: Field<bool>,
    algorithm: Field<JwtAlgorithm>,
    secret: Field<String>,
    private_key: Field<String>,
    private_key_file: Field<String>,
    claims: Field<serde_json::Value>,
    header: Field<serde_json::Value>,
    ttl: Field<String>,
}

/// An optional field, with the position it was read from.
type Field<T> = Option<Located<T>>;

impl RawAuth {
    fn present(&self) -> [(&'static str, Seen); 27] {
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
            ("region", seen(&self.region)),
            ("service", seen(&self.service)),
            ("access_key", seen(&self.access_key)),
            ("secret_key", seen(&self.secret_key)),
            ("session_token", seen(&self.session_token)),
            ("from", seen(&self.from)),
            ("profile", seen(&self.profile)),
            ("unsigned_payload", seen(&self.unsigned_payload)),
            ("algorithm", seen(&self.algorithm)),
            ("secret", seen(&self.secret)),
            ("private_key", seen(&self.private_key)),
            ("private_key_file", seen(&self.private_key_file)),
            ("claims", seen(&self.claims)),
            ("header", seen(&self.header)),
            ("ttl", seen(&self.ttl)),
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
            "digest" => check(&["username", "password"], &[]).map(|()| Self::Digest {
                username: inner(raw.username),
                password: inner(raw.password),
            }),
            "aws_sigv4" => {
                check(&["region", "service"], SIGV4_OPTIONAL)?;
                aws_sigv4(raw).map(|s| Self::AwsSigV4(Box::new(s)))
            }
            "jwt" => {
                check(&["algorithm"], JWT_OPTIONAL)?;
                jwt(raw).map(|j| Self::Jwt(Box::new(j)))
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

const SIGV4_OPTIONAL: &[&str] = &[
    "access_key",
    "secret_key",
    "session_token",
    "from",
    "profile",
    "unsigned_payload",
];

const JWT_OPTIONAL: &[&str] = &[
    "secret",
    "private_key",
    "private_key_file",
    "claims",
    "header",
    "ttl",
    "in",
    "name",
];

fn aws_sigv4(raw: RawAuth) -> Result<AwsSigV4, String> {
    let source = raw.from.as_ref().map(|f| **f);
    let what = match source {
        None => "auth type 'aws_sigv4'",
        Some(AwsCredentialSource::Env) => "auth type 'aws_sigv4' with 'from: env'",
        Some(AwsCredentialSource::Profile) => "auth type 'aws_sigv4' with 'from: profile'",
    };
    let present = raw.present();
    let check = |required: &[&str], optional: &[&str]| {
        let mut allowed = vec!["region", "service", "from", "unsigned_payload"];
        allowed.extend_from_slice(optional);
        check_fields(what, &present, required, &allowed)
    };
    let credentials = match source {
        None => {
            check(&["access_key", "secret_key"], &["session_token"])?;
            AwsCredentials::Fields {
                access_key: inner(raw.access_key),
                secret_key: inner(raw.secret_key),
                session_token: optional(raw.session_token),
            }
        }
        Some(AwsCredentialSource::Env) => {
            check(&[], &[])?;
            AwsCredentials::Env
        }
        Some(AwsCredentialSource::Profile) => {
            check(&["profile"], &["profile"])?;
            AwsCredentials::Profile(inner(raw.profile))
        }
    };
    Ok(AwsSigV4 {
        region: inner(raw.region),
        service: inner(raw.service),
        credentials,
        unsigned_payload: optional(raw.unsigned_payload).unwrap_or(false),
    })
}

fn jwt(raw: RawAuth) -> Result<Jwt, String> {
    let algorithm = raw.algorithm.as_deref().copied();
    let algorithm = algorithm.unwrap_or(JwtAlgorithm::HS256);
    let what = format!("auth type 'jwt' with algorithm {}", algorithm.as_str());
    let present = raw.present();
    let keys = [
        ("private_key", seen(&raw.private_key)),
        ("private_key_file", seen(&raw.private_key_file)),
    ];
    if algorithm.key_kind().is_some() {
        reject(&what, &present, "secret")?;
        exactly_one(&what, &keys)?;
    } else {
        reject(&what, &keys, "private_key")?;
        reject(&what, &keys, "private_key_file")?;
        if raw.secret.is_none() {
            return Err(format!("{what} requires 'secret'"));
        }
    }
    let location = optional(raw.location).unwrap_or(ApiKeyLocation::Header);
    if location == ApiKeyLocation::Query && raw.name.is_none() {
        return Err("auth type 'jwt' with 'in: query' requires 'name'".to_owned());
    }
    let header = json_object("header", optional(raw.header))?;
    if header.contains_key("alg") {
        return Err("jwt: 'header' cannot set 'alg'; use 'algorithm'".to_owned());
    }
    Ok(Jwt {
        algorithm,
        secret: optional(raw.secret),
        private_key: optional(raw.private_key),
        private_key_file: raw.private_key_file,
        claims: json_object("claims", optional(raw.claims))?,
        header,
        ttl_secs: match raw.ttl {
            Some(ttl) => parse_ttl(&ttl).map_err(|e| error_at(ttl.position(), e))?,
            None => DEFAULT_JWT_TTL_SECS,
        },
        location,
        name: optional(raw.name),
    })
}

/// Fail when `field` was given.
fn reject(what: &str, present: &[(&str, Seen)], field: &str) -> Result<(), String> {
    match present
        .iter()
        .find(|(name, at)| *name == field && at.is_some())
    {
        Some((_, at)) => Err(error_at(
            at.flatten(),
            format!("{what} does not accept '{field}'"),
        )),
        None => Ok(()),
    }
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

    use crate::model::signed_auth::DEFAULT_JWT_TTL_SECS;

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
            auth("type: ntlm")
                .unwrap_err()
                .message
                .starts_with("unknown auth type 'ntlm'")
        );
    }

    #[test]
    fn signature_auth_types_load() {
        assert_eq!(
            auth("{type: digest, username: u, password: p}").unwrap(),
            Auth::Digest {
                username: "u".into(),
                password: "p".into()
            }
        );
        let Auth::AwsSigV4(s) = auth(
            "{type: aws_sigv4, region: us-east-1, service: s3, access_key: '{{k}}', secret_key: '{{s}}'}",
        )
        .unwrap() else {
            panic!("expected aws_sigv4");
        };
        assert!(matches!(
            s.credentials,
            AwsCredentials::Fields {
                session_token: None,
                ..
            }
        ));
        assert!(!s.unsigned_payload);
        let Auth::AwsSigV4(s) =
            auth("{type: aws_sigv4, region: r, service: s, from: env, unsigned_payload: true}")
                .unwrap()
        else {
            panic!("expected aws_sigv4");
        };
        assert_eq!(
            (s.credentials, s.unsigned_payload),
            (AwsCredentials::Env, true)
        );
        let Auth::AwsSigV4(s) =
            auth("{type: aws_sigv4, region: r, service: s, from: profile, profile: dev}").unwrap()
        else {
            panic!("expected aws_sigv4");
        };
        assert_eq!(s.credentials, AwsCredentials::Profile("dev".into()));
        let Auth::Jwt(j) = auth(
            "type: jwt\nalgorithm: HS256\nsecret: '{{jwt_secret}}'\nclaims: {sub: '{{user_id}}', scope: read}\nheader: {kid: k1}\nttl: 10m\nin: query\nname: token",
        )
        .unwrap() else {
            panic!("expected jwt");
        };
        assert_eq!(j.claims["sub"], "{{user_id}}");
        assert_eq!(j.header["kid"], "k1");
        assert_eq!((j.ttl_secs, j.location), (600, ApiKeyLocation::Query));
        let Auth::Jwt(j) =
            auth("{type: jwt, algorithm: ES256, private_key_file: keys/ec.pem}").unwrap()
        else {
            panic!("expected jwt");
        };
        assert_eq!(j.ttl_secs, DEFAULT_JWT_TTL_SECS);
        assert_eq!(
            j.private_key_file.as_deref().map(String::as_str),
            Some("keys/ec.pem")
        );
    }

    #[test]
    fn signature_auth_fields_are_checked() {
        let message = |text: &str| auth(text).unwrap_err().message;
        assert_eq!(
            message("{type: aws_sigv4, region: r, service: s}"),
            "auth type 'aws_sigv4' requires 'access_key'"
        );
        assert_eq!(
            message("{type: aws_sigv4, region: r, service: s, from: env, access_key: a}"),
            "auth type 'aws_sigv4' with 'from: env' does not accept 'access_key'"
        );
        assert_eq!(
            message("{type: aws_sigv4, region: r, service: s, from: profile}"),
            "auth type 'aws_sigv4' with 'from: profile' requires 'profile'"
        );
        assert_eq!(
            message("{type: jwt, algorithm: HS256}"),
            "auth type 'jwt' with algorithm HS256 requires 'secret'"
        );
        assert_eq!(
            message("{type: jwt, algorithm: RS256, secret: s, private_key: k}"),
            "auth type 'jwt' with algorithm RS256 does not accept 'secret'"
        );
        assert_eq!(
            message("{type: jwt, algorithm: ES256}"),
            "auth type 'jwt' with algorithm ES256 needs exactly one of: private_key, private_key_file"
        );
        assert_eq!(
            message("{type: jwt, algorithm: HS256, secret: s, private_key: k}"),
            "auth type 'jwt' with algorithm HS256 does not accept 'private_key'"
        );
        assert_eq!(
            message("{type: jwt, algorithm: HS256, secret: s, in: query}"),
            "auth type 'jwt' with 'in: query' requires 'name'"
        );
        assert_eq!(
            message("{type: jwt, algorithm: HS256, secret: s, header: {alg: none}}"),
            "jwt: 'header' cannot set 'alg'; use 'algorithm'"
        );
        assert_eq!(
            message("{type: jwt, algorithm: HS256, secret: s, claims: [a]}"),
            "jwt: 'claims' must be a mapping"
        );
        assert_eq!(
            message("{type: jwt, algorithm: HS256, secret: s, ttl: 5d}"),
            "jwt: invalid ttl '5d' (expected e.g. 30s, 5m or 1h)"
        );
        assert_eq!(
            message("{type: digest, username: u, password: p, token: t}"),
            "auth type 'digest' does not accept 'token'"
        );
    }

    #[test]
    fn signature_auth_debug_hides_credentials() {
        let digest = auth("{type: digest, username: u, password: dg-x}").unwrap();
        assert!(!format!("{digest:?}").contains("dg-x"));
    }

    #[test]
    fn field_errors_point_at_the_offending_key() {
        let err = auth("type: bearer\ntoken: t\nusername: a").unwrap_err();
        assert_eq!((err.line(), err.column()), (Some(3), Some(11)));
        let err = auth("{type: bearer, username: a}").unwrap_err();
        assert_eq!((err.line(), err.column()), (Some(1), Some(26)));
        let err = auth("type: ntlm").unwrap_err();
        assert_eq!((err.line(), err.column()), (Some(1), Some(7)));
    }
}
