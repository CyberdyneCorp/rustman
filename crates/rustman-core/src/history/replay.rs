//! What `rustman history replay` re-sends: the resolved request with every
//! secret turned back into its `{{name}}` placeholder (request-history spec
//! "Inspect and replay"). Replaying resolves those placeholders from the
//! current secret store, so the stored entry never holds a secret value and
//! a deleted secret fails with `unresolved variable(s): <name>`.
//!
//! A credential written literally (an `Authorization` header or auth token
//! that uses no secret variable) is not kept, as the redaction invariant
//! requires; such an entry cannot be replayed. Multipart and file bodies are
//! not kept either. Neither is a value derived from a secret that the
//! secret store cannot give back (a captured value that held a secret).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::template::{Templater, Withheld};
use crate::model::{ApiKeyLocation, Auth, OAuth2, OAuth2Grant, RequestSettings};
use crate::redact::{Redactor, sensitive_header};
use crate::secret::Secret;
use crate::vars::{
    Interpolator, Layer, Lookup, ResolveError, ResolvedBody, ResolvedFile, ResolvedPair,
    ResolvedRequest, Resolver, SecretLookup, TlsFiles, VarSource,
};

/// A resolved request as templates over its secrets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplayRequest {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub query: Vec<ReplayPair>,
    #[serde(default)]
    pub headers: Vec<ReplayPair>,
    pub body: ReplayBody,
    pub auth: Option<ReplayAuth>,
    #[serde(default)]
    pub settings: ReplaySettings,
    #[serde(default)]
    pub tls: ReplayTls,
    /// Where the OAuth2 token of this request is cached.
    pub auth_cache_key: Option<String>,
    /// Parts that were not kept, so the request cannot be replayed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub withheld: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplayPair {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReplayBody {
    None,
    Json { value: Value },
    JsonText { text: String },
    Text { text: String },
    Form { fields: Vec<ReplayPair> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReplayAuth {
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
    #[serde(rename = "oauth2")]
    OAuth2 {
        grant: OAuth2Grant,
        token_url: String,
        auth_url: Option<String>,
        client_id: String,
        client_secret: Option<String>,
        scopes: Option<Vec<String>>,
    },
}

/// The request settings that change how it is sent.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplaySettings {
    pub timeout_ms: Option<u64>,
    pub connect_timeout_ms: Option<u64>,
    pub follow_redirects: Option<bool>,
    pub max_redirects: Option<u32>,
    pub verify_tls: Option<bool>,
    pub http2_prior_knowledge: Option<bool>,
    pub proxy: Option<String>,
    pub cookies: Option<bool>,
    pub decompress: Option<bool>,
    pub client_pkcs12_password: Option<String>,
}

/// TLS files, by their contained paths on this machine.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplayTls {
    pub ca_bundle: Option<ReplayFile>,
    pub client_cert: Option<ReplayFile>,
    pub client_key: Option<ReplayFile>,
    pub client_pkcs12: Option<ReplayFile>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplayFile {
    pub raw: String,
    pub path: String,
}

impl ReplayRequest {
    /// `request` with its secrets turned back into placeholders.
    pub fn new(request: &ResolvedRequest, auth_cache_key: Option<String>) -> Self {
        let mut builder = Builder {
            t: Templater::new(&request.secrets),
            withheld: Vec::new(),
        };
        let url = builder.t.resolved(&request.url);
        let query = request.query.iter().map(|p| builder.pair(p)).collect();
        let headers = request.headers.iter().map(|p| builder.header(p)).collect();
        let body = builder.body(&request.body);
        let auth = request.auth.as_ref().map(|a| builder.auth(a));
        let settings = builder.settings(&request.settings);
        let tls = builder.tls(&request.tls);
        Self {
            method: request.method.clone(),
            url,
            query,
            headers,
            body,
            auth,
            settings,
            tls,
            auth_cache_key,
            withheld: builder.withheld,
        }
    }

