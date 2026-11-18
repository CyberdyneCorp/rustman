//! `kind: graphql` request files in a workspace (add-graphql-client tasks
//! 1.2 and 1.3): loading, and interpolation of the endpoint, headers and
//! variables.

use std::fs;
use std::path::Path;

use rustman_core::model::{DOCUMENT_SOURCE_ERROR, Request};
use rustman_core::vars::{ResolveError, ResolvedGraphql, Resolver, SecretLookup, resolve_graphql};
use rustman_core::{ConfigError, Workspace};
use serde_json::json;

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
            "[workspace]\nname = \"t\"\nversion = 1\n[variables]\nbase = \"http://api.local\"\n",
        );
        fixture.write(
            "environments/prod.yaml",
            "variables:\n  userId: '42'\nsecrets: [api_token]\n",
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

    fn resolve(&self, name: &str) -> Result<ResolvedGraphql, ResolveError> {
        let ws = self.load().unwrap();
        let request = &ws.find_by_name(name)[0];
        let env = ws.active_environment(Some("prod"), None).unwrap();
        let resolver = Resolver::for_request(&ws.settings, request, env, &Store);
        resolve_graphql(request, &resolver)
    }
}

struct Store;

impl SecretLookup for Store {
    fn secret(&self, env: &str, name: &str) -> Result<Option<String>, String> {
        Ok((env == "prod" && name == "api_token").then(|| "s3cr3t-gql".to_owned()))
    }
}

