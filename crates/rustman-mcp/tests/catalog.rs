//! The tool, resource and prompt catalog (agent-mcp-interface tasks
//! 3.1-3.8), driven through real MCP traffic: a `RustmanServer` session on
//! one end of a pipe, an rmcp client on the other. Requests go only to the
//! in-process test servers.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, ElicitRequestParams,
    ElicitResult, ElicitationAction, GetPromptRequestParams, Implementation,
    ReadResourceRequestParams, ResourceContents,
};
use rmcp::service::{RequestContext, RoleClient, RunningService};
use rmcp::{ClientHandler, ErrorData, ServiceExt};
use rustman_core::secrets::{Keychain, KeychainError};
use rustman_core::{SecretStore, Workspace};
use rustman_mcp::server::{ErrorCode, Sessions};
use rustman_test_support::TestServer;
use rustman_test_support::bad_api::{self, BadApi};
use rustman_test_support::routes::timed_hits;
use serde_json::{Value, json};

const TOKEN: &str = "tok-5d0e9b13c47a8f62";

/// A keychain that knows one secret: `api_token`.
struct OneSecret;

impl Keychain for OneSecret {
    fn get(&self, account: &str) -> Result<Option<String>, KeychainError> {
        Ok(account.ends_with("/api_token").then(|| TOKEN.to_owned()))
    }
    fn set(&self, _: &str, _: &str) -> Result<(), KeychainError> {
        Ok(())
    }
    fn delete(&self, _: &str) -> Result<bool, KeychainError> {
        Ok(false)
    }
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A workspace whose `local` environment points at `base_url` and declares
/// the secret `api_token`; `extra` goes into `rustman.toml`.
fn workspace(base_url: &str, extra: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!(
        "[workspace]\nname = \"t\"\nversion = 1\n\n[safety]\nrate = 1000\nconcurrency = 4\n{extra}"
    );
    write(dir.path(), "rustman.toml", &toml);
    let env = format!("variables:\n  base_url: {base_url}\nsecrets:\n  - api_token\n");
    write(dir.path(), "environments/local.yaml", &env);
    write(
        dir.path(),
        "environments/staging.yaml",
        "variables:\n  base_url: http://127.0.0.1:9\n",
    );
    write(dir.path(), ".gitignore", ".rustman/\n");
    write(
        dir.path(),
        "users/get-user.rman.yaml",
        "version: 1\nkind: http\nname: Get user\nmethod: GET\nurl: \"{{base_url}}/users/{{id}}\"\nheaders:\n  - name: Authorization\n    value: \"Bearer {{api_token}}\"\nassertions:\n  - status: 200\n",
    );
    write(
        dir.path(),
        "users/echo.rman.yaml",
        "version: 1\nkind: http\nname: Echo\nmethod: GET\nurl: \"{{base_url}}/echo\"\nheaders:\n  - name: Authorization\n    value: \"Bearer {{api_token}}\"\n",
    );
    dir
}

fn sessions(root: &Path) -> Sessions {
    rustman_core::secrets::workspace_id::load_or_create(root).unwrap();
    let ws = Workspace::load(root).unwrap();
    let local = ws.environments.iter().find(|e| e.name == "local").cloned();
    let secrets = SecretStore::with_keychain(root, Some(Box::new(OneSecret)));
    Sessions::new(ws, secrets, local)
}

/// The client: `answer` is the human's answer to every confirmation, or
/// `None` for a client without elicitation.
#[derive(Clone, Default)]
struct Agent {
    answer: Option<ElicitationAction>,
}

impl ClientHandler for Agent {
    fn get_info(&self) -> ClientConfig {
        let capabilities = match self.answer {
            Some(_) => ClientCapabilities::builder().enable_elicitation().build(),
            None => ClientCapabilities::default(),
        };
        ClientConfig::new(capabilities, Implementation::new("catalog-agent", "1.0"))
    }

