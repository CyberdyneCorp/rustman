//! Introspection, the schema cache and `schema_file` (add-graphql-client
//! tasks 2.1 to 2.3), plus the request document (task 1.3), against
//! in-process servers.

use std::fs;
use std::path::Path;

use apollo_compiler::Schema;
use rustman_core::model::Request;
use rustman_core::settings::DEFAULT_MAX_SCHEMA_BYTES;
use rustman_core::vars::{ResolvedGraphql, Resolver, SecretLookup, resolve_graphql};
use rustman_core::{RequestRef, Workspace};
use rustman_graphql::{
    GraphqlError, GraphqlSchema, SCHEMA_CACHE_DIR, SchemaCache, SchemaOrigin, active_schema,
    cache_key, introspect, load_document, refresh_schema,
};
use rustman_http::{HttpEngine, SendOptions};
use rustman_test_support::graphql::INTROSPECTION_DISABLED;
use rustman_test_support::{GraphqlHttpServer, GraphqlServer, Introspection, Reply, TestServer};

const GITHUB: &str = include_str!("fixtures/github.graphql");
const LEGACY_JSON: &str = include_str!("fixtures/legacy.introspection.json");
const SECRET: &str = "s3cr3t-schema-canary";
/// The default `[graphql] max_schema_bytes`.
const LIMIT: u64 = DEFAULT_MAX_SCHEMA_BYTES;

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    /// A workspace whose `base` variable points at `base`.
    fn new(base: &str) -> Self {
        let fixture = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        fixture.write(
            "rustman.toml",
            &format!("[workspace]\nname = \"t\"\nversion = 1\n[variables]\nbase = \"{base}\"\n"),
        );
        fixture.write("environments/prod.yaml", "secrets: [api_token]\n");
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

    /// `api/q.rman.yaml` with the given extra fields.
    fn request(&self, extra: &str) {
        self.write(
            "api/q.rman.yaml",
            &format!(
                "version: 1\nkind: graphql\nname: q\nendpoint: '{{{{base}}}}/graphql'\ndocument: '{{ viewer {{ login }} }}'\n{extra}"
            ),
        );
    }

    fn workspace(&self) -> Workspace {
        Workspace::load(self.root()).unwrap_or_else(|e| panic!("{e}"))
    }

    fn cache(&self) -> SchemaCache {
        SchemaCache::for_workspace(self.root())
    }
}

struct Store;

impl SecretLookup for Store {
    fn secret(&self, env: &str, name: &str) -> Result<Option<String>, String> {
        Ok((env == "prod" && name == "api_token").then(|| SECRET.to_owned()))
    }
}

fn resolve(ws: &Workspace) -> (RequestRef<'_>, ResolvedGraphql) {
    let request = ws.find_by_name("q").remove(0);
    let env = ws.active_environment(Some("prod"), None).unwrap();
    let resolver = Resolver::for_request(&ws.settings, &request, env, &Store);
    let resolved = resolve_graphql(&request, &resolver).unwrap();
    (request, resolved)
}

/// The schema the request would be validated against.
fn active(f: &Fixture) -> Result<Option<rustman_graphql::ActiveSchema>, GraphqlError> {
    let ws = f.workspace();
    let (request, resolved) = resolve(&ws);
    active_schema(
        &request,
        &ws.settings.graphql,
        &resolved.http.url.text,
        &f.cache(),
    )
}

async fn refresh(f: &Fixture) -> Result<rustman_graphql::CachedSchema, GraphqlError> {
    let ws = f.workspace();
    let (_, resolved) = resolve(&ws);
    let engine = HttpEngine::new();
    refresh_schema(
        &engine,
        &SendOptions::default(),
        &resolved,
        &f.cache(),
        ws.settings.graphql.max_schema_bytes,
    )
    .await
}

/// `schema` with no built-in definitions, as SDL: equal maps compare
/// regardless of order.
fn assert_same_schema(ours: &Schema, expected: &Schema) {
    assert_eq!(ours.types, expected.types);
    assert_eq!(ours.directive_definitions, expected.directive_definitions);
    let roots = |s: &Schema| {
        let d = &s.schema_definition;
        (
            d.query.as_ref().map(|n| n.name.clone()),
            d.mutation.as_ref().map(|n| n.name.clone()),
            d.subscription.as_ref().map(|n| n.name.clone()),
        )
    };
    assert_eq!(roots(ours), roots(expected));
}