    /// Withhold what replay could not re-resolve or history must not keep:
    /// placeholders of the `derived` secrets (captured values that held a
    /// secret, which the secret store does not have), and, as a last net,
    /// every field when any of them still holds a value `known` redacts.
    pub fn withhold_derived(&mut self, known: &Redactor, derived: &[String]) {
        const LABEL: &str = "a value derived from a secret";
        let stored = serde_json::to_string(&*self).unwrap_or_default();
        if known.finds_secret(&stored) {
            *self = Self {
                method: self.method.clone(),
                url: String::new(),
                query: Vec::new(),
                headers: Vec::new(),
                body: ReplayBody::None,
                auth: None,
                settings: ReplaySettings::default(),
                tls: ReplayTls::default(),
                auth_cache_key: self.auth_cache_key.take(),
                withheld: vec![LABEL.to_owned()],
            };
            return;
        }
        let uses_derived = derived
            .iter()
            .any(|name| stored.contains(&format!("{{{{{name}}}}}")));
        if uses_derived && !self.withheld.iter().any(|w| w == LABEL) {
            self.withheld.push(LABEL.to_owned());
        }
    }

    /// The request again, with the secrets of `env` read from `secrets`
    /// now. Fails, before anything is sent, on a withheld part or a secret
    /// that is no longer set.
    pub fn resolve(
        &self,
        env: Option<&str>,
        secrets: &dyn SecretLookup,
    ) -> Result<ResolvedRequest, ResolveError> {
        if !self.withheld.is_empty() {
            return Err(ResolveError::Unsupported(format!(
                "this exchange cannot be replayed: {} {} not kept in history \
                 (send the request from its file instead)",
                self.withheld.join(", "),
                if self.withheld.len() == 1 {
                    "was"
                } else {
                    "were"
                }
            )));
        }
        let resolver = Resolver::new().with(StoredSecrets { env, secrets });
        let mut i = resolver.interpolator();
        let request = ResolvedRequest {
            method: self.method.clone(),
            url: i.text(&self.url),
            query: pairs(&mut i, &self.query),
            headers: pairs(&mut i, &self.headers),
            body: self.body.resolve(&mut i),
            auth: self.auth.as_ref().map(|a| a.resolve(&mut i)),
            settings: self.settings.resolve(&mut i),
            tls: self.tls.resolve(&mut i),
            secrets: Vec::new(),
        };
        let (mut request, secrets) = i.finish(request)?;
        request.secrets = secrets;
        Ok(request)
    }
}

/// Builds a [`ReplayRequest`], noting what it cannot keep.
struct Builder<'a> {
    t: Templater<'a>,
    withheld: Vec<String>,
}

