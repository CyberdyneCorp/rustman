//! `kind: grpc` request files in a workspace (add-grpc-client tasks 1.2 and
//! 1.3): registration, the file model and its load errors, interpolation,
//! schema path containment, and redaction of recorded metadata.

use std::fs;
use std::path::Path;

use rustman_core::history::{Entry, HistoryStore, NewEntry};
use rustman_core::model::{
    BOTH_MESSAGES_ERROR, GrpcTransport, KindRegistry, NO_MESSAGE_ERROR, Request,
};
use rustman_core::run::RequestRecord;
use rustman_core::vars::{ResolveError, ResolvedGrpc, Resolver, SecretLookup, resolve_grpc};
use rustman_core::{ConfigError, Workspace};
use serde_json::json;

const TOKEN: &str = "tok-SECRET-1234";

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        fixture.write(
            "rustman.toml",
            "[workspace]\nname = \"t\"\nversion = 1\n[variables]\ngrpc_host = \"grpc://localhost:50051\"\n",
        );
        fixture.write(
            "environments/prod.yaml",
            "variables:\n  product: p-1\n  proto_dir: ../..\nsecrets: [token]\n",
        );
        fixture
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.root().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn load(&self) -> Result<Workspace, ConfigError> {
        Workspace::load(self.root())
    }

    fn resolve(&self, name: &str) -> Result<ResolvedGrpc, ResolveError> {
        let ws = self.load().unwrap();
        let request = &ws.find_by_name(name)[0];
        let env = ws.active_environment(Some("prod"), None).unwrap();
        let resolver = Resolver::for_request(&ws.settings, request, env, &Store);
        resolve_grpc(request, &resolver)
    }
}

struct Store;

impl SecretLookup for Store {
    fn secret(&self, env: &str, name: &str) -> Result<Option<String>, String> {
        Ok((env == "prod" && name == "token").then(|| TOKEN.to_owned()))
    }
}

/// The load error of a single file `text`, as the workspace reports it.
fn load_error(text: &str) -> String {
    let f = Fixture::new();
    f.write("grpc/r.rman.yaml", text);
    match f.load() {
        Err(ConfigError::Invalid(errors)) => errors[0].to_string().replace('\\', "/"),
        other => panic!("expected a load error, got {:?}", other.map(|_| ())),
    }
}

const HEAD: &str = "version: 1\nkind: grpc\nname: r\n";

