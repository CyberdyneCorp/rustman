//! `securitySchemes` as auth config. Every credential is a `{{variable}}`
//! declared as a secret in the generated environments, so no credential
//! value is ever written (the `openapi-import` spec, "Security schemes
//! become auth config").

use rustman_core::model::{ApiKeyLocation, Auth, OAuth2, OAuth2Grant};
use serde_json::Value;

use super::names;
use crate::openapi::Diagnostic;
use crate::openapi::model::{
    ApiModel, Operation, ParameterLocation, SecurityRequirement, SecurityScheme,
};
use crate::openapi::pointer;

/// The auth of every configured scheme and the secrets they need.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Security {
    schemes: Vec<(String, Auth)>,
    /// Secret placeholders, in scheme order.
    pub secrets: Vec<String>,
    /// The auth of the document-wide requirement: the collection's auth.
    pub collection: Option<Auth>,
}

impl Security {
    /// Map `model`'s schemes, warning about those left unconfigured.
    pub fn of(model: &ApiModel, warnings: &mut Vec<Diagnostic>) -> Self {
        let mut security = Self::default();
        for (name, scheme) in &model.security_schemes {
            match configure(name, scheme) {
                Ok((auth, secrets)) => {
                    security.schemes.push((name.clone(), auth));
                    security.secrets.extend(secrets);
                }
                Err(reason) => warnings.push(unconfigured(name, &reason)),
            }
        }
        security.collection = security.auth_for(&model.security);
        security
    }

    /// The request-level auth of `op`: `None` when it inherits the
    /// collection's, `type: none` when it is public.
    pub fn request(&self, op: &Operation) -> Option<Auth> {
        let own = self.auth_for(op.security.as_ref()?);
        if own == self.collection {
            return None;
        }
        Some(own.unwrap_or(Auth::None))
    }

    /// The auth `op` is sent with: its own requirements' (`None` when it is
    /// public), else the collection's.
    pub fn effective(&self, op: &Operation) -> Option<Auth> {
        match &op.security {
            Some(requirements) => self.auth_for(requirements),
            None => self.collection.clone(),
        }
    }

    /// The auth of the first alternative that is anonymous (no auth) or
    /// whose first scheme is configured.
    fn auth_for(&self, requirements: &[SecurityRequirement]) -> Option<Auth> {
        for requirement in requirements {
            // An empty requirement (`{}`) makes auth optional: none.
            let (scheme, _) = requirement.first()?;
            if let Some((_, auth)) = self.schemes.iter().find(|(n, _)| n == scheme) {
                return Some(auth.clone());
            }
        }
        None
    }
}

fn unconfigured(name: &str, reason: &str) -> Diagnostic {
    let at = pointer::push("/components/securitySchemes", name);
    Diagnostic::warning(
        at,
        format!("security scheme '{name}' ({reason}) is not configured; set up its auth by hand"),
    )
}

/// The `{{variable}}` and secret name of one credential of `scheme`.
fn secret(scheme: &str, field: &str) -> (String, String) {
    let name = format!("{}_{field}", names::variable(scheme));
    (format!("{{{{{name}}}}}"), name)
}

type Configured = (Auth, Vec<String>);

fn configure(name: &str, scheme: &SecurityScheme) -> Result<Configured, String> {
    match scheme {
        SecurityScheme::HttpBasic => {
            let (username, user_secret) = secret(name, "username");
            let (password, password_secret) = secret(name, "password");
            let auth = Auth::Basic { username, password };
            Ok((auth, vec![user_secret, password_secret]))
        }
        SecurityScheme::HttpBearer { .. } => {
            let (token, token_secret) = secret(name, "token");
            Ok((Auth::Bearer { token }, vec![token_secret]))
        }
        SecurityScheme::ApiKey {
            name: key,
            location,
        } => Ok(api_key(name, key, *location)),
        SecurityScheme::OAuth2 { flows } => oauth2(name, flows),
        SecurityScheme::Http { scheme } => Err(format!("http {scheme}")),
        SecurityScheme::OpenIdConnect { .. } => Err("openIdConnect".into()),
        SecurityScheme::MutualTls => Err("mutualTLS".into()),
    }
}

/// An API key: a header or query parameter, or a cookie sent in the
/// `Cookie` header.
fn api_key(scheme: &str, key: &str, location: ParameterLocation) -> Configured {
    let (value, key_secret) = secret(scheme, "key");
    let auth = match location {
        ParameterLocation::Query => Auth::ApiKey {
            name: key.to_owned(),
            value,
            location: Some(ApiKeyLocation::Query),
        },
        ParameterLocation::Cookie => Auth::ApiKey {
            name: "Cookie".to_owned(),
            value: format!("{key}={value}"),
            location: Some(ApiKeyLocation::Header),
        },
        ParameterLocation::Header | ParameterLocation::Path => Auth::ApiKey {
            name: key.to_owned(),
            value,
            location: Some(ApiKeyLocation::Header),
        },
    };
    (auth, vec![key_secret])
}