impl Builder<'_> {
    fn pair(&self, pair: &ResolvedPair) -> ReplayPair {
        ReplayPair {
            name: self.t.resolved(&pair.name),
            value: self.t.resolved(&pair.value),
        }
    }

    fn header(&mut self, pair: &ResolvedPair) -> ReplayPair {
        let Some(canonical) = sensitive_header(&pair.name.text) else {
            return self.pair(pair);
        };
        let value = &pair.value;
        let value = self.credential(
            &format!("the {canonical} header"),
            &value.text,
            &value.secret_spans,
        );
        ReplayPair {
            name: self.t.resolved(&pair.name),
            value,
        }
    }

    /// A credential's template, or an empty value noted as withheld.
    fn credential(&mut self, label: &str, text: &str, spans: &[crate::vars::SecretSpan]) -> String {
        match self.t.credential(text, spans) {
            Ok(template) => template,
            Err(Withheld) => {
                self.withheld.push(label.to_owned());
                String::new()
            }
        }
    }

    fn body(&mut self, body: &ResolvedBody) -> ReplayBody {
        match body {
            ResolvedBody::None => ReplayBody::None,
            ResolvedBody::Json(value) => ReplayBody::Json {
                value: self.json(value),
            },
            ResolvedBody::JsonText(text) => ReplayBody::JsonText {
                text: self.t.resolved(text),
            },
            ResolvedBody::Text(text) => ReplayBody::Text {
                text: self.t.resolved(text),
            },
            ResolvedBody::Form(fields) => ReplayBody::Form {
                fields: fields.iter().map(|p| self.pair(p)).collect(),
            },
            ResolvedBody::Multipart(_) | ResolvedBody::File(_) => {
                self.withheld.push("the multipart or file body".to_owned());
                ReplayBody::None
            }
        }
    }

    fn json(&self, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.t.text(text)),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.json(v)).collect()),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (self.t.text(k), self.json(v)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    fn auth(&mut self, auth: &Auth) -> ReplayAuth {
        match auth {
            Auth::None => ReplayAuth::None,
            Auth::Basic { username, password } => ReplayAuth::Basic {
                username: self.t.text(username),
                password: self.credential("the basic auth password", password, &[]),
            },
            Auth::Bearer { token } => ReplayAuth::Bearer {
                token: self.credential("the bearer token", token, &[]),
            },
            Auth::ApiKey {
                name,
                value,
                location,
            } => ReplayAuth::ApiKey {
                name: self.t.text(name),
                value: self.credential("the API key", value, &[]),
                location: *location,
            },
            Auth::OAuth2(oauth2) => self.oauth2(oauth2),
        }
    }

    fn oauth2(&mut self, o: &OAuth2) -> ReplayAuth {
        let client_secret = o
            .client_secret
            .as_deref()
            .map(|s| self.credential("the OAuth2 client secret", s, &[]));
        ReplayAuth::OAuth2 {
            grant: o.grant,
            token_url: self.t.text(&o.token_url),
            auth_url: o.auth_url.as_deref().map(|u| self.t.text(u)),
            client_id: self.t.text(&o.client_id),
            client_secret,
            scopes: o.scopes.clone(),
        }
    }

    fn settings(&mut self, s: &RequestSettings) -> ReplaySettings {
        let password = s
            .client_pkcs12_password
            .as_deref()
            .map(|p| self.credential("the PKCS#12 password", p, &[]));
        ReplaySettings {
            timeout_ms: s.timeout_ms,
            connect_timeout_ms: s.connect_timeout_ms,
            follow_redirects: s.follow_redirects,
            max_redirects: s.max_redirects,
            verify_tls: s.verify_tls,
            http2_prior_knowledge: s.http2_prior_knowledge,
            proxy: s.proxy.as_deref().map(|p| self.t.text(p)),
            cookies: s.cookies,
            decompress: s.decompress,
            client_pkcs12_password: password,
        }
    }

    fn tls(&self, tls: &TlsFiles) -> ReplayTls {
        let file = |f: &Option<ResolvedFile>| {
            f.as_ref().map(|f| ReplayFile {
                raw: self.t.text(&f.raw),
                path: self.t.text(&f.path.to_string_lossy()),
            })
        };
        ReplayTls {
            ca_bundle: file(&tls.ca_bundle),
            client_cert: file(&tls.client_cert),
            client_key: file(&tls.client_key),
            client_pkcs12: file(&tls.client_pkcs12),
        }
    }
}

fn pairs(i: &mut Interpolator<'_, '_>, pairs: &[ReplayPair]) -> Vec<ResolvedPair> {
    pairs
        .iter()
        .map(|p| ResolvedPair {
            name: i.text(&p.name),
            value: i.text(&p.value),
        })
        .collect()
}

fn text(i: &mut Interpolator<'_, '_>, template: &str) -> String {
    i.text(template).text
}

fn json(i: &mut Interpolator<'_, '_>, value: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(text(i, s)),
        Value::Array(items) => Value::Array(items.iter().map(|v| json(i, v)).collect()),
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (text(i, k), json(i, v))).collect())
        }
        other => other.clone(),
    }
}

