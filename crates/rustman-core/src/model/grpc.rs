//! The `kind: grpc` request file: one gRPC call (unary or streaming) with
//! its target, metadata, deadline, TLS, schema source and stream bounds.
//!
//! Sending belongs to the `rustman-grpc` crate; this module only holds the
//! file model and its load-time checks.

use std::fmt;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use super::assertion::Assertion;
use super::capture::Capture;
use super::common::{Checked, NameValue, Variables};
use super::http::RequestSettings;
use super::jsonrpc::unlocated;
use crate::error::{LoadError, Position};
use crate::format::{Located, from_yaml};
use crate::secret::Hidden;

/// Messages a headless stream collects when `stream.max_messages` is not set.
pub const DEFAULT_STREAM_MESSAGES: u32 = 100;

/// How long a headless stream runs when `stream.duration_ms` is not set.
pub const DEFAULT_STREAM_DURATION_MS: u64 = 30_000;

/// The load error for a file with both `message` and `messages`.
pub const BOTH_MESSAGES_ERROR: &str = "grpc: exactly one of message or messages is allowed";

/// The load error for a file with neither `message` nor `messages`.
pub const NO_MESSAGE_ERROR: &str = "grpc: one of message or messages is required";

/// The canonical gRPC status code names, indexed by code.
pub const GRPC_CODE_NAMES: [&str; 17] = [
    "OK",
    "CANCELLED",
    "UNKNOWN",
    "INVALID_ARGUMENT",
    "DEADLINE_EXCEEDED",
    "NOT_FOUND",
    "ALREADY_EXISTS",
    "PERMISSION_DENIED",
    "RESOURCE_EXHAUSTED",
    "FAILED_PRECONDITION",
    "ABORTED",
    "OUT_OF_RANGE",
    "UNIMPLEMENTED",
    "INTERNAL",
    "UNAVAILABLE",
    "DATA_LOSS",
    "UNAUTHENTICATED",
];

/// The name of gRPC status code `code`, such as `NOT_FOUND`; `UNKNOWN` for
/// codes outside the standard range.
pub fn grpc_code_name(code: i32) -> &'static str {
    usize::try_from(code)
        .ok()
        .and_then(|i| GRPC_CODE_NAMES.get(i))
        .copied()
        .unwrap_or("UNKNOWN")
}

/// The gRPC status code named `name` (case-insensitive), or written as a
/// number.
pub fn parse_grpc_code(name: &str) -> Option<i32> {
    let name = name.trim();
    if let Ok(code) = name.parse::<i32>() {
        return Some(code);
    }
    GRPC_CODE_NAMES
        .iter()
        .position(|n| n.eq_ignore_ascii_case(name))
        .and_then(|i| i32::try_from(i).ok())
}

/// A `kind: grpc` request file. Optional fields stay `None` when absent;
/// the `effective_*` methods apply the defaults. Saving writes `source`
/// back unchanged.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrpcRequest {
    pub version: u64,
    pub kind: String,
    pub name: String,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    /// `host:port`, `grpc://host:port` or `grpcs://host:port`;
    /// `{{variables}}` are interpolated.
    pub target: String,
    /// Fully qualified: `package.Service/Method`.
    pub method: String,
    /// The request message (unary and server streaming), in the proto3
    /// JSON mapping; string values are interpolated.
    pub message: Option<Located<Value>>,
    /// The request messages, in order (client and bidi streaming).
    pub messages: Option<Located<Vec<Value>>>,
    /// Request metadata, in order; names ending in `-bin` take base64.
    pub metadata: Option<Vec<NameValue>>,
    pub deadline_ms: Option<u64>,
    pub tls: Option<GrpcTls>,
    pub transport: Option<GrpcTransport>,
    pub schema: Option<GrpcSchema>,
    pub stream: Option<StreamLimits>,
    pub variables: Option<Variables>,
    pub assertions: Option<Vec<Located<Assertion>>>,
    pub captures: Option<Vec<Checked<Capture>>>,
    /// The file text, written back unchanged on save.
    #[serde(skip)]
    pub source: String,
}

/// `transport`: native gRPC over HTTP/2, or gRPC-Web.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GrpcTransport {
    #[default]
    Grpc,
    GrpcWeb,
}

/// `tls`: the same CA, client certificate and `insecure` options as
/// `http-requests`. `Debug` hides the PKCS#12 password.
#[derive(Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrpcTls {
    /// TLS on or off; by default on for `grpcs://` and bare targets on
    /// port 443, off for `grpc://` and other bare targets.
    pub enabled: Option<bool>,
    /// Skip certificate verification.
    pub insecure: Option<bool>,
    pub ca_bundle: Option<Located<String>>,
    pub client_cert: Option<Located<String>>,
    pub client_key: Option<Located<String>>,
    pub client_pkcs12: Option<Located<String>>,
    pub client_pkcs12_password: Option<String>,
}