/// OAuth2 with the client credentials flow, else the authorization code
/// flow (sent with PKCE). Other flows are not configured.
fn oauth2(scheme: &str, flows: &Value) -> Result<Configured, String> {
    let (grant, flow) = if let Some(flow) = flows.get("clientCredentials") {
        (OAuth2Grant::ClientCredentials, flow)
    } else if let Some(flow) = flows.get("authorizationCode") {
        (OAuth2Grant::AuthorizationCode, flow)
    } else {
        return Err("oauth2 without a clientCredentials or authorizationCode flow".into());
    };
    let url = |key: &str| flow.get(key).and_then(Value::as_str).map(str::to_owned);
    let (client_id, id_secret) = secret(scheme, "client_id");
    let (client_secret, client_secret_name) = secret(scheme, "client_secret");
    let scopes: Vec<String> = flow
        .get("scopes")
        .and_then(Value::as_object)
        .map(|s| s.keys().cloned().collect())
        .unwrap_or_default();
    let auth = Auth::OAuth2(OAuth2 {
        grant,
        token_url: url("tokenUrl").unwrap_or_default(),
        auth_url: url("authorizationUrl"),
        client_id,
        client_secret: Some(client_secret),
        scopes: (!scopes.is_empty()).then_some(scopes),
    });
    Ok((auth, vec![id_secret, client_secret_name]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model(schemes: Vec<(&str, SecurityScheme)>, global: &str) -> ApiModel {
        ApiModel {
            title: "t".into(),
            version: "1".into(),
            description: None,
            servers: Vec::new(),
            tags: Vec::new(),
            operations: Vec::new(),
            security: vec![vec![(global.to_owned(), Vec::new())]],
            security_schemes: schemes.into_iter().map(|(n, s)| (n.into(), s)).collect(),
            components: json!({}),
        }
    }

    #[test]
    fn bearer_uses_a_token_secret() {
        let mut warnings = Vec::new();
        let m = model(
            vec![("bearerAuth", SecurityScheme::HttpBearer { format: None })],
            "bearerAuth",
        );
        let security = Security::of(&m, &mut warnings);
        assert_eq!(
            security.collection,
            Some(Auth::Bearer {
                token: "{{bearerAuth_token}}".into()
            })
        );
        assert_eq!(security.secrets, ["bearerAuth_token"]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn api_keys_in_every_location() {
        let key = |location| SecurityScheme::ApiKey {
            name: "sid".into(),
            location,
        };
        let (cookie, _) = api_key("c", "sid", ParameterLocation::Cookie);
        assert_eq!(
            cookie,
            Auth::ApiKey {
                name: "Cookie".into(),
                value: "sid={{c_key}}".into(),
                location: Some(ApiKeyLocation::Header)
            }
        );
        let m = model(vec![("q", key(ParameterLocation::Query))], "q");
        let security = Security::of(&m, &mut Vec::new());
        assert!(matches!(
            security.collection,
            Some(Auth::ApiKey {
                location: Some(ApiKeyLocation::Query),
                ..
            })
        ));
    }

    #[test]
    fn oauth2_flows() {
        let flows = json!({"authorizationCode": {
            "authorizationUrl": "https://id.example.com/authorize",
            "tokenUrl": "https://id.example.com/token",
            "scopes": {"read": "", "write": ""},
        }});
        let (auth, secrets) = oauth2("o", &flows).unwrap();
        let Auth::OAuth2(o) = auth else {
            panic!("not oauth2")
        };
        assert_eq!(o.grant, OAuth2Grant::AuthorizationCode);
        assert_eq!(o.client_id, "{{o_client_id}}");
        assert_eq!(o.scopes, Some(vec!["read".into(), "write".into()]));
        assert_eq!(secrets, ["o_client_id", "o_client_secret"]);
        assert!(oauth2("o", &json!({"implicit": {}})).is_err());
    }

    #[test]
    fn unsupported_schemes_warn_and_stay_unconfigured() {
        let m = model(
            vec![
                ("oidc", SecurityScheme::OpenIdConnect { url: "u".into() }),
                ("mtls", SecurityScheme::MutualTls),
            ],
            "oidc",
        );
        let mut warnings = Vec::new();
        let security = Security::of(&m, &mut warnings);
        assert_eq!(security.collection, None);
        let messages: Vec<String> = warnings.iter().map(ToString::to_string).collect();
        assert_eq!(
            messages,
            [
                "/components/securitySchemes/oidc: security scheme 'oidc' (openIdConnect) is not configured; set up its auth by hand",
                "/components/securitySchemes/mtls: security scheme 'mtls' (mutualTLS) is not configured; set up its auth by hand",
            ]
        );
    }
}