impl ReplayBody {
    fn resolve(&self, i: &mut Interpolator<'_, '_>) -> ResolvedBody {
        match self {
            Self::None => ResolvedBody::None,
            Self::Json { value } => ResolvedBody::Json(json(i, value)),
            Self::JsonText { text } => ResolvedBody::JsonText(i.text(text)),
            Self::Text { text } => ResolvedBody::Text(i.text(text)),
            Self::Form { fields } => ResolvedBody::Form(pairs(i, fields)),
        }
    }
}

impl ReplayAuth {
    fn resolve(&self, i: &mut Interpolator<'_, '_>) -> Auth {
        match self {
            Self::None => Auth::None,
            Self::Basic { username, password } => Auth::Basic {
                username: text(i, username),
                password: text(i, password),
            },
            Self::Bearer { token } => Auth::Bearer {
                token: text(i, token),
            },
            Self::ApiKey {
                name,
                value,
                location,
            } => Auth::ApiKey {
                name: text(i, name),
                value: text(i, value),
                location: *location,
            },
            Self::OAuth2 {
                grant,
                token_url,
                auth_url,
                client_id,
                client_secret,
                scopes,
            } => Auth::OAuth2(OAuth2 {
                grant: *grant,
                token_url: text(i, token_url),
                auth_url: auth_url.as_deref().map(|u| text(i, u)),
                client_id: text(i, client_id),
                client_secret: client_secret.as_deref().map(|s| text(i, s)),
                scopes: scopes.clone(),
            }),
        }
    }
}

impl ReplaySettings {
    fn resolve(&self, i: &mut Interpolator<'_, '_>) -> RequestSettings {
        RequestSettings {
            timeout_ms: self.timeout_ms,
            connect_timeout_ms: self.connect_timeout_ms,
            follow_redirects: self.follow_redirects,
            max_redirects: self.max_redirects,
            verify_tls: self.verify_tls,
            http2_prior_knowledge: self.http2_prior_knowledge,
            proxy: self.proxy.as_deref().map(|p| text(i, p)),
            cookies: self.cookies,
            decompress: self.decompress,
            client_pkcs12_password: self.client_pkcs12_password.as_deref().map(|p| text(i, p)),
            ..RequestSettings::default()
        }
    }
}

impl ReplayTls {
    fn resolve(&self, i: &mut Interpolator<'_, '_>) -> TlsFiles {
        let mut file = |f: &Option<ReplayFile>| {
            f.as_ref().map(|f| ResolvedFile {
                raw: text(i, &f.raw),
                path: PathBuf::from(text(i, &f.path)),
            })
        };
        TlsFiles {
            ca_bundle: file(&self.ca_bundle),
            client_cert: file(&self.client_cert),
            client_key: file(&self.client_key),
            client_pkcs12: file(&self.client_pkcs12),
        }
    }
}

/// Every placeholder of a stored request names a secret of its environment.
struct StoredSecrets<'a> {
    env: Option<&'a str>,
    secrets: &'a dyn SecretLookup,
}

impl VarSource for StoredSecrets<'_> {
    fn layer(&self) -> Layer {
        Layer::Environment
    }

    fn lookup(&self, name: &str) -> Option<Lookup> {
        let Some(env) = self.env else {
            return Some(Lookup::SecretNotSet {
                hint: "the exchange had no environment".to_owned(),
            });
        };
        Some(match self.secrets.secret(env, name) {
            Ok(Some(value)) => Lookup::Secret(Secret::new(name, value)),
            Ok(None) => Lookup::SecretNotSet {
                hint: format!("secret not set; run 'rustman secret set {name} --env {env}'"),
            },
            Err(err) => Lookup::SecretNotSet {
                hint: format!("could not read secret: {err}"),
            },
        })
    }
}

#[cfg(test)]
mod tests;