#[tokio::test]
async fn a_github_schema_round_trips_through_introspection() {
    let server = GraphqlServer::start(GITHUB).await;
    let f = Fixture::new(&server.url(""));
    f.request("");
    let ws = f.workspace();
    let (_, resolved) = resolve(&ws);
    let fetched = introspect(
        &HttpEngine::new(),
        &SendOptions::default(),
        &resolved,
        LIMIT,
    )
    .await
    .unwrap();
    assert!(!fetched.legacy);
    assert!(
        fetched.schema.warnings().is_empty(),
        "{:?}",
        fetched.schema.warnings()
    );
    let expected = Schema::parse_and_validate(GITHUB, "github.graphql").unwrap();
    assert_same_schema(fetched.schema.schema(), &expected);
    assert_eq!(fetched.schema.type_count(), 27);
    insta::assert_snapshot!("github_sdl", fetched.schema.sdl());
    let hit = &server.hits()[0];
    assert_eq!(hit.method, "POST");
    assert_eq!(
        hit.header("accept"),
        Some("application/graphql-response+json, application/json")
    );
    assert_eq!(hit.header("content-type"), Some("application/json"));
}

/// About the size of a large commerce API: thousands of types.
fn large_sdl(objects: usize) -> String {
    let mut sdl = String::from("type Query {\n");
    for i in 0..objects {
        sdl.push_str(&format!(
            "  item{i}(id: ID!, filter: Filter{}): Item{i}\n",
            i % 200
        ));
    }
    sdl.push_str("}\n");
    for i in 0..objects {
        let next = (i + 1) % objects;
        sdl.push_str(&format!(
            "\"Item number {i}\"\ntype Item{i} implements Entity {{\n  id: ID!\n  title: String @deprecated(reason: \"Use name\")\n  name: String!\n  tags(first: Int = 10): [String!]!\n  status: Status{}\n  next: Item{next}\n}}\n",
            i % 200
        ));
    }
    for i in 0..200 {
        sdl.push_str(&format!(
            "enum Status{i} {{\n  ACTIVE\n  ARCHIVED\n}}\ninput Filter{i} {{\n  query: String\n  status: Status{i} = ACTIVE\n}}\n"
        ));
    }
    sdl.push_str("interface Entity {\n  id: ID!\n}\n");
    sdl
}

#[tokio::test]
async fn a_schema_with_thousands_of_types_converts() {
    let sdl = large_sdl(1500);
    let server = GraphqlServer::start(&sdl).await;
    let f = Fixture::new(&server.url(""));
    f.request("");
    let ws = f.workspace();
    let (_, resolved) = resolve(&ws);
    let options = SendOptions {
        timeout: std::time::Duration::from_secs(300),
        ..SendOptions::default()
    };
    let fetched = introspect(&HttpEngine::new(), &options, &resolved, LIMIT)
        .await
        .unwrap();
    let expected = Schema::parse_and_validate(sdl, "large.graphql").unwrap();
    assert_eq!(fetched.schema.type_count(), 1500 + 400 + 2);
    assert_same_schema(fetched.schema.schema(), &expected);
}

#[test]
fn a_legacy_introspection_result_converts() {
    let value: serde_json::Value = serde_json::from_str(LEGACY_JSON).unwrap();
    let schema = GraphqlSchema::from_introspection(&value, "legacy").unwrap();
    assert!(schema.warnings().is_empty(), "{:?}", schema.warnings());
    insta::assert_snapshot!("legacy_sdl", schema.sdl());
}

#[tokio::test]
async fn servers_without_newer_fields_get_the_legacy_query() {
    let server = GraphqlServer::with_introspection(GITHUB, Introspection::LegacyOnly).await;
    let f = Fixture::new(&server.url(""));
    f.request("");
    let ws = f.workspace();
    let (_, resolved) = resolve(&ws);
    let fetched = introspect(
        &HttpEngine::new(),
        &SendOptions::default(),
        &resolved,
        LIMIT,
    )
    .await
    .unwrap();
    assert!(fetched.legacy);
    assert_eq!(server.introspection_hits(), 2);
    assert!(fetched.schema.schema().types.contains_key("Repository"));
}

#[tokio::test]
async fn a_server_rejecting_introspection_reports_the_schema_file_hint() {
    let server = GraphqlServer::with_introspection(GITHUB, Introspection::Disabled).await;
    let f = Fixture::new(&server.url(""));
    f.request("");
    let err = refresh(&f).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "graphql: introspection failed: {INTROSPECTION_DISABLED}; set schema_file to validate offline"
        )
    );
    assert_eq!(server.introspection_hits(), 1);
    assert!(!f.root().join(SCHEMA_CACHE_DIR).exists());
}