impl fmt::Debug for GrpcTls {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let password = self
            .client_pkcs12_password
            .as_ref()
            .map(|_| Hidden("client_pkcs12_password"));
        f.debug_struct("GrpcTls")
            .field("enabled", &self.enabled)
            .field("insecure", &self.insecure)
            .field("ca_bundle", &self.ca_bundle)
            .field("client_cert", &self.client_cert)
            .field("client_key", &self.client_key)
            .field("client_pkcs12", &self.client_pkcs12)
            .field("client_pkcs12_password", &password)
            .finish()
    }
}

impl GrpcTls {
    /// The TLS options as `http-requests` settings, so the shared TLS
    /// builder and file containment apply unchanged.
    pub fn http(&self) -> RequestSettings {
        RequestSettings {
            verify_tls: self.insecure.map(|insecure| !insecure),
            ca_bundle: self.ca_bundle.clone(),
            client_cert: self.client_cert.clone(),
            client_key: self.client_key.clone(),
            client_pkcs12: self.client_pkcs12.clone(),
            client_pkcs12_password: self.client_pkcs12_password.clone(),
            ..RequestSettings::default()
        }
    }
}

/// `schema`: where message types come from when not from reflection.
/// Paths are relative to the request file and stay inside the workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrpcSchema {
    /// `.proto` files compiled in-process.
    pub protos: Option<Vec<Located<String>>>,
    /// Folders imports are looked up in.
    pub import_paths: Option<Vec<Located<String>>>,
    /// A binary `FileDescriptorSet`.
    pub descriptor_set: Option<Located<String>>,
}

/// `stream`: the bounds of a headless stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamLimits {
    pub max_messages: Option<u32>,
    pub duration_ms: Option<u64>,
}

impl StreamLimits {
    pub fn effective_max_messages(&self) -> u32 {
        self.max_messages.unwrap_or(DEFAULT_STREAM_MESSAGES)
    }

    pub fn effective_duration_ms(&self) -> u64 {
        self.duration_ms.unwrap_or(DEFAULT_STREAM_DURATION_MS)
    }
}

impl GrpcRequest {
    /// Parse and check the `kind: grpc` file `file` whose text is `text`.
    pub fn parse(file: &Path, text: &str) -> Result<Self, LoadError> {
        let mut request: Self = from_yaml(file, text)?;
        Checker { file, text }.request(&request)?;
        request.source = text.to_owned();
        Ok(request)
    }

    /// The request messages in order: the single `message`, or `messages`.
    pub fn message_list(&self) -> Vec<&Value> {
        match (&self.message, &self.messages) {
            (Some(message), _) => vec![message.get()],
            (None, Some(messages)) => messages.get().iter().collect(),
            (None, None) => Vec::new(),
        }
    }

    /// Whether the file lists `messages` (client or bidi streaming).
    pub fn has_message_list(&self) -> bool {
        self.messages.is_some()
    }

    pub fn effective_transport(&self) -> GrpcTransport {
        self.transport.unwrap_or_default()
    }

    pub fn effective_stream(&self) -> StreamLimits {
        self.stream.unwrap_or_default()
    }

    /// The TLS options as `http-requests` settings.
    pub fn http_settings(&self) -> Option<RequestSettings> {
        self.tls.as_ref().map(GrpcTls::http)
    }

    /// The assertions of the request, in the shape the assertion engine
    /// takes.
    pub fn assertion_list(&self) -> Vec<Assertion> {
        unlocated(self.assertions.as_deref())
    }

    /// Every workspace file and folder the request refers to, with its
    /// position.
    pub fn file_paths(&self) -> Vec<&Located<String>> {
        let mut paths = Vec::new();
        if let Some(tls) = &self.tls {
            let files = [
                &tls.ca_bundle,
                &tls.client_cert,
                &tls.client_key,
                &tls.client_pkcs12,
            ];
            paths.extend(files.into_iter().flatten());
        }
        if let Some(schema) = &self.schema {
            paths.extend(schema.protos.iter().flatten());
            paths.extend(schema.import_paths.iter().flatten());
            paths.extend(&schema.descriptor_set);
        }
        paths.extend(self.assertion_list_files());
        paths
    }

    fn assertion_list_files(&self) -> impl Iterator<Item = &Located<String>> {
        self.assertions
            .iter()
            .flatten()
            .filter_map(|a| match a.get() {
                Assertion::Schema(schema) => schema.file.as_ref(),
                _ => None,
            })
    }
}

/// The load-time checks of a `grpc` file; every error names its line.
struct Checker<'a> {
    file: &'a Path,
    text: &'a str,
}

