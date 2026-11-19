//! Secret canary over every output path of the agent server: each tool of
//! the catalog, each resource (listed and templated), each prompt and the
//! progress notifications. The canary is a secret of the active environment
//! that the test server echoes back and that a tracked request file leaks;
//! it must never reach the client. A tool added to the catalog fails this
//! test until it gets a call here. Requests go only to the in-process test
//! servers.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig,
    GetPromptRequestParams, Implementation, ProgressNotificationParam, ReadResourceRequestParams,
};
use rmcp::service::{NotificationContext, RoleClient, RunningService};
use rmcp::{ClientHandler, ServiceExt};
use rustman_core::secrets::{Keychain, KeychainError};
use rustman_core::{SecretStore, Workspace};
use rustman_mcp::server::Sessions;
use rustman_test_support::TestServer;
use rustman_test_support::bad_api::{self, BadApi};
use rustman_test_support::grpc::{GrpcServer, Reflection};
use serde_json::{Value, json};

const CANARY: &str = "canary-3f9e1b7d52c04a68";

/// The canary as it may appear in an output: plain, base64 (standard and
/// URL-safe). Its URL-encoded form is the plain one: every character is
/// unreserved.
fn canary_forms() -> Vec<String> {
    use base64::Engine as _;
    use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
    vec![
        CANARY.to_owned(),
        STANDARD.encode(CANARY),
        URL_SAFE_NO_PAD.encode(CANARY),
    ]
}

/// The first form of the canary in `text`, if any.
fn leaked_form(text: &str) -> Option<String> {
    canary_forms().into_iter().find(|form| text.contains(form))
}

struct OneSecret;

impl Keychain for OneSecret {
    fn get(&self, account: &str) -> Result<Option<String>, KeychainError> {
        Ok(account.ends_with("/api_token").then(|| CANARY.to_owned()))
    }
    fn set(&self, _: &str, _: &str) -> Result<(), KeychainError> {
        Ok(())
    }
    fn delete(&self, _: &str) -> Result<bool, KeychainError> {
        Ok(false)
    }
}

/// Every notification the client received, serialized.
#[derive(Clone, Default)]
struct Agent {
    seen: Arc<Mutex<Vec<String>>>,
}

impl ClientHandler for Agent {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("canary-agent", "1.0"),
        )
    }

    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _: NotificationContext<RoleClient>,
    ) {
        let text = serde_json::to_string(&params).unwrap();
        self.seen.lock().unwrap().push(text);
    }
}

type Client = RunningService<RoleClient, Agent>;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A workspace whose `local` environment reaches `server` and `api` and
/// holds the canary as the secret `api_token`. `users/leak` has the
/// canary's value pasted into a tracked file.
fn workspace(server: &TestServer, api: &BadApi, grpc: &GrpcServer) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n\n[safety]\nrate = 1000\nconcurrency = 4\n\n[jsonrpc]\nagent_read_only_methods = [\"echo\"]\n",
    );
    let env = format!(
        "variables:\n  base_url: {}\n  api_url: {}\n  grpc_host: '{}'\nsecrets:\n  - api_token\n",
        server.url(""),
        api.url(""),
        grpc.target()
    );
    write(dir.path(), "environments/local.yaml", &env);
    write(dir.path(), ".gitignore", ".rustman/\n");
    write(
        dir.path(),
        "users/echo.rman.yaml",
        "version: 1\nkind: http\nname: Echo\nmethod: GET\nurl: \"{{base_url}}/echo\"\nheaders:\n  - name: Authorization\n    value: \"Bearer {{api_token}}\"\n",
    );
    let leak = format!(
        "version: 1\nkind: http\nname: Leak\nmethod: GET\nurl: \"{{{{base_url}}}}/echo?k={CANARY}\"\nheaders:\n  - name: X-Pasted\n    value: \"{CANARY}\"\n  - name: X-Pasted-B64\n    value: \"{}\"\n",
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, CANARY)
    );
    write(dir.path(), "users/leak.rman.yaml", &leak);
    write(
        dir.path(),
        "grpc/get.rman.yaml",
        "version: 1\nkind: grpc\nname: Get\ntarget: '{{grpc_host}}'\nmethod: shop.v1.Catalog/GetProduct\nmessage: {id: '{{api_token}}'}\nmetadata:\n  - name: authorization\n    value: 'Bearer {{api_token}}'\n",
    );
    write(dir.path(), "api/openapi.yaml", bad_api::OPENAPI);
    dir
}