#[tokio::test]
async fn a_non_graphql_response_fails_introspection() {
    let server = TestServer::http().await;
    let f = Fixture::new(&server.url("/status/502"));
    f.write(
        "api/q.rman.yaml",
        "version: 1\nkind: graphql\nname: q\nendpoint: '{{base}}'\ndocument: '{ a }'\n",
    );
    let err = refresh(&f).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "graphql: introspection failed: HTTP 502: response is not a GraphQL result; set schema_file to validate offline"
    );
}

#[tokio::test]
async fn refresh_writes_the_cache_with_endpoint_fetch_time_and_sdl_hash() {
    let server = GraphqlServer::start(GITHUB).await;
    let f = Fixture::new(&server.url(""));
    f.request("");
    let cached = refresh(&f).await.unwrap();
    let endpoint = server.url("/graphql");
    let key = cache_key(&endpoint).unwrap();
    let path = f.root().join(SCHEMA_CACHE_DIR).join(format!("{key}.json"));
    assert_eq!(cached.path, path);
    let file: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(file["endpoint"], endpoint);
    assert_eq!(file["sdl_sha256"], cached.schema.sdl_sha256());
    assert_eq!(file["type_count"], 27);
    assert!(file["fetched_at"].as_str().unwrap().ends_with('Z'));
    assert!(file["introspection"]["__schema"]["types"].is_array());
    let loaded = f.cache().load(&endpoint).unwrap().unwrap();
    assert_eq!(loaded.schema.sdl(), cached.schema.sdl());
    assert_eq!(loaded.fetched_at, cached.fetched_at);
}

#[tokio::test]
async fn a_cached_schema_is_used_without_introspection() {
    let server = GraphqlServer::start(GITHUB).await;
    let f = Fixture::new(&server.url(""));
    f.request("");
    assert!(active(&f).unwrap().is_none(), "no schema before a refresh");
    refresh(&f).await.unwrap();
    assert_eq!(server.introspection_hits(), 1);
    for _ in 0..3 {
        let schema = active(&f).unwrap().unwrap();
        assert!(matches!(schema.origin, SchemaOrigin::Cache { .. }));
        assert!(schema.schema.schema().types.contains_key("Issue"));
    }
    assert_eq!(server.hits().len(), 1, "validation never introspects");
}

#[tokio::test]
async fn a_schema_file_takes_precedence_over_the_cache() {
    let server = GraphqlServer::start(GITHUB).await;
    let f = Fixture::new(&server.url(""));
    f.request("");
    let cached = refresh(&f).await.unwrap();
    // A corrupt cache proves the cache is not read at all.
    fs::write(&cached.path, "not json").unwrap();
    f.write("api/schema.graphql", "type Query { onlyInFile: Int }\n");
    f.request("schema_file: schema.graphql\n");
    let schema = active(&f).unwrap().unwrap();
    assert_eq!(
        schema.origin,
        SchemaOrigin::RequestFile("schema.graphql".into())
    );
    let query = &schema.schema.schema().types["Query"];
    assert!(query.as_object().unwrap().fields.contains_key("onlyInFile"));
}

#[test]
fn a_schema_file_may_be_introspection_json() {
    let f = Fixture::new("http://127.0.0.1:9");
    f.write("api/schema.json", LEGACY_JSON);
    f.request("schema_file: schema.json\n");
    let schema = active(&f).unwrap().unwrap();
    assert!(schema.schema.schema().types.contains_key("Role"));
}

#[test]
fn an_endpoint_entry_in_rustman_toml_sets_the_schema_file() {
    let f = Fixture::new("http://127.0.0.1:9");
    let toml = "[workspace]\nname = \"t\"\nversion = 1\n[variables]\nbase = \"http://127.0.0.1:9\"\n\n[[graphql.endpoints]]\nurl = \"http://127.0.0.1:9/graphql\"\nschema_file = \"schemas/api.graphql\"\n";
    f.write("rustman.toml", toml);
    f.write("schemas/api.graphql", "type Query { fromSettings: Int }\n");
    f.request("");
    let schema = active(&f).unwrap().unwrap();
    assert_eq!(
        schema.origin,
        SchemaOrigin::SettingsFile("schemas/api.graphql".into())
    );
    f.write("api/own.graphql", "type Query { fromRequest: Int }\n");
    f.request("schema_file: own.graphql\n");
    let schema = active(&f).unwrap().unwrap();
    assert_eq!(
        schema.origin,
        SchemaOrigin::RequestFile("own.graphql".into())
    );
}

