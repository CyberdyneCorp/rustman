//! Execution over HTTP (add-graphql-client tasks 4.1 to 4.4) against an
//! in-process scripted server: the exact payloads, response parsing,
//! multipart uploads and Automatic Persisted Queries.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rustman_core::model::{GRAPHQL_ACCEPT, Request};
use rustman_core::vars::{ResolvedGraphql, Resolver, SecretLookup, resolve_graphql};
use rustman_core::{RequestRef, Workspace};
use rustman_graphql::schema::sha256_hex;
use rustman_graphql::{
    ApqOutcome, GraphqlError, HttpOperation, HttpResult, NOT_GRAPHQL, ResultStatus, execute_http,
    load_document,
};
use rustman_http::{HttpEngine, SendOptions};
use rustman_test_support::{GraphqlHttpServer, RecordedRequest, Reply};
use serde_json::{Value, json};

const SECRET: &str = "s3cr3t-graphql-exec-canary";

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new(base: &str) -> Self {
        let fixture = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        fixture.write(
            "rustman.toml",
            &format!(
                "[workspace]\nname = \"t\"\nversion = 1\n[variables]\nbase = \"{base}\"\nuserId = \"42\"\n"
            ),
        );
        fixture.write("environments/prod.yaml", "secrets: [api_token]\n");
        fixture
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.dir.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// `api/q.rman.yaml`: a graphql request with `body` after the endpoint.
    fn request(&self, body: &str) {
        self.write(
            "api/q.rman.yaml",
            &format!(
                "version: 1\nkind: graphql\nname: q\nendpoint: '{{{{base}}}}/graphql'\n{body}"
            ),
        );
    }

    fn workspace(&self) -> Workspace {
        Workspace::load(self.dir.path()).unwrap_or_else(|e| panic!("{e}"))
    }

    fn root(&self) -> &Path {
        self.dir.path()
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

/// Prepare and send the fixture's request.
async fn run(f: &Fixture) -> Result<HttpResult, GraphqlError> {
    run_with(f, &SendOptions::default()).await
}

/// [`run`] with `options`.
async fn run_with(f: &Fixture, options: &SendOptions) -> Result<HttpResult, GraphqlError> {
    let ws = f.workspace();
    let (request, resolved) = resolve(&ws);
    let Request::Graphql(graphql) = request.request() else {
        panic!("a graphql request")
    };
    let document = load_document(&request, graphql)?;
    let operation = HttpOperation::for_request(&request, graphql, document, &resolved)?;
    let engine = HttpEngine::new();
    execute_http(&engine, options, &resolved, &operation).await
}

async fn ok_server() -> GraphqlHttpServer {
    GraphqlHttpServer::start(|_| Reply::graphql(json!({"data": {"user": {"name": "Ada"}}}))).await
}

#[tokio::test]
async fn a_query_is_posted_as_json_with_the_graphql_accept_header() {
    let server = ok_server().await;
    let f = Fixture::new(&server.url(""));
    f.request(
        "document: 'query Users($id: ID!) { user(id: $id, note: \"{{x}}\") { name } }'\n\
         operation_name: Users\nvariables:\n  id: '{{userId}}'\n",
    );
    let result = run(&f).await.unwrap();
    assert_eq!(result.requests, 1);
    assert_eq!(result.apq, None);
    let hit = &server.hits()[0];
    assert_eq!(hit.method, "POST");
    assert_eq!(hit.uri, "/graphql");
    assert_eq!(hit.header("content-type"), Some("application/json"));
    assert_eq!(hit.header("accept"), Some(GRAPHQL_ACCEPT));
    assert_eq!(
        hit.json(),
        json!({
            "query": "query Users($id: ID!) { user(id: $id, note: \"{{x}}\") { name } }",
            "operationName": "Users",
            "variables": {"id": "42"},
        })
    );
    let response = result.response.unwrap();
    assert_eq!(response.status, ResultStatus::Ok);
    assert_eq!(response.data.unwrap()["user"]["name"], "Ada");
}

#[tokio::test]
async fn absent_members_are_left_out_and_a_set_accept_is_kept() {
    let server = ok_server().await;
    let f = Fixture::new(&server.url(""));
    f.request("document: '{ user(id: 1) { name } }'\nheaders:\n  - name: Accept\n    value: application/json\n");
    run(&f).await.unwrap();
    let hit = &server.hits()[0];
    assert_eq!(hit.json(), json!({"query": "{ user(id: 1) { name } }"}));
    assert_eq!(hit.header("accept"), Some("application/json"));
}

#[tokio::test]
async fn a_get_query_sends_url_parameters() {
    let server = ok_server().await;
    let f = Fixture::new(&server.url(""));
    f.request(
        "document: 'query U($id: ID!) { user(id: $id) { name } }'\noperation_name: U\n\
         method: GET\nvariables:\n  id: '{{userId}}'\n",
    );
    run(&f).await.unwrap();
    let hit = &server.hits()[0];
    assert_eq!(hit.method, "GET");
    assert!(hit.body.is_empty());
    assert_eq!(
        hit.query_pairs(),
        [
            (
                "query".to_owned(),
                "query U($id: ID!) { user(id: $id) { name } }".to_owned()
            ),
            ("operationName".to_owned(), "U".to_owned()),
            ("variables".to_owned(), r#"{"id":"42"}"#.to_owned()),
        ]
    );
}

#[tokio::test]
async fn nothing_is_sent_for_a_mutation_over_get_or_an_ambiguous_document() {
    let server = ok_server().await;
    let f = Fixture::new(&server.url(""));
    f.request("document: 'mutation { logout }'\nmethod: GET\n");
    let err = run(&f).await.unwrap_err();
    assert_eq!(err.to_string(), "graphql: mutations cannot use GET");
    f.request("document: 'query A { a } query B { b }'\n");
    let err = run(&f).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "graphql: document has 2 operations; set operation_name"
    );
    assert!(server.hits().is_empty());
}

#[tokio::test]
async fn partial_data_keeps_data_and_errors_side_by_side() {
    let server = GraphqlHttpServer::start(|_| {
        Reply::graphql(json!({
            "data": {"user": {"name": "Ada", "email": null}},
            "errors": [{"message": "not allowed", "path": ["user", "email"],
                        "locations": [{"line": 1, "column": 22}]}],
            "extensions": {"traceId": "t-1"},
        }))
    })
    .await;
    let f = Fixture::new(&server.url(""));
    f.request("document: '{ user(id: 1) { name email } }'\n");
    let result = run(&f).await.unwrap();
    assert_eq!(result.exchange.status, 200);
    let response = result.response.unwrap();
    assert_eq!(response.status, ResultStatus::Partial);
    assert_eq!(response.data.unwrap()["user"]["name"], "Ada");
    assert_eq!(response.errors[0].path_text(), "user.email");
    assert_eq!(response.extensions.unwrap()["traceId"], "t-1");
}

#[tokio::test]
async fn an_html_502_is_not_a_graphql_result_and_keeps_its_raw_body() {
    let page = "<html><body>502 Bad Gateway</body></html>";
    let server = GraphqlHttpServer::start(move |_| Reply::raw(502, "text/html", page)).await;
    let f = Fixture::new(&server.url(""));
    f.request("document: '{ user(id: 1) { name } }'\n");
    let result = run(&f).await.unwrap();
    assert_eq!(result.exchange.status, 502);
    assert!(result.response.is_none());
    assert_eq!(result.exchange.body.as_ref(), page.as_bytes());
    assert_eq!(NOT_GRAPHQL, "graphql: response is not a GraphQL result");
}

/// The parts of a `multipart/form-data` body: (headers, content) by name.
fn multipart_parts(hit: &RecordedRequest) -> Vec<(String, String, Vec<u8>)> {
    let content_type = hit.header("content-type").unwrap();
    let boundary = content_type
        .strip_prefix("multipart/form-data; boundary=")
        .expect("a multipart body");
    let body = hit.body.to_vec();
    let delimiter = format!("--{boundary}");
    let text = String::from_utf8(body).unwrap();
    text.split(&delimiter)
        .filter(|chunk| chunk.starts_with("\r\n"))
        .map(|chunk| {
            let (headers, content) = chunk[2..].split_once("\r\n\r\n").unwrap();
            let name = headers
                .split("name=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .unwrap()
                .to_owned();
            let content = content.strip_suffix("\r\n").unwrap_or(content);
            (name, headers.to_owned(), content.as_bytes().to_vec())
        })
        .collect()
}

#[tokio::test]
async fn an_upload_is_sent_as_a_graphql_multipart_request() {
    let server =
        GraphqlHttpServer::start(|_| Reply::graphql(json!({"data": {"upload": true}}))).await;
    let f = Fixture::new(&server.url(""));
    f.write("api/avatar.png", "PNGDATA");
    f.request(
        "document: 'mutation($file: Upload!) { upload(file: $file) }'\n\
         variables:\n  file:\n    $file: ./avatar.png\n",
    );
    let result = run(&f).await.unwrap();
    assert_eq!(result.response.unwrap().status, ResultStatus::Ok);
    let hit = &server.hits()[0];
    let parts = multipart_parts(hit);
    let names: Vec<&str> = parts.iter().map(|(n, _, _)| n.as_str()).collect();
    assert_eq!(names, ["operations", "map", "0"]);
    let operations: Value = serde_json::from_slice(&parts[0].2).unwrap();
    assert_eq!(
        operations,
        json!({
            "query": "mutation($file: Upload!) { upload(file: $file) }",
            "variables": {"file": null},
        })
    );
    let map: Value = serde_json::from_slice(&parts[1].2).unwrap();
    assert_eq!(map, json!({"0": ["variables.file"]}));
    let (_, headers, content) = &parts[2];
    assert!(headers.contains("filename=\"avatar.png\""), "{headers}");
    assert!(headers.contains("Content-Type: image/png"), "{headers}");
    assert_eq!(content, b"PNGDATA");
}

#[tokio::test]
async fn a_missing_upload_file_fails_before_send() {
    let server = ok_server().await;
    let f = Fixture::new(&server.url(""));
    f.request(
        "document: 'mutation($file: Upload!) { upload(file: $file) }'\n\
         variables:\n  file:\n    $file: ./avatar.png\n",
    );
    let err = run(&f).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "graphql: upload file not found: ./avatar.png"
    );
    assert!(server.hits().is_empty());
    assert!(f.root().join("api").is_dir());
}

/// A server that knows a persisted query only once it was sent in full.
async fn apq_server() -> GraphqlHttpServer {
    let known: Arc<Mutex<HashSet<String>>> = Arc::default();
    GraphqlHttpServer::start(move |hit| {
        let body = hit.json();
        let hash = body["extensions"]["persistedQuery"]["sha256Hash"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let mut known = known.lock().unwrap();
        if body.get("query").is_some() {
            known.insert(hash);
        } else if !known.contains(&hash) {
            return Reply::graphql(json!({"errors": [{"message": "PersistedQueryNotFound",
                "extensions": {"code": "PERSISTED_QUERY_NOT_FOUND"}}]}));
        }
        Reply::graphql(json!({"data": {"users": []}}))
    })
    .await
}

#[tokio::test]
async fn apq_sends_the_hash_first_and_registers_on_a_miss() {
    let server = apq_server().await;
    let f = Fixture::new(&server.url(""));
    let document = "{ users { id } }";
    f.request(&format!("document: '{document}'\napq: true\n"));
    let result = run(&f).await.unwrap();
    assert_eq!(result.requests, 2);
    assert_eq!(result.apq, Some(ApqOutcome::MissRegistered));
    assert_eq!(result.apq.unwrap().to_string(), "APQ miss (registered)");
    assert_eq!(result.response.unwrap().status, ResultStatus::Ok);
    let hits = server.hits();
    assert_eq!(hits.len(), 2);
    let persisted =
        json!({"persistedQuery": {"version": 1, "sha256Hash": sha256_hex(document.as_bytes())}});
    assert_eq!(hits[0].json(), json!({"extensions": persisted}));
    assert_eq!(
        hits[1].json(),
        json!({"query": document, "extensions": persisted})
    );

    let again = run(&f).await.unwrap();
    assert_eq!(again.requests, 1);
    assert_eq!(again.apq.unwrap().to_string(), "APQ hit");
    assert_eq!(server.hits().len(), 3);
}

#[tokio::test]
async fn apq_shows_no_outcome_for_an_answer_that_is_not_graphql() {
    let server =
        GraphqlHttpServer::start(|_| Reply::raw(502, "text/html", "<h1>Bad Gateway</h1>")).await;
    let f = Fixture::new(&server.url(""));
    f.request("document: '{ users { id } }'\napq: true\n");
    let result = run(&f).await.unwrap();
    assert!(result.response.is_none());
    assert_eq!(result.apq, None, "a 502 page is neither a hit nor a miss");
    assert_eq!(result.requests, 1);
}

#[tokio::test]
async fn apq_is_not_registered_when_the_retry_misses_again() {
    let server = GraphqlHttpServer::start(|_| {
        Reply::graphql(json!({"errors": [{"message": "PersistedQueryNotFound"}]}))
    })
    .await;
    let f = Fixture::new(&server.url(""));
    f.request("document: '{ users { id } }'\napq: true\n");
    let result = run(&f).await.unwrap();
    assert_eq!(result.requests, 2);
    assert_eq!(server.hits().len(), 2, "one retry only");
    assert_eq!(result.apq, Some(ApqOutcome::MissNotRegistered));
    assert_eq!(result.apq.unwrap().to_string(), "APQ miss (not registered)");
}

#[tokio::test]
async fn apq_unsupported_by_the_server_is_not_a_hit() {
    let server = GraphqlHttpServer::start(|hit| {
        if hit.json().get("query").is_none() {
            return Reply::graphql(json!({"errors": [{"message": "PersistedQueryNotSupported",
                "extensions": {"code": "PERSISTED_QUERY_NOT_SUPPORTED"}}]}));
        }
        Reply::graphql(json!({"data": {"users": []}}))
    })
    .await;
    let f = Fixture::new(&server.url(""));
    f.request("document: '{ users { id } }'\napq: true\n");
    let result = run(&f).await.unwrap();
    assert_eq!(result.requests, 2, "the full document follows");
    assert_eq!(result.apq, Some(ApqOutcome::MissNotRegistered));
    assert_eq!(result.response.unwrap().status, ResultStatus::Ok);
}

#[tokio::test]
async fn secrets_in_variables_and_headers_stay_redacted() {
    let server = GraphqlHttpServer::start(|hit| {
        let echo = hit.body_text();
        Reply::graphql(json!({"errors": [{"message": format!("bad input: {echo}")}]}))
    })
    .await;
    let f = Fixture::new(&server.url(""));
    f.request(
        "document: 'query($t: String) { a(t: $t) }'\nvariables:\n  t: '{{api_token}}'\n\
         headers:\n  - name: X-Token\n    value: '{{api_token}}'\n",
    );
    let result = run(&f).await.unwrap();
    assert!(server.hits()[0].body_text().contains(SECRET));
    let redactor = result.exchange.redactor(&result.request);
    let message = &result.response.as_ref().unwrap().errors[0].message;
    assert!(message.contains(SECRET), "the server echoed it");
    assert!(!redactor.text(message).contains(SECRET));
    assert!(!format!("{:?}", result.request).contains(SECRET));
}

/// Options whose in-memory body cap (`[http] max_body_bytes`) is `limit`.
fn capped(limit: u64) -> SendOptions {
    let mut options = SendOptions::default();
    options.body.max_in_memory = limit;
    options
}

#[tokio::test]
async fn a_result_over_max_body_bytes_is_refused() {
    let big = "y".repeat(64 * 1024);
    let server =
        GraphqlHttpServer::start(move |_| Reply::graphql(json!({"data": {"user": big}}))).await;
    let f = Fixture::new(&server.url(""));
    f.request("document: '{ user }'\napq: true\n");
    let err = run_with(&f, &capped(4096)).await.unwrap_err();
    assert_eq!(err, GraphqlError::BodyTooLarge(4096));
    assert_eq!(
        err.to_string(),
        "HTTP: the response body is larger than the 4096 byte limit (max_body_bytes)"
    );
    assert_eq!(server.hits().len(), 1, "an oversized answer is not resent");
}

#[tokio::test]
async fn a_result_of_exactly_max_body_bytes_is_read() {
    let reply = json!({"data": {"user": "z".repeat(1000)}}).to_string();
    let size = reply.len() as u64;
    let served = reply.clone();
    let server =
        GraphqlHttpServer::start(move |_| Reply::raw(200, "application/json", &served)).await;
    let f = Fixture::new(&server.url(""));
    f.request("document: '{ user }'\n");
    let result = run_with(&f, &capped(size)).await.unwrap();
    assert_eq!(result.exchange.body.as_ref(), reply.as_bytes());
    assert!(result.exchange.spill.is_none());
    let response = result.response.unwrap();
    assert_eq!(response.status, ResultStatus::Ok);
    let err = run_with(&f, &capped(size - 1)).await.unwrap_err();
    assert_eq!(err, GraphqlError::BodyTooLarge(size - 1));
}

/// A traced query POST carries `traceparent` (add-trace-propagation task
/// 2.3); an untraced one carries none.
#[tokio::test]
async fn a_traced_query_post_carries_a_traceparent() {
    let server = GraphqlHttpServer::start(|_| Reply::graphql(json!({"data": {"user": 1}}))).await;
    let f = Fixture::new(&server.url(""));
    f.request("document: '{ user }'\n");
    let options = SendOptions {
        trace: Some(rustman_core::trace::TracePropagation::default()),
        ..SendOptions::default()
    };
    run_with(&f, &options).await.unwrap();
    run(&f).await.unwrap();
    let hits = server.hits();
    assert_eq!(hits[0].method, "POST");
    let traceparent = hits[0].header("traceparent").expect("a traceparent");
    assert!(traceparent.starts_with("00-") && traceparent.ends_with("-01"));
    assert_eq!(hits[1].header("traceparent"), None);
}