/// Line 1: the position of errors about the file as a whole.
const FIRST_LINE: Position = Position { line: 1, column: 1 };

impl Checker<'_> {
    fn fail<T>(&self, at: Option<Position>, message: &str) -> Result<T, LoadError> {
        Err(LoadError::new(self.file, at, message))
    }

    /// The position of the top-level `key:` of the file, when the file is
    /// YAML written one key per line.
    fn key_line(&self, key: &str) -> Option<Position> {
        let prefix = format!("{key}:");
        let index = self.text.lines().position(|l| l.starts_with(&prefix))?;
        Some(Position {
            line: index + 1,
            column: 1,
        })
    }

    fn request(&self, r: &GrpcRequest) -> Result<(), LoadError> {
        self.target(&r.target)?;
        self.method(&r.method)?;
        self.messages(r)?;
        if let Some(schema) = &r.schema {
            self.schema(schema)?;
        }
        if r.effective_stream().max_messages == Some(0) {
            let at = self.key_line("stream");
            return self.fail(at, "grpc: stream.max_messages must be at least 1");
        }
        Ok(())
    }

    fn target(&self, target: &str) -> Result<(), LoadError> {
        let at = self.key_line("target").or(Some(FIRST_LINE));
        let trimmed = target.trim();
        if trimmed.is_empty() {
            return self.fail(at, "grpc: target must not be empty");
        }
        if trimmed.contains("{{") || valid_target(trimmed) {
            return Ok(());
        }
        self.fail(
            at,
            "grpc: target must be host:port or a grpc:// or grpcs:// URL",
        )
    }

    fn method(&self, method: &str) -> Result<(), LoadError> {
        if method.contains("{{") || valid_method(method) {
            return Ok(());
        }
        let at = self.key_line("method").or(Some(FIRST_LINE));
        self.fail(
            at,
            "grpc: method must be fully qualified as package.Service/Method",
        )
    }

    fn messages(&self, r: &GrpcRequest) -> Result<(), LoadError> {
        match (&r.message, &r.messages) {
            (Some(_), Some(both)) => self.fail(both.position(), BOTH_MESSAGES_ERROR),
            (None, None) => self.fail(Some(FIRST_LINE), NO_MESSAGE_ERROR),
            (Some(message), None) if !message.get().is_object() => {
                self.fail(message.position(), "grpc: message must be a map")
            }
            (None, Some(messages)) if !messages.get().iter().all(Value::is_object) => self.fail(
                messages.position(),
                "grpc: every entry of messages must be a map",
            ),
            _ => Ok(()),
        }
    }

    fn schema(&self, schema: &GrpcSchema) -> Result<(), LoadError> {
        let at = self.key_line("schema");
        let protos = schema.protos.as_ref().is_some_and(|p| !p.is_empty());
        if protos && schema.descriptor_set.is_some() {
            return self.fail(
                at,
                "grpc: schema sets either protos or descriptor_set, not both",
            );
        }
        if !protos && schema.import_paths.is_some() {
            return self.fail(at, "grpc: schema.import_paths requires schema.protos");
        }
        Ok(())
    }
}

/// `grpc://` or `grpcs://` followed by `host:port`, or a bare `host:port`.
fn valid_target(target: &str) -> bool {
    let authority = match target.split_once("://") {
        Some(("grpc" | "grpcs", rest)) => rest.trim_end_matches('/'),
        Some(_) => return false,
        None => target,
    };
    let Some((host, port)) = authority.rsplit_once(':') else {
        return false;
    };
    !host.is_empty() && !host.contains('/') && port.parse::<u16>().is_ok()
}