const UNARY: &str = "version: 1
kind: grpc
name: Get product
target: \"{{grpc_host}}\"
method: shop.v1.Catalog/GetProduct
message: {id: \"p-1\"}
";

#[test]
fn grpc_is_a_registered_kind() {
    assert!(KindRegistry::default().names().contains(&"grpc"));
    let err = load_error("version: 1\nkind: soap\nname: r\n");
    assert!(err.contains("(expected one of: "), "{err}");
    let listed = err.split("expected one of: ").nth(1).unwrap();
    assert!(listed.split([',', ' ', ')']).any(|k| k == "grpc"), "{err}");
}

#[test]
fn a_valid_unary_file_loads() {
    let f = Fixture::new();
    f.write("grpc/get.rman.yaml", UNARY);
    let ws = f.load().unwrap();
    let found = ws.find_by_name("Get product");
    let Request::Grpc(grpc) = found[0].request() else {
        panic!("a grpc request");
    };
    assert_eq!(grpc.target, "{{grpc_host}}");
    assert_eq!(grpc.method, "shop.v1.Catalog/GetProduct");
    assert_eq!(grpc.message_list(), [&json!({"id": "p-1"})]);
    assert!(!grpc.has_message_list());
}

#[test]
fn every_field_loads() {
    let text = format!(
        "{HEAD}description: all fields
tags: [smoke]
target: grpcs://api.example:443
method: shop.v1.Catalog/UploadProducts
messages:
  - {{id: a}}
  - {{id: b}}
metadata:
  - name: authorization
    value: 'Bearer {{{{token}}}}'
  - name: trace-bin
    value: AQID
deadline_ms: 200
tls:
  enabled: true
  insecure: false
  ca_bundle: certs/ca.pem
transport: grpc-web
schema:
  protos: [protos/shop/v1/catalog.proto]
  import_paths: [protos]
stream:
  max_messages: 5
  duration_ms: 1000
variables:
  x: 1
"
    );
    let f = Fixture::new();
    f.write("grpc/all.rman.yaml", &text);
    let ws = f.load().unwrap();
    let Request::Grpc(grpc) = ws.find_by_name("r")[0].request() else {
        panic!("a grpc request");
    };
    assert_eq!(grpc.message_list().len(), 2);
    assert!(grpc.has_message_list());
    assert_eq!(grpc.metadata.as_ref().unwrap()[1].name, "trace-bin");
    assert_eq!(grpc.deadline_ms, Some(200));
    assert_eq!(grpc.effective_transport(), GrpcTransport::GrpcWeb);
    assert_eq!(grpc.effective_stream().effective_max_messages(), 5);
    assert_eq!(grpc.tls.as_ref().unwrap().enabled, Some(true));
}

#[test]
fn both_message_fields_fail_to_load() {
    let err = load_error(&format!(
        "{HEAD}target: localhost:50051\nmethod: a.S/M\nmessage: {{}}\nmessages: [{{}}]\n"
    ));
    assert!(err.ends_with(BOTH_MESSAGES_ERROR), "{err}");
    assert_eq!(
        BOTH_MESSAGES_ERROR,
        "grpc: exactly one of message or messages is allowed"
    );
    let err = load_error(&format!("{HEAD}target: localhost:50051\nmethod: a.S/M\n"));
    assert!(err.ends_with(NO_MESSAGE_ERROR), "{err}");
}

#[test]
fn unknown_fields_are_refused() {
    let err = load_error(&format!(
        "{HEAD}target: h:1\nmethod: a.S/M\nmessage: {{}}\nheaders: []\n"
    ));
    assert!(err.contains("headers"), "{err}");
}

#[test]
fn target_metadata_and_message_strings_are_interpolated() {
    let f = Fixture::new();
    f.write(
        "grpc/get.rman.yaml",
        "version: 1
kind: grpc
name: get
target: '{{grpc_host}}'
method: shop.v1.Catalog/GetProduct
message:
  id: '{{product}}'
  tags: ['{{product}}', 2]
  '{{product}}': kept
metadata:
  - name: authorization
    value: 'Bearer {{token}}'
  - name: x-off
    value: '{{missing_is_fine_when_disabled}}'
    enabled: false
",
    );
    let resolved = f.resolve("get").unwrap();
    assert_eq!(resolved.target(), "grpc://localhost:50051");
    assert_eq!(
        resolved.messages,
        [json!({"id": "p-1", "tags": ["p-1", 2], "{{product}}": "kept"})]
    );
    let metadata = &resolved.request.headers;
    assert_eq!(metadata.len(), 1);
    assert_eq!(metadata[0].value.text, format!("Bearer {TOKEN}"));
    assert!(!format!("{resolved:?}").contains(TOKEN));
}

#[test]
fn missing_variables_are_reported() {
    let f = Fixture::new();
    f.write(
        "grpc/get.rman.yaml",
        &format!("{HEAD}target: '{{{{nope}}}}'\nmethod: a.S/M\nmessage: {{id: '{{{{nada}}}}'}}\n"),
    );
    let err = f.resolve("r").unwrap_err().to_string();
    assert!(err.contains("nope") && err.contains("nada"), "{err}");
}

/// Spec "Token in metadata": the server gets the real token (the resolved
/// value), and history stores `Bearer «redacted:token»`.
#[test]
fn history_stores_the_metadata_token_redacted() {
    let f = Fixture::new();
    f.write(
        "grpc/get.rman.yaml",
        "version: 1
kind: grpc
name: get
target: '{{grpc_host}}'
method: shop.v1.Catalog/GetProduct
message: {note: 'token {{token}}'}
metadata:
  - name: authorization
    value: 'Bearer {{token}}'
",
    );
    let resolved = f.resolve("get").unwrap();
    assert_eq!(
        resolved.request.headers[0].value.text,
        format!("Bearer {TOKEN}")
    );
    let sent = resolved.sent(&resolved.redactor());
    assert_eq!(sent.headers[0].value, "Bearer «redacted:token»");
    assert_eq!(
        sent.url,
        "grpc://localhost:50051/shop.v1.Catalog/GetProduct"
    );
    let record = RequestRecord::new("grpc/get".into(), "get".into(), "grpc/get.rman.yaml".into());
    let entry = Entry::new(NewEntry {
        record: &record,
        sent,
        request: Some("grpc/get.rman.yaml"),
        environment: Some("prod"),
        spill: None,
        replay: None,
    });
    let history = tempfile::tempdir().unwrap();
    let store = HistoryStore::at(history.path(), 100);
    store.record(&entry).unwrap();
    let stored = store.get(&entry.id).unwrap();
    assert_eq!(stored.sent.headers[0].value, "Bearer «redacted:token»");
    let text = fs::read_to_string(history.path().join(format!("{}.json", entry.id))).unwrap();
    assert!(!text.contains(TOKEN), "{text}");
    assert!(text.contains("Bearer «redacted:token»"), "{text}");
}

#[test]
fn schema_paths_escaping_the_workspace_are_refused() {
    let err = load_error(&format!(
        "{HEAD}target: h:1\nmethod: a.S/M\nmessage: {{}}\nschema:\n  protos: [../../x.proto]\n"
    ));
    assert!(err.contains("file path escapes workspace"), "{err}");
    let f = Fixture::new();
    f.write(
        "grpc/r.rman.yaml",
        &format!(
            "{HEAD}target: h:1\nmethod: a.S/M\nmessage: {{}}\nschema:\n  protos: ['{{{{proto_dir}}}}/x.proto']\n"
        ),
    );
    let err = f.resolve("r").unwrap_err().to_string();
    assert!(err.contains("file path escapes workspace"), "{err}");
}

#[test]
fn schema_paths_resolve_against_the_request_folder() {
    let f = Fixture::new();
    f.write(
        "grpc/r.rman.yaml",
        &format!(
            "{HEAD}target: h:1\nmethod: a.S/M\nmessage: {{}}\nschema:\n  protos: [protos/a.proto]\n  import_paths: [protos]\n"
        ),
    );
    let resolved = f.resolve("r").unwrap();
    let root = dunce::canonicalize(f.root()).unwrap();
    let proto = &resolved.schema.protos[0];
    assert_eq!(proto.raw, "protos/a.proto");
    assert!(proto.path.ends_with(Path::new("grpc/protos/a.proto")));
    assert!(proto.path.starts_with(f.root()) || proto.path.starts_with(&root));
    assert!(!resolved.schema.is_reflection());
}

/// Canary: a literal credential in `authorization` metadata and a secret
/// inside a message never reach the recorded request.
#[test]
fn recorded_requests_hide_literal_credentials_and_message_secrets() {
    let f = Fixture::new();
    f.write(
        "grpc/lit.rman.yaml",
        "version: 1
kind: grpc
name: lit
target: localhost:50051
method: a.S/M
messages:
  - {password: '{{token}}'}
metadata:
  - name: Authorization
    value: Bearer literal-CANARY-9876
",
    );
    let resolved = f.resolve("lit").unwrap();
    let sent = resolved.sent(&resolved.redactor());
    let shown = serde_json::to_string(&sent).unwrap();
    assert!(!shown.contains("literal-CANARY-9876"), "{shown}");
    assert!(!shown.contains(TOKEN), "{shown}");
    assert_eq!(sent.headers[0].value, "Bearer «redacted:Authorization»");
    assert_eq!(
        sent.body.as_deref(),
        Some(r#"[{"password":"«redacted:token»"}]"#)
    );
}
