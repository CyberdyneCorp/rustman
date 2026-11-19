//! Resolving a whole HTTP request before it is sent.
//!
//! `{{name}}` is interpolated in the URL, query, headers, body content, form
//! and multipart text fields, auth fields, settings strings and file paths.
//! Every missing name across all of them is reported at once, and nothing can
//! be sent unless resolution succeeds.

use std::fmt;
use std::path::PathBuf;

use serde_json::Value;

use super::resolver::{Interpolator, ResolveError, Resolved, Resolver, UsedSecret};
use crate::format::Located;
use crate::model::{
    Auth, AwsCredentials, AwsSigV4, Body, JsonContent, Jwt, NameValue, OAuth2, PartSource, Request,
    RequestSettings,
};
use crate::paths::PathEscape;
use crate::redact::Redactor;
use crate::secret::Hidden;
use crate::workspace::RequestRef;

/// A request with every placeholder replaced: the only input the transport takes.
///
/// Header, query, URL and text-body values record which byte ranges came from
/// secrets ([`Resolved::secret_spans`]), and `secrets` holds every secret value
/// used anywhere in the request. Output sinks go through
/// [`Redactor::for_request`]; the `Debug` output is already redacted.
#[derive(Clone, PartialEq)]
pub struct ResolvedRequest {
    pub method: String,
    pub url: Resolved,
    pub query: Vec<ResolvedPair>,
    /// Inherited headers merged with the request's own.
    pub headers: Vec<ResolvedPair>,
    pub body: ResolvedBody,
    /// The request's auth, else the nearest collection's.
    pub auth: Option<Auth>,
    pub settings: RequestSettings,
    /// The TLS files named in `settings`, contained in the workspace.
    pub tls: TlsFiles,
    /// Every secret value substituted anywhere in this request.
    pub secrets: Vec<UsedSecret>,
}

impl fmt::Debug for ResolvedRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let raw = if f.alternate() {
            format!("{:#?}", Fields(self))
        } else {
            format!("{:?}", Fields(self))
        };
        f.write_str(&Redactor::for_request(self).text(&raw))
    }
}

/// The derived `Debug` of a [`ResolvedRequest`], before redaction.
struct Fields<'a>(&'a ResolvedRequest);

impl fmt::Debug for Fields<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let r = self.0;
        f.debug_struct("ResolvedRequest")
            .field("method", &r.method)
            .field("url", &r.url)
            .field("query", &r.query)
            .field("headers", &Headers(&r.headers))
            .field("body", &FullBody(&r.body))
            .field("auth", &r.auth)
            .field("settings", &r.settings)
            .field("tls", &r.tls)
            .field("secrets", &r.secrets)
            .finish()
    }
}

/// Headers whose sensitive values (`Authorization`, `Cookie`, ...) are
/// replaced whole, even when written literally.
struct Headers<'a>(&'a [ResolvedPair]);

impl fmt::Debug for Headers<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redactor = Redactor::new();
        f.debug_list()
            .entries(self.0.iter().map(|p| {
                let value = redactor.resolved_header_value(&p.name.text, &p.value);
                (p.name.text.as_str(), value)
            }))
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPair {
    pub name: Resolved,
    pub value: Resolved,
}

/// `Debug` hides the content of a structured JSON body, which records no
/// secret spans; a whole [`ResolvedRequest`] prints it redacted instead.
#[derive(Clone, PartialEq)]
pub enum ResolvedBody {
    None,
    /// A YAML-structured JSON body; every string (keys included) was interpolated.
    Json(Value),
    /// JSON written as text; interpolated as text.
    JsonText(Resolved),
    Text(Resolved),
    Form(Vec<ResolvedPair>),
    Multipart(Vec<ResolvedPart>),
    File(ResolvedFile),
}

impl fmt::Debug for ResolvedBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("None"),
            Self::Json(_) => f.debug_tuple("Json").field(&Hidden("json")).finish(),
            Self::JsonText(text) => f.debug_tuple("JsonText").field(text).finish(),
            Self::Text(text) => f.debug_tuple("Text").field(text).finish(),
            Self::Form(fields) => f.debug_tuple("Form").field(fields).finish(),
            Self::Multipart(parts) => f.debug_tuple("Multipart").field(parts).finish(),
            Self::File(file) => f.debug_tuple("File").field(file).finish(),
        }
    }
}

/// A body with its JSON content shown, for a `Debug` that is redacted after.
struct FullBody<'a>(&'a ResolvedBody);

