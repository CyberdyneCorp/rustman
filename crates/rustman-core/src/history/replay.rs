//! What `rustman history replay` re-sends: the resolved request with every
//! secret turned back into its `{{name}}` placeholder (request-history spec
//! "Inspect and replay"). Replaying resolves those placeholders from the
//! current secret store, so the stored entry never holds a secret value and
//! a deleted secret fails with `unresolved variable(s): <name>`. A secret
//! from a provider is fetched again when the replay is sent.
//!
//! A credential written literally (an `Authorization` header or auth token
//! that uses no secret variable) is not kept, as the redaction invariant
//! requires; such an entry cannot be replayed. Multipart and file bodies are
//! not kept either. Neither is a value derived from a secret that the
//! secret store cannot give back (a captured value that held a secret).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::template::{Templater, Withheld};
use crate::environment::Environment;
use crate::model::{
    ApiKeyLocation, Auth, AwsCredentials, AwsSigV4, Jwt, JwtAlgorithm, OAuth2, OAuth2Grant,
    RequestSettings, VarValue,
};
use crate::redact::{Redactor, sensitive_header};
use crate::secret::Secret;
use crate::secrets::provider::{SecretProviders, SecretRef, resolve_with_secrets};
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
    Digest {
        username: String,
        password: String,
    },
    #[serde(rename = "aws_sigv4")]
    AwsSigV4 {
        region: String,
        service: String,
        /// `None` for credentials from the environment or a profile.
        access_key: Option<String>,
        secret_key: Option<String>,
        session_token: Option<String>,
        /// The profile of `from: profile`; `None` with `from: env`.
        profile: Option<String>,
        #[serde(default)]
        from_env: bool,
        #[serde(default)]
        unsigned_payload: bool,
    },
    Jwt {
        algorithm: JwtAlgorithm,
        secret: Option<String>,
        private_key: Option<String>,
        private_key_file: Option<String>,
        #[serde(default)]
        claims: Map<String, Value>,
        #[serde(default)]
        header: Map<String, Value>,
        ttl_secs: u64,
        location: ApiKeyLocation,
        name: Option<String>,
    },
    /// Signed by a plugin when replayed; the params as templates.
    Plugin {
        scheme: String,
        params: Vec<(String, String)>,
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
        self.check_kept()?;
        let resolver = Resolver::new().with(StoredSecrets {
            env,
            providers: None,
            secrets,
        });
        self.resolve_with(&resolver)
    }

    /// [`Self::resolve`] for a send: a placeholder naming a provider
    /// reference (`ci/KEY`), or a variable `environment` backs with one,
    /// is fetched again through `providers`.
    pub async fn resolve_for_send(
        &self,
        environment: Option<&Environment>,
        env: Option<&str>,
        secrets: &dyn SecretLookup,
        providers: Option<&SecretProviders>,
    ) -> Result<ResolvedRequest, ResolveError> {
        self.check_kept()?;
        let mut resolver = Resolver::new().with(StoredSecrets {
            env,
            providers: Some(ProviderNames { environment }),
            secrets,
        });
        resolve_with_secrets(&mut resolver, providers, |r| self.resolve_with(r)).await
    }

    fn check_kept(&self) -> Result<(), ResolveError> {
        if self.withheld.is_empty() {
            return Ok(());
        }
        Err(ResolveError::Unsupported(format!(
            "this exchange cannot be replayed: {} {} not kept in history \
             (send the request from its file instead)",
            self.withheld.join(", "),
            if self.withheld.len() == 1 {
                "was"
            } else {
                "were"
            }
        )))
    }

    fn resolve_with(&self, resolver: &Resolver<'_>) -> Result<ResolvedRequest, ResolveError> {
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
            Auth::Digest { username, password } => ReplayAuth::Digest {
                username: self.t.text(username),
                password: self.credential("the digest password", password, &[]),
            },
            Auth::AwsSigV4(sigv4) => self.aws_sigv4(sigv4),
            Auth::Jwt(jwt) => self.jwt(jwt),
            Auth::Plugin(plugin) => ReplayAuth::Plugin {
                scheme: plugin.scheme.clone(),
                params: plugin
                    .params
                    .iter()
                    .map(|(name, value)| (name.clone(), self.t.text(value)))
                    .collect(),
            },
        }
    }

    fn aws_sigv4(&mut self, s: &AwsSigV4) -> ReplayAuth {
        let (mut access_key, mut secret_key, mut session_token) = (None, None, None);
        let (mut profile, mut from_env) = (None, false);
        match &s.credentials {
            AwsCredentials::Fields {
                access_key: key,
                secret_key: secret,
                session_token: token,
            } => {
                access_key = Some(self.t.text(key));
                secret_key = Some(self.credential("the AWS secret key", secret, &[]));
                session_token = token
                    .as_deref()
                    .map(|t| self.credential("the AWS session token", t, &[]));
            }
            AwsCredentials::Env => from_env = true,
            AwsCredentials::Profile(name) => profile = Some(self.t.text(name)),
        }
        ReplayAuth::AwsSigV4 {
            region: self.t.text(&s.region),
            service: self.t.text(&s.service),
            access_key,
            secret_key,
            session_token,
            profile,
            from_env,
            unsigned_payload: s.unsigned_payload,
        }
    }

    fn jwt(&mut self, j: &Jwt) -> ReplayAuth {
        let object = |b: &Self, map: &Map<String, Value>| match b.json(&Value::Object(map.clone()))
        {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        ReplayAuth::Jwt {
            algorithm: j.algorithm,
            secret: j
                .secret
                .as_deref()
                .map(|s| self.credential("the JWT secret", s, &[])),
            private_key: j
                .private_key
                .as_deref()
                .map(|k| self.credential("the JWT private key", k, &[])),
            private_key_file: j.private_key_file.as_ref().map(|f| self.t.text(f)),
            claims: object(self, &j.claims),
            header: object(self, &j.header),
            ttl_secs: j.ttl_secs,
            location: j.location,
            name: j.name.as_deref().map(|n| self.t.text(n)),
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
            Self::Digest { username, password } => Auth::Digest {
                username: text(i, username),
                password: text(i, password),
            },
            Self::AwsSigV4 { .. } => Auth::AwsSigV4(Box::new(self.aws_sigv4(i))),
            Self::Jwt { .. } => Auth::Jwt(Box::new(self.jwt(i))),
            Self::Plugin { scheme, params } => Auth::Plugin(crate::model::PluginAuth {
                scheme: scheme.clone(),
                params: params
                    .iter()
                    .map(|(name, value)| (name.clone(), text(i, value)))
                    .collect(),
            }),
        }
    }

    fn aws_sigv4(&self, i: &mut Interpolator<'_, '_>) -> AwsSigV4 {
        let Self::AwsSigV4 {
            region,
            service,
            access_key,
            secret_key,
            session_token,
            profile,
            from_env,
            unsigned_payload,
        } = self
        else {
            unreachable!("called on aws_sigv4 auth only");
        };
        let credentials = match (profile, from_env) {
            (Some(name), _) => AwsCredentials::Profile(text(i, name)),
            (None, true) => AwsCredentials::Env,
            (None, false) => AwsCredentials::Fields {
                access_key: text(i, access_key.as_deref().unwrap_or_default()),
                secret_key: text(i, secret_key.as_deref().unwrap_or_default()),
                session_token: session_token.as_deref().map(|t| text(i, t)),
            },
        };
        AwsSigV4 {
            region: text(i, region),
            service: text(i, service),
            credentials,
            unsigned_payload: *unsigned_payload,
        }
    }

    fn jwt(&self, i: &mut Interpolator<'_, '_>) -> Jwt {
        let Self::Jwt {
            algorithm,
            secret,
            private_key,
            private_key_file,
            claims,
            header,
            ttl_secs,
            location,
            name,
        } = self
        else {
            unreachable!("called on jwt auth only");
        };
        let object = |i: &mut Interpolator<'_, '_>, map: &Map<String, Value>| match json(
            i,
            &Value::Object(map.clone()),
        ) {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        Jwt {
            algorithm: *algorithm,
            secret: secret.as_deref().map(|s| text(i, s)),
            private_key: private_key.as_deref().map(|k| text(i, k)),
            private_key_file: private_key_file.as_deref().map(|f| text(i, f).into()),
            claims: object(i, claims),
            header: object(i, header),
            ttl_secs: *ttl_secs,
            location: *location,
            name: name.as_deref().map(|n| text(i, n)),
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
    /// Set when replay may fetch provider secrets again.
    providers: Option<ProviderNames<'a>>,
    secrets: &'a dyn SecretLookup,
}

/// Tells which placeholders of a stored request name provider secrets.
struct ProviderNames<'a> {
    environment: Option<&'a Environment>,
}

impl ProviderNames<'_> {
    /// The reference behind placeholder `name`: a variable of the
    /// environment backed by a provider, or the reference text itself
    /// unless the environment keeps a store secret of that name.
    fn reference(&self, name: &str) -> Option<SecretRef> {
        if let Some(env) = self.environment {
            if let Some(VarValue::Secret(reference)) = env.variables.get(name) {
                return Some(reference.clone());
            }
            if env.secrets.iter().any(|s| s == name) {
                return None;
            }
        }
        SecretRef::parse(name).ok()
    }
}

impl VarSource for StoredSecrets<'_> {
    fn layer(&self) -> Layer {
        Layer::Environment
    }

    fn lookup(&self, name: &str) -> Option<Lookup> {
        let provided = self.providers.as_ref().and_then(|p| p.reference(name));
        if let Some(reference) = provided {
            return Some(Lookup::Provider(reference));
        }
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