#[test]
fn schema_files_must_stay_inside_the_workspace() {
    let f = Fixture::new("http://127.0.0.1:9");
    f.request("schema_file: ../../outside.graphql\n");
    let err = active(&f).unwrap_err().to_string();
    assert!(err.contains("escapes workspace"), "{err}");
}

#[tokio::test]
async fn secrets_never_reach_the_cache_file() {
    let server = GraphqlServer::start(GITHUB).await;
    let f = Fixture::new(&server.url(""));
    f.write(
        "api/q.rman.yaml",
        "version: 1\nkind: graphql\nname: q\nendpoint: '{{base}}/graphql/{{api_token}}'\ndocument: '{ viewer { login } }'\nheaders:\n  - name: X-Api-Key\n    value: '{{api_token}}'\n",
    );
    let cached = refresh(&f).await.unwrap();
    assert_eq!(server.hits()[0].header("x-api-key"), Some(SECRET));
    let text = fs::read_to_string(&cached.path).unwrap();
    assert!(!text.contains(SECRET), "{text}");
    assert!(cached.endpoint.contains("redacted"), "{}", cached.endpoint);
    assert!(!cached.endpoint.contains(SECRET));
}

#[test]
fn the_document_is_sent_byte_for_byte() {
    let f = Fixture::new("http://127.0.0.1:9");
    let document = "query {\r\n  a(x: \"{{x}}\") # {{y}}\r\n}\r\n";
    f.write("api/q.graphql", document);
    f.write(
        "api/q.rman.yaml",
        "version: 1\nkind: graphql\nname: q\nendpoint: '{{base}}'\ndocument_file: q.graphql\n",
    );
    let ws = f.workspace();
    let (request, _) = resolve(&ws);
    let Request::Graphql(graphql) = request.request() else {
        panic!("graphql");
    };
    assert_eq!(load_document(&request, graphql).unwrap(), document);
    f.write(
        "api/q.rman.yaml",
        "version: 1\nkind: graphql\nname: q\nendpoint: '{{base}}'\ndocument: '{ a(x: \"{{x}}\") }'\n",
    );
    let ws = f.workspace();
    let (request, _) = resolve(&ws);
    let Request::Graphql(graphql) = request.request() else {
        panic!("graphql");
    };
    assert_eq!(
        load_document(&request, graphql).unwrap(),
        "{ a(x: \"{{x}}\") }"
    );
}

/// Regression (#70): an introspection response larger than `[http]
/// max_body_bytes` was spilled to disk and read back whole, bounded only by
/// `max_download_bytes`. It is now held in memory up to `[graphql]
/// max_schema_bytes`, whatever `max_body_bytes` is, and a larger one is
/// refused as soon as the limit is passed, never spilled or read back.
#[tokio::test]
async fn introspection_is_bounded_by_max_schema_bytes() {
    let size = LEGACY_JSON.len() as u64;
    let server =
        GraphqlHttpServer::start(|_| Reply::raw(200, "application/json", LEGACY_JSON)).await;
    let f = Fixture::new(&server.url(""));
    f.request("");
    let ws = f.workspace();
    let (_, resolved) = resolve(&ws);
    let mut options = SendOptions::default();
    // Below the schema: without the schema limit, the body would spill.
    options.body.max_in_memory = 1024;
    let engine = HttpEngine::new();

    let fetched = introspect(&engine, &options, &resolved, size)
        .await
        .unwrap();
    assert!(fetched.schema.schema().types.contains_key("Query"));

    let err = introspect(&engine, &options, &resolved, size - 1)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "graphql: introspection failed: the response is larger than the {} byte limit \
             ([graphql] max_schema_bytes); set schema_file to validate offline",
            size - 1
        )
    );
    assert_eq!(server.hits().len(), 2, "an oversized answer is not resent");
}

/// A refused schema leaves the cache as it was.
#[tokio::test]
async fn an_oversized_schema_is_not_cached() {
    let server =
        GraphqlHttpServer::start(|_| Reply::raw(200, "application/json", LEGACY_JSON)).await;
    let f = Fixture::new(&server.url(""));
    f.write(
        "rustman.toml",
        &format!(
            "[workspace]\nname = \"t\"\nversion = 1\n[variables]\nbase = \"{}\"\n\
             [graphql]\nmax_schema_bytes = 1024\n",
            server.url("")
        ),
    );
    f.request("");
    let err = refresh(&f).await.unwrap_err();
    assert!(
        err.to_string()
            .contains("larger than the 1024 byte limit ([graphql] max_schema_bytes)"),
        "{err}"
    );
    assert!(!f.root().join(SCHEMA_CACHE_DIR).exists());
}