    async fn create_elicitation(
        &self,
        _: ElicitRequestParams,
        _: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, ErrorData> {
        let action = self.answer.clone().unwrap_or(ElicitationAction::Decline);
        Ok(ElicitResult::new(action).with_content(json!({"confirm": true})))
    }
}

type Client = RunningService<RoleClient, Agent>;

async fn connect_with(root: &Path, agent: Agent) -> Client {
    let server = sessions(root).open();
    let (server_io, client_io) = tokio::io::duplex(1 << 22);
    tokio::spawn(async move {
        let service = server.serve(server_io).await.unwrap();
        let _ = service.waiting().await;
    });
    agent.serve(client_io).await.unwrap()
}

async fn connect(root: &Path) -> Client {
    connect_with(root, Agent::default()).await
}

async fn call(client: &Client, tool: &str, arguments: Value) -> CallToolResult {
    let params = CallToolRequestParams::new(tool.to_owned())
        .with_arguments(arguments.as_object().unwrap().clone());
    client.call_tool(params).await.unwrap()
}

/// The structured result of a call that must succeed.
async fn ok(client: &Client, tool: &str, arguments: Value) -> Value {
    let result = call(client, tool, arguments).await;
    assert_eq!(result.is_error, Some(false), "{tool}: {result:?}");
    result.structured_content.unwrap()
}

fn code(result: &CallToolResult) -> String {
    assert_eq!(result.is_error, Some(true), "{result:?}");
    let structured = result.structured_content.as_ref().unwrap();
    structured["code"].as_str().unwrap().to_owned()
}

fn message(result: &CallToolResult) -> String {
    let structured = result.structured_content.as_ref().unwrap();
    structured["message"].as_str().unwrap().to_owned()
}

/// Every tool's output schema, by name.
async fn output_schemas(client: &Client) -> BTreeMap<String, Value> {
    let tools = client.list_all_tools().await.unwrap();
    tools
        .into_iter()
        .map(|t| {
            let schema = Value::Object((*t.output_schema.expect("an output schema")).clone());
            (t.name.to_string(), schema)
        })
        .collect()
}

/// Panic unless `instance` validates against `schema`.
fn conforms(tool: &str, schema: &Value, instance: &Value) {
    let validator = jsonschema::validator_for(schema).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{tool}: {errors:?}\n{instance:#}");
}

/// Call `tool` and check its `structuredContent` against its schema.
async fn checked(
    client: &Client,
    schemas: &BTreeMap<String, Value>,
    tool: &str,
    args: Value,
) -> Value {
    let structured = ok(client, tool, args).await;
    conforms(tool, &schemas[tool], &structured);
    structured
}

/// Every file of the workspace outside `.rustman/`, with its text.
fn snapshot_files(root: &Path) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.ends_with(".rustman") {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                files.insert(rel, std::fs::read_to_string(&path).unwrap_or_default());
            }
        }
    }
    files
}

const TOOLS: [&str; 18] = [
    "list_requests",
    "get_request",
    "list_environments",
    "select_environment",
    "send_request",
    "run_collection",
    "import_openapi",
    "explore_plan",
    "explore_run",
    "explore_status",
    "explore_cancel",
    "get_report",
    "list_findings",
    "search_history",
    // jsonrpc-requests: registered with the protocol tools.
    "rpc_call",
    // grpc-requests: registered with the `grpc` feature (the default).
    "grpc_list",
    "grpc_describe",
    "grpc_call",
];