async fn connect(root: &Path, agent: Agent) -> Client {
    rustman_core::secrets::workspace_id::load_or_create(root).unwrap();
    let ws = Workspace::load(root).unwrap();
    let local = ws.environments.iter().find(|e| e.name == "local").cloned();
    let secrets = SecretStore::with_keychain(root, Some(Box::new(OneSecret)));
    let server = Sessions::new(ws, secrets, local).open();
    let (server_io, client_io) = tokio::io::duplex(1 << 23);
    tokio::spawn(async move {
        let service = server.serve(server_io).await.unwrap();
        let _ = service.waiting().await;
    });
    agent.serve(client_io).await.unwrap()
}

async fn call(client: &Client, tool: &str, arguments: Value) -> CallToolResult {
    let params = CallToolRequestParams::new(tool.to_owned())
        .with_arguments(arguments.as_object().unwrap().clone());
    client.call_tool(params).await.unwrap()
}

/// Every output the client got, for the final check.
#[derive(Default)]
struct Outputs {
    texts: Vec<(String, String)>,
}

impl Outputs {
    fn add(&mut self, what: &str, value: &impl serde::Serialize) {
        let text = serde_json::to_string(value).unwrap();
        self.texts.push((what.to_owned(), text));
    }

    fn assert_clean(&self) {
        for (what, text) in &self.texts {
            let leaked = leaked_form(text);
            assert!(
                leaked.is_none(),
                "{what} leaked the canary {leaked:?}: {text}"
            );
        }
        // The canary did reach the server's outputs, masked.
        let masked: Vec<&str> = self
            .texts
            .iter()
            .filter(|(_, text)| text.contains("«redacted:api_token»"))
            .map(|(what, _)| what.as_str())
            .collect();
        for what in [
            "send_request",
            "get_request",
            "get_report",
            "search_history",
        ] {
            assert!(
                masked.contains(&what),
                "{what} never showed the canary: {masked:?}"
            );
        }
        assert!(
            masked.iter().any(|w| w.starts_with("rustman://requests/")),
            "{masked:?}"
        );
        assert!(
            masked.iter().any(|w| w.starts_with("rustman://bodies/")),
            "{masked:?}"
        );
    }
}

async fn wait_until_done(client: &Client, tool: &str, args: Value, out: &mut Outputs) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let result = call(client, tool, args.clone()).await;
        out.add(tool, &result);
        let structured = result.structured_content.unwrap_or_default();
        let running = result.is_error == Some(true) || structured["state"] == "running";
        if !running {
            return;
        }
        assert!(Instant::now() < deadline, "{tool} {args}: never finished");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The `body_ref` of a result, if its body was truncated.
fn body_ref(result: &CallToolResult) -> Option<String> {
    let structured = result.structured_content.as_ref()?;
    structured["body"]["body_ref"].as_str().map(str::to_owned)
}

/// The gRPC tools: a call whose response echoes the canary, a saved
/// request carrying it in metadata, blocked targets and symbols naming it.
async fn call_grpc_tools(client: &Client, out: &mut Outputs) -> Vec<String> {
    let blocked = format!("grpc://blocked-{CANARY}.example:1");
    let calls = [
        (
            "grpc_call",
            json!({"target": "{{grpc_host}}", "method": "shop.v1.Catalog/GetProduct",
                   "message": {"id": "{{api_token}}"},
                   "metadata": {"authorization": "Bearer {{api_token}}"}}),
        ),
        ("grpc_call", json!({"path": "grpc/get"})),
        (
            "grpc_call",
            json!({"target": blocked, "method": "shop.v1.Catalog/GetProduct"}),
        ),
        (
            "grpc_call",
            json!({"target": "{{grpc_host}}", "method": format!("shop.v1.Catalog/{CANARY}")}),
        ),
        ("grpc_list", json!({"target": "{{grpc_host}}"})),
        ("grpc_list", json!({"target": blocked})),
        (
            "grpc_describe",
            json!({"path": "grpc/get", "symbol": "shop.v1.Product"}),
        ),
        (
            "grpc_describe",
            json!({"target": "{{grpc_host}}", "symbol": CANARY}),
        ),
    ];
    let mut called = Vec::new();
    for (tool, args) in calls {
        out.add(tool, &call(client, tool, args).await);
        called.push(tool.to_owned());
    }
    called
}