const USERS: &str = "version: 1
kind: graphql
name: users
endpoint: '{{base}}/graphql'
document: |
  query User($id: ID!) { user(id: $id) { name note(text: \"{{x}}\") } }
operation_name: User
variables:
  id: '{{userId}}'
  limit: 5
  filter:
    tags: ['{{userId}}', plain]
headers:
  - name: X-Token
    value: '{{api_token}}'
";

#[test]
fn a_valid_graphql_request_appears_in_its_collection() {
    let f = Fixture::new();
    f.write("api/users.rman.yaml", USERS);
    let ws = f.load().unwrap();
    let found = ws.find_by_name("users");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].request().kind(), "graphql");
    let Request::Graphql(graphql) = found[0].request() else {
        panic!("a graphql request");
    };
    assert_eq!(graphql.endpoint, "{{base}}/graphql");
    assert_eq!(ws.collections()[0].path, Path::new("api"));
}

#[test]
fn both_document_sources_fail_loading_naming_the_file() {
    let f = Fixture::new();
    f.write(
        "api/both.rman.yaml",
        "version: 1\nkind: graphql\nname: both\nendpoint: http://x\ndocument: '{ a }'\ndocument_file: q.graphql\n",
    );
    let err = f.load().unwrap_err().to_string();
    assert!(err.contains(DOCUMENT_SOURCE_ERROR), "{err}");
    assert!(err.contains("both.rman.yaml"), "{err}");
}

#[test]
fn endpoint_headers_and_variable_strings_are_interpolated() {
    let f = Fixture::new();
    f.write("api/users.rman.yaml", USERS);
    let resolved = f.resolve("users").unwrap();
    assert_eq!(resolved.http.url.text, "http://api.local/graphql");
    assert_eq!(resolved.operation_name.as_deref(), Some("User"));
    assert_eq!(
        serde_json::Value::Object(resolved.variables.clone()),
        json!({"id": "42", "limit": 5, "filter": {"tags": ["42", "plain"]}})
    );
    let header = &resolved.http.headers[0];
    assert_eq!(
        (header.name.text.as_str(), header.value.text.as_str()),
        ("X-Token", "s3cr3t-gql")
    );
}

#[test]
fn the_variables_map_is_sent_as_interpolated_strings() {
    let f = Fixture::new();
    f.write(
        "q.rman.yaml",
        "version: 1\nkind: graphql\nname: q\nendpoint: http://x\ndocument: '{ a }'\nvariables:\n  id: '{{userId}}'\n",
    );
    let resolved = f.resolve("q").unwrap();
    assert_eq!(
        serde_json::to_string(&resolved.variables).unwrap(),
        r#"{"id":"42"}"#
    );
}

#[test]
fn collection_headers_and_auth_are_inherited() {
    let f = Fixture::new();
    f.write(
        "api/_collection.yaml",
        "headers:\n  - name: X-Team\n    value: '{{userId}}'\nauth:\n  type: bearer\n  token: '{{api_token}}'\n",
    );
    f.write(
        "api/q.rman.yaml",
        "version: 1\nkind: graphql\nname: q\nendpoint: http://x\ndocument: '{ a }'\n",
    );
    let resolved = f.resolve("q").unwrap();
    assert_eq!(resolved.http.headers[0].value.text, "42");
    assert!(
        matches!(&resolved.http.auth, Some(rustman_core::model::Auth::Bearer { token }) if token == "s3cr3t-gql")
    );
}

#[test]
fn unresolved_placeholders_fail_and_list_every_name() {
    let f = Fixture::new();
    f.write(
        "q.rman.yaml",
        "version: 1\nkind: graphql\nname: q\nendpoint: '{{host}}/graphql'\ndocument: '{ a(x: \"{{in_document}}\") }'\nvariables:\n  a: '{{missing}}'\n",
    );
    let err = f.resolve("q").unwrap_err();
    assert_eq!(err.to_string(), "unresolved variable(s): host, missing");
}

#[test]
fn secrets_in_variables_are_redacted_in_debug_output() {
    let f = Fixture::new();
    f.write(
        "q.rman.yaml",
        "version: 1\nkind: graphql\nname: q\nendpoint: http://x\ndocument: '{ a }'\nvariables:\n  token: '{{api_token}}'\nconnection_params:\n  auth: 'Bearer {{api_token}}'\n",
    );
    let resolved = f.resolve("q").unwrap();
    assert_eq!(resolved.variables["token"], "s3cr3t-gql");
    assert_eq!(
        resolved.connection_params.as_ref().unwrap()["auth"],
        "Bearer s3cr3t-gql"
    );
    let shown = format!("{resolved:?} {resolved:#?}");
    assert!(!shown.contains("s3cr3t-gql"), "{shown}");
    assert!(shown.contains("redacted"), "{shown}");
}

#[test]
fn an_http_request_is_not_resolved_as_graphql() {
    let f = Fixture::new();
    f.write(
        "h.rman.yaml",
        "version: 1\nkind: http\nname: h\nurl: http://x\n",
    );
    assert!(matches!(f.resolve("h"), Err(ResolveError::Unsupported(_))));
}

#[test]
fn an_edited_graphql_request_is_written_back_with_every_field() {
    use rustman_core::format::write::graphql_to_yaml;
    use rustman_core::model::KindRegistry;

    let text = "version: 1\nkind: graphql\nname: Feed\ndescription: Live feed\ntags: [live]\n\
        endpoint: '{{base}}/graphql'\ndocument: |-\n  subscription Feed {\n    onMessage { n }\n  }\n\
        operation_name: Feed\nvariables:\n  limit: 5\n  name: \"{{user}}\"\n\
        headers:\n- name: X-Trace\n  value: abc\nauth:\n  type: bearer\n  token: '{{token}}'\n\
        transport: ws\nmethod: GET\napq: true\nschema_file: schema.graphql\n\
        ws_protocol: graphql-ws\nconnection_params:\n  token: '{{token}}'\n\
        max_events: 3\nduration: 1500ms\nassertions:\n- no_errors: true\n- errors_count: 0\n\
        captures:\n- var: n\n  from: json\n  path: $.data.onMessage.n\n";
    let registry = KindRegistry::default();
    let Request::Graphql(loaded) = registry.load(Path::new("feed.rman.yaml"), text).unwrap() else {
        panic!("a graphql request");
    };
    let written = graphql_to_yaml(&loaded);
    let Request::Graphql(again) = registry
        .load(Path::new("feed.rman.yaml"), &written)
        .unwrap()
    else {
        panic!("a graphql request");
    };
    let mut expected = loaded.clone();
    expected.source = written.clone();
    assert_eq!(*again, *expected, "{written}");
    assert!(written.contains("duration: '1500ms'"), "{written}");
    assert!(written.contains("ws_protocol: graphql-ws"), "{written}");
}
