//! Postman auth: `basic`, `bearer`, `apikey` and `oauth2` (client
//! credentials, authorization code) map to `http-requests` auth; `noauth`
//! disables inherited auth and `inherit` keeps it; every other type is
//! reported as unsupported and written as `none`, so the request never
//! inherits a parent credential Postman would not have sent.

use serde_json::Value;

use super::{Context, string};
use crate::ir::{ApiKeyIn, Grant, ImportedAuth, ImportedOAuth2, Location, NoteKind};

/// The auth object `v`; `None` when absent, [`ImportedAuth::None`] when
/// unsupported.
pub(super) fn parse(v: &Value, at: &Location, ctx: &mut Context) -> Option<ImportedAuth> {
    let kind = v["type"].as_str()?;
    let params = Params::of(&v[kind]);
    let auth = match kind {
        // Postman's explicit "inherit auth from parent".
        "inherit" => return None,
        "noauth" => ImportedAuth::None,
        "basic" => ImportedAuth::Basic {
            username: params.get("username"),
            password: params.get("password"),
        },
        "bearer" => ImportedAuth::Bearer {
            token: params.get("token"),
        },
        "apikey" => ImportedAuth::ApiKey {
            name: params.get("key"),
            value: params.get("value"),
            location: if params.get("in") == "query" {
                ApiKeyIn::Query
            } else {
                ApiKeyIn::Header
            },
        },
        "oauth2" => return oauth2(&params, at, ctx),
        other => {
            ctx.note(
                NoteKind::Unsupported,
                format!("{other} auth unsupported"),
                at,
            );
            return Some(ImportedAuth::None);
        }
    };
    Some(auth)
}

fn oauth2(params: &Params, at: &Location, ctx: &mut Context) -> Option<ImportedAuth> {
    let grant = match params.get("grant_type").as_str() {
        "client_credentials" => Grant::ClientCredentials,
        // Postman's default grant, with or without PKCE.
        "" | "authorization_code" | "authorization_code_with_pkce" => Grant::AuthorizationCode,
        other => {
            let message = format!("oauth2 auth with grant '{other}' unsupported");
            ctx.note(NoteKind::Unsupported, message, at);
            return Some(ImportedAuth::None);
        }
    };
    let token_url = params.get("accessTokenUrl");
    if token_url.is_empty() {
        let message = "oauth2 auth without an access token URL unsupported";
        ctx.note(NoteKind::Unsupported, message, at);
        return Some(ImportedAuth::None);
    }
    if !params.get("accessToken").is_empty() {
        let message = "oauth2 access token dropped; Rustman fetches its own token";
        ctx.note(NoteKind::Dropped, message, at);
    }
    let optional = |key| Some(params.get(key)).filter(|v| !v.is_empty());
    let auth_url = match grant {
        Grant::AuthorizationCode => optional("authUrl"),
        Grant::ClientCredentials => None,
    };
    Some(ImportedAuth::Oauth2(ImportedOAuth2 {
        grant,
        token_url,
        auth_url,
        client_id: params.get("clientId"),
        client_secret: optional("clientSecret"),
        scopes: params
            .get("scope")
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
    }))
}

/// Auth parameters: a list of `{key, value}` (v2.1) or an object (v2.0).
struct Params(Vec<(String, String)>);

impl Params {
    fn of(v: &Value) -> Self {
        let pairs = match v {
            Value::Array(items) => items
                .iter()
                .filter_map(|p| Some((string(&p["key"])?, string(&p["value"]).unwrap_or_default())))
                .collect(),
            Value::Object(map) => map
                .iter()
                .filter_map(|(k, v)| Some((k.clone(), string(v)?)))
                .collect(),
            _ => Vec::new(),
        };
        Self(pairs)
    }

    /// The value of `key`, or an empty string.
    fn get(&self, key: &str) -> String {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }
}