/// `package.Service/Method`: a dotted service name and a method name.
fn valid_method(method: &str) -> bool {
    let Some((service, name)) = method.split_once('/') else {
        return false;
    };
    let ident = |s: &str| {
        !s.is_empty()
            && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !s.starts_with(|c: char| c.is_ascii_digit())
    };
    ident(name) && service.split('.').all(ident)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<GrpcRequest, LoadError> {
        GrpcRequest::parse(Path::new("r.rman.yaml"), text)
    }

    const HEAD: &str = "version: 1\nkind: grpc\nname: r\n";

    #[test]
    fn a_unary_request_loads_with_defaults() {
        let r = parse(&format!(
            "{HEAD}target: '{{{{grpc_host}}}}'\nmethod: shop.v1.Catalog/GetProduct\nmessage: {{id: p-1}}\n"
        ))
        .unwrap();
        assert_eq!(r.message_list(), [&serde_json::json!({"id": "p-1"})]);
        assert!(!r.has_message_list());
        assert_eq!(r.effective_transport(), GrpcTransport::Grpc);
        assert_eq!(r.effective_stream().effective_max_messages(), 100);
        assert_eq!(r.effective_stream().effective_duration_ms(), 30_000);
        assert!(r.http_settings().is_none());
    }

    #[test]
    fn both_message_fields_are_refused() {
        let err = parse(&format!(
            "{HEAD}target: grpc://h:1\nmethod: a.S/M\nmessage: {{}}\nmessages:\n  - {{}}\n"
        ))
        .unwrap_err();
        assert_eq!(err.message, BOTH_MESSAGES_ERROR);
        assert_eq!(
            err.to_string(),
            format!("r.rman.yaml:8: {BOTH_MESSAGES_ERROR}")
        );
        let err = parse(&format!("{HEAD}target: grpc://h:1\nmethod: a.S/M\n")).unwrap_err();
        assert_eq!(err.message, NO_MESSAGE_ERROR);
    }

    #[test]
    fn targets_and_methods_are_checked() {
        for target in [
            "localhost:50051",
            "grpc://h:1",
            "grpcs://h.example:443/",
            "[::1]:9",
        ] {
            assert!(valid_target(target), "{target}");
        }
        for target in ["http://h:1", "h", "h:port", "grpc://:1", "a/b:1"] {
            assert!(!valid_target(target), "{target}");
        }
        for method in ["shop.v1.Catalog/GetProduct", "Svc/M"] {
            assert!(valid_method(method), "{method}");
        }
        for method in [
            "shop.v1.Catalog.GetProduct",
            "a.S/",
            "/M",
            "a..S/M",
            "a.S/M/x",
        ] {
            assert!(!valid_method(method), "{method}");
        }
        let err = parse(&format!(
            "{HEAD}target: http://h:1\nmethod: a.S/M\nmessage: {{}}\n"
        ))
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "r.rman.yaml:4: grpc: target must be host:port or a grpc:// or grpcs:// URL"
        );
        let err = parse(&format!(
            "{HEAD}target: h:1\nmethod: GetProduct\nmessage: {{}}\n"
        ))
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "r.rman.yaml:5: grpc: method must be fully qualified as package.Service/Method"
        );
    }

    #[test]
    fn messages_must_be_maps() {
        let err = parse(&format!("{HEAD}target: h:1\nmethod: a.S/M\nmessage: [1]\n")).unwrap_err();
        assert_eq!(err.message, "grpc: message must be a map");
        let err = parse(&format!(
            "{HEAD}target: h:1\nmethod: a.S/M\nmessages:\n  - {{}}\n  - 2\n"
        ))
        .unwrap_err();
        assert_eq!(err.message, "grpc: every entry of messages must be a map");
        let r = parse(&format!("{HEAD}target: h:1\nmethod: a.S/M\nmessages: []\n")).unwrap();
        assert!(r.has_message_list() && r.message_list().is_empty());
    }

    #[test]
    fn schema_and_stream_are_checked() {
        let base = format!("{HEAD}target: h:1\nmethod: a.S/M\nmessage: {{}}\n");
        let err = parse(&format!(
            "{base}schema:\n  protos: [a.proto]\n  descriptor_set: d.bin\n"
        ))
        .unwrap_err();
        assert!(err.message.contains("not both"), "{err}");
        let err = parse(&format!("{base}schema:\n  import_paths: [p]\n")).unwrap_err();
        assert!(err.message.contains("requires schema.protos"), "{err}");
        let err = parse(&format!("{base}stream:\n  max_messages: 0\n")).unwrap_err();
        assert!(err.message.contains("at least 1"), "{err}");
        let r = parse(&format!(
            "{base}schema:\n  protos: [protos/a.proto]\n  import_paths: [protos]\ntls:\n  ca_bundle: ca.pem\n"
        ))
        .unwrap();
        let paths: Vec<&str> = r.file_paths().iter().map(|p| p.get().as_str()).collect();
        assert_eq!(paths, ["ca.pem", "protos/a.proto", "protos"]);
    }

    #[test]
    fn status_code_names() {
        assert_eq!(grpc_code_name(5), "NOT_FOUND");
        assert_eq!(grpc_code_name(99), "UNKNOWN");
        assert_eq!(grpc_code_name(-1), "UNKNOWN");
        assert_eq!(parse_grpc_code("not_found"), Some(5));
        assert_eq!(parse_grpc_code(" 4 "), Some(4));
        assert_eq!(parse_grpc_code("NOPE"), None);
    }

    #[test]
    fn tls_maps_to_http_settings_and_hides_the_password() {
        let tls = GrpcTls {
            insecure: Some(true),
            client_pkcs12_password: Some("pkcs-x".into()),
            ..GrpcTls::default()
        };
        assert_eq!(tls.http().verify_tls, Some(false));
        assert!(!format!("{tls:?}").contains("pkcs-x"));
    }
}