#[tokio::test(flavor = "multi_thread")]
async fn tools_list_names_the_catalog_with_schemas() {
    let ws = workspace("http://127.0.0.1:9", "");
    let client = connect(ws.path()).await;
    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(names, TOOLS);
    for tool in &tools {
        assert_eq!(tool.input_schema["type"], "object", "{}", tool.name);
        let output = tool.output_schema.as_ref().expect("an output schema");
        assert_eq!(output["type"], "object", "{}", tool.name);
        assert!(tool.description.is_some(), "{}", tool.name);
    }
    // This build has neither graphql-requests nor webhook-inspector.
    for absent in [
        "graphql_introspect",
        "graphql_query",
        "list_webhook_captures",
    ] {
        assert!(!names.contains(&absent), "{absent}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_tools_conform_to_their_schemas() {
    let ws = workspace("http://127.0.0.1:9", "");
    let client = connect(ws.path()).await;
    let schemas = output_schemas(&client).await;
    let list = checked(&client, &schemas, "list_requests", json!({})).await;
    assert_eq!(list["total"], 2);
    assert_eq!(list["items"][0]["id"], "users/echo");
    let filtered = checked(
        &client,
        &schemas,
        "list_requests",
        json!({"text": "get user"}),
    )
    .await;
    assert_eq!(filtered["items"].as_array().unwrap().len(), 1);
    let request = checked(
        &client,
        &schemas,
        "get_request",
        json!({"path": "users/get-user"}),
    )
    .await;
    assert_eq!(request["method"], "GET");
    assert!(request["text"].as_str().unwrap().contains("{{api_token}}"));
    let by_file = ok(
        &client,
        "get_request",
        json!({"path": "users/get-user.rman.yaml"}),
    )
    .await;
    assert_eq!(by_file["id"], "users/get-user");
    let envs = checked(&client, &schemas, "list_environments", json!({})).await;
    assert_eq!(envs["environments"][0]["name"], "local");
    assert_eq!(envs["environments"][0]["active"], true);
    assert_eq!(envs["environments"][0]["secrets"], json!(["api_token"]));
    checked(
        &client,
        &schemas,
        "select_environment",
        json!({"name": "staging"}),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn select_environment_is_session_scoped_and_never_writes() {
    let ws = workspace("http://127.0.0.1:9", "");
    let before = snapshot_files(ws.path());
    let client = connect(ws.path()).await;
    ok(&client, "select_environment", json!({"name": "staging"})).await;
    let envs = ok(&client, "list_environments", json!({})).await;
    let active: Vec<&str> = envs["environments"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["active"] == true)
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(active, ["staging"]);
    assert_eq!(snapshot_files(ws.path()), before);
    // Another session still starts in `local`.
    let other = connect(ws.path()).await;
    let envs = ok(&other, "list_environments", json!({})).await;
    assert_eq!(envs["environments"][0]["active"], true);
    let missing = call(&client, "select_environment", json!({"name": "prod"})).await;
    assert_eq!(code(&missing), "not_found");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_request_is_not_found() {
    let ws = workspace("http://127.0.0.1:9", "");
    let client = connect(ws.path()).await;
    let result = call(&client, "get_request", json!({"path": "users/nope"})).await;
    assert_eq!(code(&result), "not_found");
}

#[tokio::test(flavor = "multi_thread")]
async fn one_hundred_twenty_requests_page_as_50_50_and_20() {
    let ws = workspace("http://127.0.0.1:9", "");
    for i in 0..118 {
        write(
            ws.path(),
            &format!("bulk/r{i:03}.rman.yaml"),
            &format!("version: 1\nkind: http\nname: r{i:03}\nurl: http://127.0.0.1:9/{i}\n"),
        );
    }
    let client = connect(ws.path()).await;
    let mut seen = Vec::new();
    let mut sizes = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let args = match &cursor {
            Some(c) => json!({"cursor": c}),
            None => json!({}),
        };
        let page = ok(&client, "list_requests", args).await;
        let items = page["items"].as_array().unwrap();
        sizes.push(items.len());
        seen.extend(items.iter().map(|i| i["id"].as_str().unwrap().to_owned()));
        match page["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    assert_eq!(sizes, [50, 50, 20]);
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 120);
}

#[tokio::test(flavor = "multi_thread")]
async fn send_request_sends_a_saved_request_with_vars() {
    let server = TestServer::http().await;
    let ws = workspace(&server.url(""), "");
    let client = connect(ws.path()).await;
    let schemas = output_schemas(&client).await;
    let args = json!({"path": "users/get-user.rman.yaml", "vars": {"id": "42"}});
    let result = checked(&client, &schemas, "send_request", args).await;
    assert_eq!(result["status"], 200);
    assert_eq!(result["outcome"], "passed");
    assert!(result["timings"]["total_ms"].as_f64().unwrap() >= 0.0);
    assert!(!result["headers"].as_array().unwrap().is_empty());
    assert_eq!(result["assertions"][0]["passed"], true);
    assert!(result["body"]["text"].as_str().unwrap().contains("42"));
    assert_eq!(result["url"], server.url("/users/42"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_echoed_secret_never_reaches_the_agent() {
    let server = TestServer::http().await;
    let ws = workspace(&server.url(""), "");
    let client = connect(ws.path()).await;
    let result = call(&client, "send_request", json!({"path": "users/echo"})).await;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let text = serde_json::to_string(&result).unwrap();
    assert!(!text.contains(TOKEN), "{text}");
    let body = result.structured_content.unwrap()["body"]["text"].clone();
    assert!(body.as_str().unwrap().contains("«redacted:"), "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn agents_never_write_snapshot_files() {
    let server = TestServer::http().await;
    let ws = workspace(&server.url(""), "");
    write(
        ws.path(),
        "users/snap.rman.yaml",
        "version: 1\nkind: http\nname: Snap\nmethod: GET\nurl: \"{{base_url}}/users/1\"\nassertions:\n  - snapshot\n",
    );
    let client = connect(ws.path()).await;
    let sent = ok(&client, "send_request", json!({"path": "users/snap"})).await;
    assert_eq!(sent["outcome"], "failed", "{sent}");
    let run = ok(
        &client,
        "run_collection",
        json!({"path": "users/snap.rman.yaml"}),
    )
    .await;
    let report = finished_run(&client, run["job_id"].as_str().unwrap()).await;
    assert_eq!(report["summary"]["summary"]["failed"], 1, "{report:#}");
    assert!(!ws.path().join("users/__snapshots__").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn digest_auth_is_refused_rather_than_sent_without_credentials() {
    let server = TestServer::http().await;
    let ws = workspace(&server.url(""), "");
    write(
        ws.path(),
        "users/digest.rman.yaml",
        "version: 1\nkind: http\nname: Digest\nmethod: GET\nurl: \"{{base_url}}/timed/0?id=agent-digest\"\nauth:\n  type: digest\n  username: alice\n  password: \"{{api_token}}\"\n",
    );
    let client = connect(ws.path()).await;
    let result = ok(&client, "send_request", json!({"path": "users/digest"})).await;
    assert_eq!(result["outcome"], "errored", "{result}");
    let error = result["error"].as_str().unwrap();
    assert!(error.contains("digest"), "{error}");
    assert!(timed_hits("agent-digest").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn ad_hoc_requests_pass_the_safety_policy() {
    let server = TestServer::http().await;
    let ws = workspace(&server.url(""), "");
    let client = connect(ws.path()).await;
    let blocked = call(
        &client,
        "send_request",
        json!({"url": "https://example.org/"}),
    )
    .await;
    assert_eq!(code(&blocked), "blocked");
    assert_eq!(message(&blocked), "host not allowed");
    let post = json!({"method": "POST", "url": server.url("/echo"), "body": "{}"});
    let refused = call(&client, "send_request", post).await;
    assert_eq!(code(&refused), "blocked");
    assert_eq!(message(&refused), "mutations not allowed for agents");
    let get = json!({"url": "{{base_url}}/status/503", "headers": {"Accept": "text/plain"}});
    let sent = ok(&client, "send_request", get).await;
    assert_eq!(sent["status"], 503);
    assert_eq!(sent["request"], "ad-hoc");
    let neither = call(&client, "send_request", json!({})).await;
    assert_eq!(code(&neither), "invalid_input");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_large_body_is_truncated_with_a_readable_ref() {
    let server = TestServer::http().await;
    let ws = workspace(&server.url(""), "");
    let client = connect(ws.path()).await;
    let result = ok(
        &client,
        "send_request",
        json!({"url": server.url("/bytes/2097152")}),
    )
    .await;
    let body = &result["body"];
    assert_eq!(body["truncated"], true);
    assert_eq!(body["size"], 2_097_152);
    let body_ref = body["body_ref"].as_str().unwrap();
    let read = client
        .read_resource(ReadResourceRequestParams::new(body_ref))
        .await
        .unwrap();
    let len = match &read.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => text.len(),
        ResourceContents::BlobResourceContents { blob, .. } => blob.len() * 3 / 4,
        _ => 0,
    };
    assert!(len >= 2_097_152 - 4, "{len}");
}

/// Poll `get_report` for a run job until it is finished.
async fn finished_run(client: &Client, job: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let result = call(client, "get_report", json!({"id": job})).await;
        if result.is_error == Some(false) {
            return result.structured_content.unwrap();
        }
        assert!(message(&result).contains("still running"), "{result:?}");
        assert!(Instant::now() < deadline, "the run did not finish");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn run_collection_runs_in_the_background_and_reports() {
    let server = TestServer::http().await;
    let ws = workspace(&server.url(""), "");
    let client = connect(ws.path()).await;
    let schemas = output_schemas(&client).await;
    let started = checked(
        &client,
        &schemas,
        "run_collection",
        json!({"path": "users", "vars": {"id": "42"}}),
    )
    .await;
    assert_eq!(started["requests"], 2);
    let job = started["job_id"].as_str().unwrap();
    let report = finished_run(&client, job).await;
    conforms("get_report", &schemas["get_report"], &report);
    assert_eq!(report["kind"], "run");
    assert_eq!(report["summary"]["summary"]["passed"], 2, "{report:#}");
    let junit = ok(&client, "get_report", json!({"id": job, "format": "junit"})).await;
    assert!(junit["text"].as_str().unwrap().contains("<testsuite"));
    let text = serde_json::to_string(&junit).unwrap();
    assert!(!text.contains(TOKEN));
    // The report resource holds the echoed token redacted.
    let uri = format!("rustman://reports/{job}");
    let read = client
        .read_resource(ReadResourceRequestParams::new(uri))
        .await
        .unwrap();
    let ResourceContents::TextResourceContents { text, .. } = &read.contents[0] else {
        panic!("a text resource");
    };
    assert!(!text.contains(TOKEN), "{text}");
    assert!(text.contains("«redacted:"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn run_collection_against_a_blocked_host_reports_blocked() {
    let ws = workspace("http://127.0.0.1:9", "");
    write(
        ws.path(),
        "outside/get.rman.yaml",
        "version: 1\nkind: http\nname: outside\nurl: https://example.org/\n",
    );
    let client = connect(ws.path()).await;
    let started = ok(&client, "run_collection", json!({"path": "outside"})).await;
    let report = finished_run(&client, started["job_id"].as_str().unwrap()).await;
    let problems = &report["summary"]["not_passed"];
    assert_eq!(problems[0]["status"], "errored");
    assert_eq!(problems[0]["error"], "blocked: host not allowed");
    let escape = call(&client, "run_collection", json!({"path": "../x"})).await;
    assert_eq!(code(&escape), "invalid_input");
}

/// A workspace for `bad-api`, its OpenAPI document at `api/openapi.yaml`.
fn bad_api_workspace(api: &BadApi, extra: &str) -> tempfile::TempDir {
    let ws = workspace(&api.url(""), extra);
    write(ws.path(), "api/openapi.yaml", bad_api::OPENAPI);
    ws
}

#[tokio::test(flavor = "multi_thread")]
async fn explore_plan_sends_nothing() {
    let api = BadApi::start().await;
    let ws = bad_api_workspace(&api, "");
    let client = connect(ws.path()).await;
    let schemas = output_schemas(&client).await;
    let args = json!({"spec": "api/openapi.yaml", "target": api.url("")});
    let plan = checked(&client, &schemas, "explore_plan", args).await;
    assert!(plan["total"].as_u64().unwrap() > 0);
    assert!(plan["allowed"].as_u64().unwrap() > 0);
    // Read-only by default: mutating probes are planned as blocked.
    assert!(plan["blocked"].as_u64().unwrap() > 0);
    assert!(api.hits().is_empty(), "{:?}", api.hits());
    let other = json!({"spec": "api/openapi.yaml", "target": "https://example.org"});
    let blocked = call(&client, "explore_plan", other).await;
    assert_eq!(code(&blocked), "blocked");
    let mutations =
        json!({"spec": "api/openapi.yaml", "target": api.url(""), "allow_mutations": true});
    let refused = call(&client, "explore_plan", mutations).await;
    assert_eq!(message(&refused), "mutations not allowed for agents");
}

/// Poll `explore_status` until the job is no longer running.
async fn finished_exploration(client: &Client, job: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let status = ok(client, "explore_status", json!({"id": job})).await;
        if status["state"] != "running" {
            return status;
        }
        assert!(Instant::now() < deadline, "the exploration did not finish");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_exploration_runs_and_its_findings_filter_by_severity() {
    let api = BadApi::start().await;
    let ws = bad_api_workspace(&api, "");
    let client = connect(ws.path()).await;
    let schemas = output_schemas(&client).await;
    let args = json!({"spec": "api/openapi.yaml", "target": api.url("")});
    let started = checked(&client, &schemas, "explore_run", args).await;
    let job = started["job_id"].as_str().unwrap();
    let status = finished_exploration(&client, job).await;
    conforms("explore_status", &schemas["explore_status"], &status);
    assert_eq!(status["state"], "completed", "{status:#}");
    let id = started["exploration_id"].as_str().unwrap();
    assert_eq!(status["report"], format!("rustman://reports/{id}"));
    // Read-only: no mutating request reached the server.
    assert!(
        api.hits()
            .iter()
            .all(|h| ["GET", "HEAD", "OPTIONS"].contains(&h.method.as_str()))
    );
    let all = checked(
        &client,
        &schemas,
        "list_findings",
        json!({"exploration_id": id}),
    )
    .await;
    let high = checked(
        &client,
        &schemas,
        "list_findings",
        json!({"severity": "high"}),
    )
    .await;
    let severities = |list: &Value| -> Vec<String> {
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["severity"].as_str().unwrap().to_owned())
            .collect()
    };
    assert!(
        severities(&high)
            .iter()
            .all(|s| s == "critical" || s == "high")
    );
    assert!(!severities(&high).is_empty());
    assert!(severities(&all).len() > severities(&high).len());
    let by_operation = ok(&client, "list_findings", json!({"operation": "/search"})).await;
    for finding in by_operation["items"].as_array().unwrap() {
        assert!(finding["operation"].as_str().unwrap().contains("/search"));
    }
    let report = checked(&client, &schemas, "get_report", json!({"id": id})).await;
    assert_eq!(report["kind"], "exploration");
    let markdown = ok(
        &client,
        "get_report",
        json!({"id": job, "format": "markdown"}),
    )
    .await;
    assert!(!markdown["text"].as_str().unwrap().is_empty());
    let bad = call(&client, "list_findings", json!({"severity": "dire"})).await;
    assert_eq!(code(&bad), "invalid_input");
}

#[tokio::test(flavor = "multi_thread")]
async fn import_openapi_dry_runs_by_default_and_writes_only_when_confirmed() {
    let api = BadApi::start().await;
    let ws = bad_api_workspace(&api, "");
    let before = snapshot_files(ws.path());
    let client = connect(ws.path()).await;
    let schemas = output_schemas(&client).await;
    let dry = checked(
        &client,
        &schemas,
        "import_openapi",
        json!({"spec": "api/openapi.yaml", "into": "bad"}),
    )
    .await;
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["mode"], "create");
    assert!(dry["requests"].as_u64().unwrap() > 0);
    assert_eq!(snapshot_files(ws.path()), before);
    let write = json!({"spec": "api/openapi.yaml", "into": "bad", "dry_run": false});
    let unconfirmed = call(&client, "import_openapi", write.clone()).await;
    assert_eq!(code(&unconfirmed), "confirmation_unavailable");
    let human = connect_with(
        ws.path(),
        Agent {
            answer: Some(ElicitationAction::Accept),
        },
    )
    .await;
    let written = ok(&human, "import_openapi", write).await;
    assert_eq!(written["dry_run"], false);
    assert!(ws.path().join("bad").is_dir());
    let outside = call(
        &client,
        "import_openapi",
        json!({"spec": "../openapi.yaml"}),
    )
    .await;
    assert_eq!(code(&outside), "invalid_input");
    assert!(api.hits().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn search_history_filters_and_pages_newest_first() {
    let server = TestServer::http().await;
    let ws = workspace(&server.url(""), "");
    let client = connect(ws.path()).await;
    for path in [
        "/status/500",
        "/status/200",
        "/status/503",
        "/status/404",
        "/status/502",
    ] {
        ok(&client, "send_request", json!({"url": server.url(path)})).await;
    }
    let schemas = output_schemas(&client).await;
    let failures = checked(
        &client,
        &schemas,
        "search_history",
        json!({"status": "500-599", "limit": 2}),
    )
    .await;
    let statuses: Vec<u64> = failures["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["status"].as_u64().unwrap())
        .collect();
    assert_eq!(statuses, [502, 503]);
    assert_eq!(failures["total"], 3);
    let cursor = failures["next_cursor"].as_str().unwrap();
    let rest = ok(
        &client,
        "search_history",
        json!({"status": "5xx", "limit": 2, "cursor": cursor}),
    )
    .await;
    assert_eq!(rest["items"][0]["status"], 500);
    assert!(rest.get("next_cursor").is_none());
    let by_url = ok(
        &client,
        "search_history",
        json!({"url": "/status/404", "method": "get", "since": "1h"}),
    )
    .await;
    assert_eq!(by_url["total"], 1);
    let future = ok(
        &client,
        "search_history",
        json!({"since": "2999-01-01T00:00:00Z"}),
    )
    .await;
    assert_eq!(future["total"], 0);
    let until = ok(
        &client,
        "search_history",
        json!({"until": "2000-01-01T00:00:00Z"}),
    )
    .await;
    assert_eq!(until["total"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn resources_list_read_and_refuse_traversal() {
    let ws = workspace("http://127.0.0.1:9", "");
    let client = connect(ws.path()).await;
    let resources = client.list_all_resources().await.unwrap();
    let uris: Vec<&str> = resources.iter().map(|r| r.uri.as_str()).collect();
    assert!(
        uris.contains(&"rustman://requests/users/get-user.rman.yaml"),
        "{uris:?}"
    );
    assert!(uris.contains(&"rustman://schemas/report"), "{uris:?}");
    let templates = client.list_all_resource_templates().await.unwrap();
    assert_eq!(templates.len(), 4);
    let read = client
        .read_resource(ReadResourceRequestParams::new(
            "rustman://requests/users/get-user.rman.yaml",
        ))
        .await
        .unwrap();
    let ResourceContents::TextResourceContents { text, .. } = &read.contents[0] else {
        panic!("a text resource");
    };
    assert!(text.contains("{{api_token}}"));
    let schema = client
        .read_resource(ReadResourceRequestParams::new("rustman://schemas/report"))
        .await
        .unwrap();
    assert!(matches!(
        &schema.contents[0],
        ResourceContents::TextResourceContents { .. }
    ));
    for uri in [
        "rustman://requests/../../etc/passwd",
        "rustman://requests/users/../../rustman.toml",
        "rustman://schemas/../rustman.toml",
        "rustman://reports/../../etc/passwd",
        "rustman://bodies/..",
    ] {
        let err = client
            .read_resource(ReadResourceRequestParams::new(uri))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("not a path inside the workspace"),
            "{uri}: {err}"
        );
    }
    let missing = client
        .read_resource(ReadResourceRequestParams::new(
            "rustman://requests/rustman.toml",
        ))
        .await;
    assert!(missing.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_explore_prompt_plans_before_running() {
    let ws = workspace("http://127.0.0.1:9", "");
    let client = connect(ws.path()).await;
    let prompts = client.list_all_prompts().await.unwrap();
    let names: Vec<&str> = prompts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["explore-api", "triage-findings"]);
    let args = json!({"spec": "api/openapi.yaml", "target": "http://127.0.0.1:9"});
    let params = GetPromptRequestParams::new("explore-api")
        .with_arguments(args.as_object().unwrap().clone());
    let prompt = client.get_prompt(params).await.unwrap();
    let text = serde_json::to_value(&prompt.messages[0].content).unwrap()["text"]
        .as_str()
        .unwrap()
        .to_owned();
    let plan = text.find("explore_plan").unwrap();
    assert!(plan < text.find("explore_run").unwrap(), "{text}");
    let args = json!({"exploration_id": "exp-20261118-abcdef"});
    let params = GetPromptRequestParams::new("triage-findings")
        .with_arguments(args.as_object().unwrap().clone());
    let triage = client.get_prompt(params).await.unwrap();
    let text = serde_json::to_string(&triage.messages).unwrap();
    assert!(text.contains("list_findings") && text.contains("exp-20261118-abcdef"));
}

#[tokio::test(flavor = "multi_thread")]
async fn every_tool_error_uses_a_catalogued_code() {
    let ws = workspace("http://127.0.0.1:9", "");
    let client = connect(ws.path()).await;
    let catalogued: Vec<&str> = ErrorCode::ALL.iter().map(|c| c.as_str()).collect();
    let bad_calls = [
        ("list_requests", json!({"cursor": "nope"})),
        ("list_requests", json!({"unknown": 1})),
        ("get_request", json!({})),
        ("get_request", json!({"path": "missing"})),
        ("list_environments", json!({"x": 1})),
        ("select_environment", json!({"name": "missing"})),
        ("send_request", json!({"path": "missing"})),
        ("send_request", json!({"url": "https://example.org"})),
        ("run_collection", json!({"path": "missing"})),
        ("import_openapi", json!({"spec": "missing.yaml"})),
        (
            "explore_plan",
            json!({"spec": "missing.yaml", "target": "http://127.0.0.1:9"}),
        ),
        (
            "explore_run",
            json!({"spec": "missing.yaml", "target": "http://127.0.0.1:9"}),
        ),
        ("explore_status", json!({"id": "exp-missing"})),
        ("explore_cancel", json!({"job_id": "job-9"})),
        ("get_report", json!({})),
        ("list_findings", json!({"exploration_id": "exp-missing"})),
        ("search_history", json!({"status": "teapot"})),
        ("rpc_call", json!({"path": "missing"})),
        (
            "rpc_call",
            json!({"endpoint": "http://127.0.0.1:9", "method": "m"}),
        ),
        ("grpc_list", json!({})),
        ("grpc_list", json!({"target": "grpc://blocked.example:1"})),
        ("grpc_describe", json!({"path": "missing", "symbol": "a.B"})),
        ("grpc_call", json!({"path": "missing"})),
        (
            "grpc_call",
            json!({"target": "grpc://blocked.example:1", "method": "a.B/C"}),
        ),
    ];
    let mut tools_seen: Vec<&str> = Vec::new();
    for (tool, args) in bad_calls {
        let result = call(&client, tool, args.clone()).await;
        let code = code(&result);
        assert!(catalogued.contains(&code.as_str()), "{tool} {args}: {code}");
        tools_seen.push(tool);
    }
    tools_seen.dedup();
    assert_eq!(tools_seen, TOOLS);
    let unknown = client
        .call_tool(CallToolRequestParams::new("drop_database"))
        .await;
    assert!(unknown.is_err());
}

/// Every line of the workspace's audit log.
fn audit_lines(root: &Path) -> Vec<Value> {
    let dir = root.join(rustman_mcp::server::audit::AUDIT_DIR);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for entry in entries {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        lines.extend(
            text.lines()
                .map(|l| serde_json::from_str::<Value>(l).unwrap()),
        );
    }
    lines
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spec_url_redirected_off_the_allowlist_is_blocked() {
    let allowed = TestServer::http().await;
    let elsewhere = BadApi::start().await;
    let ws = workspace(
        &allowed.url(""),
        &format!("\n[agent]\nallowed_hosts = [\"{}\"]\n", allowed.url("")),
    );
    let client = connect(ws.path()).await;
    let spec = allowed.url(&format!(
        "/redirect/302?to={}",
        elsewhere.url("/openapi.json")
    ));
    let calls = [
        ("import_openapi", json!({"spec": spec})),
        (
            "explore_plan",
            json!({"spec": spec, "target": allowed.url("")}),
        ),
        (
            "explore_run",
            json!({"spec": spec, "target": allowed.url("")}),
        ),
    ];
    for (tool, args) in calls {
        let result = call(&client, tool, args).await;
        assert_eq!(code(&result), "blocked", "{tool}: {result:?}");
        assert_eq!(message(&result), "host not allowed", "{tool}");
    }
    assert!(
        elsewhere.hits().is_empty(),
        "{} hits",
        elsewhere.hits().len()
    );
    let lines = audit_lines(ws.path());
    assert_eq!(lines.len(), 3, "{lines:#?}");
    for line in &lines {
        assert_eq!(line["outcome"], "blocked", "{line}");
        assert_eq!(line["method"], "GET", "{line}");
        assert_eq!(line["url"], spec.as_str(), "{line}");
    }
    // An allowed URL is still read, and audited with its URL.
    let direct = allowed.url("/openapi/petstore.yaml");
    ok(&client, "import_openapi", json!({"spec": direct})).await;
    let last = audit_lines(ws.path()).pop().unwrap();
    assert_eq!(last["outcome"], "ok");
    assert_eq!(last["url"], direct.as_str());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_tool_is_audited() {
    let ws = workspace("http://127.0.0.1:9", "");
    let client = connect(ws.path()).await;
    let unknown = client
        .call_tool(CallToolRequestParams::new("drop_database"))
        .await;
    assert!(unknown.is_err());
    let lines = audit_lines(ws.path());
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert_eq!(lines[0]["tool"], "drop_database");
    assert_eq!(lines[0]["outcome"], "error");
    assert_eq!(lines[0]["reason"], "no tool 'drop_database'");
}

#[tokio::test(flavor = "multi_thread")]
async fn every_request_of_a_run_job_is_audited() {
    let server = TestServer::http().await;
    let ws = workspace(&server.url(""), "");
    write(
        ws.path(),
        "users/outside.rman.yaml",
        "version: 1\nkind: http\nname: outside\nurl: https://example.org/\n",
    );
    let client = connect(ws.path()).await;
    let args = json!({"path": "users", "vars": {"id": "42"}});
    let started = ok(&client, "run_collection", args).await;
    let job = started["job_id"].as_str().unwrap();
    finished_run(&client, job).await;
    let lines = audit_lines(ws.path());
    let requests: Vec<&Value> = lines
        .iter()
        .filter(|l| l["tool"] == "run_collection" && l["arguments"]["job_id"] == job)
        .collect();
    assert_eq!(requests.len(), 3, "{lines:#?}");
    assert!(requests.iter().all(|l| l["method"] == "GET"));
    let blocked: Vec<&&Value> = requests
        .iter()
        .filter(|l| l["outcome"] == "blocked")
        .collect();
    assert_eq!(blocked.len(), 1, "{requests:#?}");
    assert_eq!(blocked[0]["url"], "https://example.org/");
    assert_eq!(blocked[0]["reason"], "host not allowed");
    let echo = server.url("/echo");
    assert!(
        requests
            .iter()
            .any(|l| l["url"] == echo.as_str() && l["outcome"] == "ok")
    );
    let text = serde_json::to_string(&lines).unwrap();
    assert!(!text.contains(TOKEN), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_requests_are_skipped_and_spawn_nothing() {
    let ws = workspace("http://127.0.0.1:9", "");
    let marker = ws.path().join("spawned");
    let fixture = env!("CARGO_BIN_EXE_rustman-mcp-fixture");
    let server = format!(
        "\n[mcp_servers.local]\ncommand = {:?}\nargs = [\"--marker\", {:?}]\n",
        fixture,
        marker.to_str().unwrap()
    );
    let toml = std::fs::read_to_string(ws.path().join("rustman.toml")).unwrap();
    write(ws.path(), "rustman.toml", &format!("{toml}{server}"));
    write(
        ws.path(),
        "tools.rman.yaml",
        "version: 1\nkind: mcp\nname: Tools\nserver: local\noperation: tools/list\n",
    );
    let client = connect(ws.path()).await;
    let result = ok(&client, "send_request", json!({"path": "tools"})).await;
    assert_eq!(result["outcome"], "skipped", "{result}");
    assert!(!marker.exists(), "the agent spawned a workspace command");
}