impl fmt::Debug for FullBody<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            ResolvedBody::Json(value) => f.debug_tuple("Json").field(value).finish(),
            other => other.fmt(f),
        }
    }
}

/// A file the request reads, named by a path relative to the request file's
/// folder and checked to stay inside the workspace after interpolation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedFile {
    /// The path as written, after interpolation; for messages.
    pub raw: String,
    /// The contained, normalized absolute path: the one to open.
    pub path: PathBuf,
}

impl ResolvedFile {
    /// A path not yet checked; [`resolve_request`] fills in `path`.
    fn unchecked(raw: String) -> Self {
        Self {
            raw,
            path: PathBuf::new(),
        }
    }
}

/// The TLS files of a request's settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TlsFiles {
    pub ca_bundle: Option<ResolvedFile>,
    pub client_cert: Option<ResolvedFile>,
    pub client_key: Option<ResolvedFile>,
    pub client_pkcs12: Option<ResolvedFile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPart {
    pub name: String,
    pub source: ResolvedPartSource,
    pub content_type: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedPartSource {
    Text(Resolved),
    File(ResolvedFile),
}

/// Resolve `request` through `resolver`.
pub fn resolve_request(
    request: &RequestRef<'_>,
    resolver: &Resolver<'_>,
) -> Result<ResolvedRequest, ResolveError> {
    let Request::Http(http) = request.request() else {
        let reason = request.request().unsupported_reason().unwrap_or_default();
        return Err(ResolveError::Unsupported(reason));
    };
    let mut i = resolver.interpolator();
    let mut settings = settings(&mut i, http.settings.as_ref());
    let mut tls = tls_files(&settings);
    inherit_client_identity(&mut i, request, &mut settings, &mut tls)?;
    let resolved = ResolvedRequest {
        method: http.effective_method().to_owned(),
        url: i.text(&http.url),
        query: pairs(&mut i, http.query.as_deref().unwrap_or_default()),
        headers: pairs(&mut i, &request.effective_headers()),
        body: body(&mut i, http.body.as_ref()),
        auth: request.effective_auth().map(|a| auth(&mut i, a)),
        tls,
        settings,
        secrets: Vec::new(),
    };
    let (mut resolved, secrets) = i.finish(resolved)?;
    resolved.secrets = secrets;
    contain_files(request, &mut resolved)?;
    contain_jwt_key(request, &mut resolved)?;
    Ok(resolved)
}

/// The workspace client certificate (`rustman.toml [http]`), for a request
/// that sets neither `client_cert` nor `client_pkcs12`. Its paths are
/// relative to the workspace root and contained here.
pub(super) fn inherit_client_identity(
    i: &mut Interpolator<'_, '_>,
    request: &RequestRef<'_>,
    settings: &mut RequestSettings,
    tls: &mut TlsFiles,
) -> Result<(), ResolveError> {
    if settings.client_cert.is_some()
        || settings.client_key.is_some()
        || settings.client_pkcs12.is_some()
    {
        return Ok(());
    }
    let http = request.http;
    let file = |raw: &Option<String>| -> Result<Option<ResolvedFile>, PathEscape> {
        let Some(raw) = raw else {
            return Ok(None);
        };
        let path = request.contain_workspace_file(raw)?;
        Ok(Some(ResolvedFile {
            raw: raw.clone(),
            path,
        }))
    };
    tls.client_cert = file(&http.client_cert)?;
    tls.client_key = file(&http.client_key)?;
    tls.client_pkcs12 = file(&http.client_pkcs12)?;
    if tls.client_pkcs12.is_some() {
        settings.client_pkcs12_password = optional(i, http.client_pkcs12_password.as_deref());
    }
    Ok(())
}

/// Check every interpolated file path stays inside the workspace and keep the
/// contained path. Files already contained (from `rustman.toml`) are kept.
/// The error shows the resolved path with any secret value redacted.
pub(super) fn contain_files(
    request: &RequestRef<'_>,
    resolved: &mut ResolvedRequest,
) -> Result<(), ResolveError> {
    let mut escaped = None;
    let unchecked = |file: &&mut ResolvedFile| file.path.as_os_str().is_empty();
    for file in resolved.files_mut().into_iter().filter(unchecked) {
        match request.contain_file(&file.raw) {
            Ok(path) => file.path = path,
            Err(PathEscape(path)) => {
                escaped = Some(path);
                break;
            }
        }
    }
    match escaped {
        Some(path) => {
            let shown = Redactor::for_request(resolved).text(&path).into_owned();
            Err(PathEscape(shown).into())
        }
        None => Ok(()),
    }
}

impl ResolvedRequest {
    /// Every file the request reads: body, multipart files, TLS files.
    pub fn files(&self) -> Vec<&ResolvedFile> {
        let mut files = Vec::new();
        match &self.body {
            ResolvedBody::File(file) => files.push(file),
            ResolvedBody::Multipart(parts) => {
                files.extend(parts.iter().filter_map(|p| match &p.source {
                    ResolvedPartSource::File(file) => Some(file),
                    ResolvedPartSource::Text(_) => None,
                }));
            }
            _ => {}
        }
        let t = &self.tls;
        let tls = [
            &t.ca_bundle,
            &t.client_cert,
            &t.client_key,
            &t.client_pkcs12,
        ];
        files.extend(tls.into_iter().flatten());
        files
    }

    fn files_mut(&mut self) -> Vec<&mut ResolvedFile> {
        let mut files = Vec::new();
        match &mut self.body {
            ResolvedBody::File(file) => files.push(file),
            ResolvedBody::Multipart(parts) => {
                files.extend(parts.iter_mut().filter_map(|p| match &mut p.source {
                    ResolvedPartSource::File(file) => Some(file),
                    ResolvedPartSource::Text(_) => None,
                }));
            }
            _ => {}
        }
        let t = &mut self.tls;
        let tls = [
            &mut t.ca_bundle,
            &mut t.client_cert,
            &mut t.client_key,
            &mut t.client_pkcs12,
        ];
        files.extend(tls.into_iter().flatten());
        files
    }
}

pub(super) fn tls_files(settings: &RequestSettings) -> TlsFiles {
    let file =
        |p: &Option<Located<String>>| p.as_ref().map(|p| ResolvedFile::unchecked(p.get().clone()));
    TlsFiles {
        ca_bundle: file(&settings.ca_bundle),
        client_cert: file(&settings.client_cert),
        client_key: file(&settings.client_key),
        client_pkcs12: file(&settings.client_pkcs12),
    }
}

pub(super) fn pairs(i: &mut Interpolator<'_, '_>, items: &[NameValue]) -> Vec<ResolvedPair> {
    items
        .iter()
        .filter(|p| p.enabled)
        .map(|p| ResolvedPair {
            name: i.text(&p.name),
            value: i.text(&p.value),
        })
        .collect()
}

fn body(i: &mut Interpolator<'_, '_>, body: Option<&Body>) -> ResolvedBody {
    match body {
        None | Some(Body::None) => ResolvedBody::None,
        Some(Body::Json(JsonContent::Value(value))) => ResolvedBody::Json(json(i, value)),
        Some(Body::Json(JsonContent::Text(text))) => ResolvedBody::JsonText(i.text(text)),
        Some(Body::Text(text)) => ResolvedBody::Text(i.text(text)),
        Some(Body::Form(fields)) => ResolvedBody::Form(pairs(i, fields)),
        Some(Body::Multipart(parts)) => ResolvedBody::Multipart(
            parts
                .iter()
                .map(|p| ResolvedPart {
                    name: p.name.clone(),
                    source: match &p.source {
                        PartSource::Text(text) => ResolvedPartSource::Text(i.text(text)),
                        PartSource::File(path) => {
                            ResolvedPartSource::File(ResolvedFile::unchecked(i.text(path).text))
                        }
                    },
                    content_type: p.content_type.clone(),
                })
                .collect(),
        ),
        Some(Body::File(path)) => ResolvedBody::File(ResolvedFile::unchecked(i.text(path).text)),
    }
}

fn json(i: &mut Interpolator<'_, '_>, value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(i.text(text).text),
        Value::Array(items) => Value::Array(items.iter().map(|v| json(i, v)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (i.text(k).text, json(i, v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub(super) fn auth(i: &mut Interpolator<'_, '_>, auth: &Auth) -> Auth {
    let mut t = |s: &str| i.text(s).text;
    match auth {
        Auth::None => Auth::None,
        Auth::Basic { username, password } => Auth::Basic {
            username: t(username),
            password: t(password),
        },
        Auth::Bearer { token } => Auth::Bearer { token: t(token) },
        Auth::ApiKey {
            name,
            value,
            location,
        } => Auth::ApiKey {
            name: t(name),
            value: t(value),
            location: *location,
        },
        Auth::OAuth2(oauth) => Auth::OAuth2(oauth2(i, oauth)),
        Auth::Digest { username, password } => Auth::Digest {
            username: t(username),
            password: t(password),
        },
        Auth::AwsSigV4(sigv4) => Auth::AwsSigV4(Box::new(aws_sigv4(i, sigv4))),
        Auth::Jwt(jwt) => Auth::Jwt(Box::new(jwt_auth(i, jwt))),
    }
}

fn aws_sigv4(i: &mut Interpolator<'_, '_>, s: &AwsSigV4) -> AwsSigV4 {
    let mut t = |s: &str| i.text(s).text;
    let credentials = match &s.credentials {
        AwsCredentials::Fields {
            access_key,
            secret_key,
            session_token,
        } => AwsCredentials::Fields {
            access_key: t(access_key),
            secret_key: t(secret_key),
            session_token: session_token.as_deref().map(&mut t),
        },
        AwsCredentials::Env => AwsCredentials::Env,
        AwsCredentials::Profile(name) => AwsCredentials::Profile(t(name)),
    };
    AwsSigV4 {
        region: t(&s.region),
        service: t(&s.service),
        credentials,
        unsigned_payload: s.unsigned_payload,
    }
}

/// A JWT config with its strings interpolated: claims and header values,
/// the key material and the key file path (contained later, see
/// [`contain_jwt_key`]).
fn jwt_auth(i: &mut Interpolator<'_, '_>, j: &Jwt) -> Jwt {
    let object = |i: &mut Interpolator<'_, '_>, map: &serde_json::Map<String, Value>| {
        map.iter()
            .map(|(k, v)| (i.text(k).text, json(i, v)))
            .collect()
    };
    Jwt {
        secret: optional(i, j.secret.as_deref()),
        private_key: optional(i, j.private_key.as_deref()),
        private_key_file: path(i, j.private_key_file.as_ref()),
        claims: object(i, &j.claims),
        header: object(i, &j.header),
        name: optional(i, j.name.as_deref()),
        ..j.clone()
    }
}

/// Resolve a JWT `private_key_file` against the workspace root, keeping it
/// inside the workspace; the resolved auth then holds the full path.
pub(super) fn contain_jwt_key(
    request: &RequestRef<'_>,
    resolved: &mut ResolvedRequest,
) -> Result<(), ResolveError> {
    let Some(Auth::Jwt(jwt)) = &mut resolved.auth else {
        return Ok(());
    };
    let Some(file) = &jwt.private_key_file else {
        return Ok(());
    };
    let path = request.contain_workspace_file(file)?;
    jwt.private_key_file = Some(Located::new(path.to_string_lossy().into_owned()));
    Ok(())
}

fn oauth2(i: &mut Interpolator<'_, '_>, oauth: &OAuth2) -> OAuth2 {
    let mut t = |s: &str| i.text(s).text;
    OAuth2 {
        grant: oauth.grant,
        token_url: t(&oauth.token_url),
        auth_url: oauth.auth_url.as_deref().map(&mut t),
        client_id: t(&oauth.client_id),
        client_secret: oauth.client_secret.as_deref().map(&mut t),
        scopes: oauth
            .scopes
            .as_ref()
            .map(|scopes| scopes.iter().map(|s| t(s)).collect()),
    }
}

pub(super) fn settings(
    i: &mut Interpolator<'_, '_>,
    settings: Option<&RequestSettings>,
) -> RequestSettings {
    let Some(settings) = settings else {
        return RequestSettings::default();
    };
    RequestSettings {
        proxy: optional(i, settings.proxy.as_deref()),
        client_pkcs12_password: optional(i, settings.client_pkcs12_password.as_deref()),
        ca_bundle: path(i, settings.ca_bundle.as_ref()),
        client_cert: path(i, settings.client_cert.as_ref()),
        client_key: path(i, settings.client_key.as_ref()),
        client_pkcs12: path(i, settings.client_pkcs12.as_ref()),
        ..settings.clone()
    }
}

fn optional(i: &mut Interpolator<'_, '_>, text: Option<&str>) -> Option<String> {
    text.map(|t| i.text(t).text)
}

fn path(i: &mut Interpolator<'_, '_>, path: Option<&Located<String>>) -> Option<Located<String>> {
    path.map(|p| Located::new(i.text(p).text))
}