/// One call per catalogued tool (more for some), each with the canary in
/// its reach.
async fn call_every_tool(
    client: &Client,
    server: &TestServer,
    api: &BadApi,
    out: &mut Outputs,
) -> Vec<String> {
    let mut called: Vec<String> = Vec::new();
    let mut bodies = Vec::new();
    let explore = json!({"spec": "api/openapi.yaml", "target": api.url("")});
    let sends = [
        json!({"path": "users/echo"}),
        json!({"path": "users/leak"}),
        json!({"url": "{{base_url}}/echo", "headers": {"X-Canary": "{{api_token}}"}}),
        json!({"url": "{{base_url}}/latin1", "body": "{{api_token}}"}),
        json!({"url": format!("{}?pad=2100000", server.url("/mirror")), "body": "{{api_token}}"}),
        json!({"url": format!("https://blocked.example/{CANARY}")}),
    ];
    for args in sends {
        let result = call(client, "send_request", args).await;
        bodies.extend(body_ref(&result));
        out.add("send_request", &result);
    }
    called.push("send_request".to_owned());
    let rpc = [
        json!({"endpoint": "{{base_url}}/mirror", "method": "echo", "params": ["{{api_token}}"]}),
        json!({"endpoint": format!("https://blocked.example/{CANARY}"), "method": "echo"}),
        json!({"endpoint": "{{base_url}}/echo", "method": format!("m{CANARY}")}),
    ];
    for args in rpc {
        out.add("rpc_call", &call(client, "rpc_call", args).await);
    }
    called.push("rpc_call".to_owned());
    called.extend(call_grpc_tools(client, out).await);
    let simple = [
        ("list_requests", json!({})),
        ("list_requests", json!({"text": CANARY})),
        ("get_request", json!({"path": "users/leak"})),
        ("get_request", json!({"path": CANARY})),
        ("list_environments", json!({})),
        ("select_environment", json!({"name": "local"})),
        ("select_environment", json!({"name": CANARY})),
        (
            "import_openapi",
            json!({"spec": "api/openapi.yaml", "into": "bad"}),
        ),
        ("explore_plan", explore.clone()),
        ("search_history", json!({})),
        ("search_history", json!({"url": CANARY})),
    ];
    for (tool, args) in simple {
        out.add(tool, &call(client, tool, args).await);
        called.push(tool.to_owned());
    }
    let run = call(client, "run_collection", json!({"path": "users"})).await;
    out.add("run_collection", &run);
    called.push("run_collection".to_owned());
    let run_job = run.structured_content.unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_until_done(client, "get_report", json!({"id": run_job}), out).await;
    let started = call(client, "explore_run", explore).await;
    out.add("explore_run", &started);
    called.push("explore_run".to_owned());
    let job = started.structured_content.unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_until_done(client, "explore_status", json!({"id": job}), out).await;
    called.push("explore_status".to_owned());
    out.add(
        "explore_cancel",
        &call(client, "explore_cancel", json!({"job_id": job})).await,
    );
    called.push("explore_cancel".to_owned());
    for format in ["summary", "json", "markdown", "junit", "sarif", "html"] {
        for id in [&run_job, &job] {
            let result = call(client, "get_report", json!({"id": id, "format": format})).await;
            out.add("get_report", &result);
        }
    }
    called.push("get_report".to_owned());
    out.add(
        "list_findings",
        &call(client, "list_findings", json!({})).await,
    );
    called.push("list_findings".to_owned());
    out.add(
        "unknown",
        &client
            .call_tool(CallToolRequestParams::new(CANARY))
            .await
            .map_err(|e| e.to_string()),
    );
    called.extend(bodies.into_iter().map(|b| format!("body:{b}")));
    called
}

async fn read_every_resource(client: &Client, extra: &[String], out: &mut Outputs) -> usize {
    let mut uris: Vec<String> = client
        .list_all_resources()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.uri.clone())
        .collect();
    uris.extend(extra.iter().cloned());
    uris.push(format!("rustman://requests/{CANARY}"));
    for uri in &uris {
        let read = client
            .read_resource(ReadResourceRequestParams::new(uri.clone()))
            .await;
        out.add(uri, &read.map_err(|e| e.to_string()));
    }
    uris.len()
}

async fn get_every_prompt(client: &Client, out: &mut Outputs) {
    for prompt in client.list_all_prompts().await.unwrap() {
        let arguments: serde_json::Map<String, Value> = prompt
            .arguments
            .iter()
            .flatten()
            .map(|a| (a.name.clone(), Value::String(format!("x {CANARY} y"))))
            .collect();
        let params = GetPromptRequestParams::new(prompt.name.clone()).with_arguments(arguments);
        let got = client.get_prompt(params).await;
        out.add(&prompt.name, &got.map_err(|e| e.to_string()));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_canary_never_reaches_the_client() {
    let server = TestServer::http().await;
    let api = BadApi::start().await;
    let grpc = GrpcServer::start(Reflection::V1).await;
    let ws = workspace(&server, &api, &grpc);
    let agent = Agent::default();
    let seen = agent.seen.clone();
    let client = connect(ws.path(), agent).await;
    let mut out = Outputs::default();

    let catalog: Vec<String> = client
        .list_all_tools()
        .await
        .unwrap()
        .iter()
        .map(|t| t.name.to_string())
        .collect();
    let called = call_every_tool(&client, &server, &api, &mut out).await;
    let missing: Vec<&String> = catalog.iter().filter(|t| !called.contains(t)).collect();
    assert!(
        missing.is_empty(),
        "tools without a canary call: {missing:?}"
    );
    let bodies: Vec<String> = called
        .iter()
        .filter_map(|c| c.strip_prefix("body:"))
        .map(str::to_owned)
        .collect();
    assert!(!bodies.is_empty(), "no truncated body to read back");
    let read = read_every_resource(&client, &bodies, &mut out).await;
    assert!(read > 4, "{read} resources");
    get_every_prompt(&client, &mut out).await;
    for notification in seen.lock().unwrap().iter() {
        out.add("notification", notification);
    }
    out.assert_clean();
    // The audit log is an output too.
    let audit = std::fs::read_dir(ws.path().join(".rustman/audit")).unwrap();
    for file in audit {
        let text = std::fs::read_to_string(file.unwrap().path()).unwrap();
        for line in text.lines() {
            let leaked = leaked_form(line);
            assert!(
                leaked.is_none(),
                "the audit log leaked the canary {leaked:?}: {line}"
            );
        }
    }
}

/// A credential pasted as a literal header value or auth block, never
/// registered as a secret: saved request files are returned as text, where
/// no JSON header shape gives it away.
#[tokio::test(flavor = "multi_thread")]
async fn literal_credentials_in_request_files_never_reach_the_client() {
    const BEARER: &str = "literal-hardcoded-abc123";
    const API_KEY: &str = "litkey-999";
    const PASSWORD: &str = "pasted-pass-77";
    const EDITED: &str = "edited-after-load-55";
    const AD_HOC: &str = "adhoc-key-4411";
    let server = TestServer::http().await;
    let api = BadApi::start().await;
    let grpc = GrpcServer::start(Reflection::None).await;
    let ws = workspace(&server, &api, &grpc);
    let lit = format!(
        "version: 1\nkind: http\nname: Lit\nmethod: GET\nurl: \"{{{{base_url}}}}/echo\"\nheaders:\n  - name: Authorization\n    value: \"Bearer {BEARER}\"\n  - name: X-Api-Key\n    value: \"{API_KEY}\"\n"
    );
    write(ws.path(), "users/lit.rman.yaml", &lit);
    let basic = format!(
        "version: 1\nkind: http\nname: Basic\nmethod: GET\nurl: \"{{{{base_url}}}}/echo\"\nauth:\n  type: basic\n  username: admin\n  password: \"{PASSWORD}\"\n"
    );
    write(ws.path(), "users/basic.rman.yaml", &basic);
    let client = connect(ws.path(), Agent::default()).await;
    let mut outputs: Vec<(String, String)> = Vec::new();
    let ad_hoc = json!({"url": "{{base_url}}/echo", "headers": {"X-Api-Key": AD_HOC}});
    let sent = call(&client, "send_request", ad_hoc).await;
    outputs.push(("ad-hoc".into(), serde_json::to_string(&sent).unwrap()));
    for path in ["users/lit", "users/lit.rman.yaml", "users/basic"] {
        for tool in ["get_request", "send_request"] {
            let result = call(&client, tool, json!({ "path": path })).await;
            assert_ne!(result.is_error, Some(true), "{tool} {path}: {result:?}");
            outputs.push((
                format!("{tool} {path}"),
                serde_json::to_string(&result).unwrap(),
            ));
        }
    }
    for file in ["users/lit.rman.yaml", "users/basic.rman.yaml"] {
        let uri = format!("rustman://requests/{file}");
        let read = client
            .read_resource(ReadResourceRequestParams::new(uri.clone()))
            .await
            .unwrap();
        outputs.push((uri, serde_json::to_string(&read).unwrap()));
    }
    // Edited on disk after the session loaded the workspace.
    write(
        ws.path(),
        "users/basic.rman.yaml",
        &basic.replace(PASSWORD, EDITED),
    );
    let edited = call(&client, "get_request", json!({"path": "users/basic"})).await;
    outputs.push(("edited".into(), serde_json::to_string(&edited).unwrap()));
    assert!(outputs.last().unwrap().1.contains("«redacted:password»"));
    for (what, text) in &outputs {
        for literal in [BEARER, API_KEY, PASSWORD, EDITED, AD_HOC] {
            assert!(!text.contains(literal), "{what} leaked {literal}: {text}");
        }
    }
    let get = &outputs[1].1;
    assert!(get.contains("«redacted:Authorization»"), "{get}");
    assert!(get.contains("«redacted:X-Api-Key»"), "{get}");
}
