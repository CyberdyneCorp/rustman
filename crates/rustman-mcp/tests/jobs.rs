//! Background jobs (agent-mcp-interface tasks 4.1 and 4.2): `explore_run`
//! answers at once while the target is slow, a third job is `busy`,
//! progress notifications come at least every 5 s, and both
//! `explore_cancel` and an MCP cancellation leave the exploration
//! `cancelled`. Requests go only to the in-process bad API.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rmcp::model::{
    CallToolRequest, CallToolRequestParams, CallToolResult, CancelledNotificationParam,
    ClientCapabilities, ClientConfig, ClientRequest, Implementation, ProgressNotificationParam,
    ServerResult,
};
use rmcp::service::{NotificationContext, PeerRequestOptions, RoleClient, RunningService};
use rmcp::{ClientHandler, ServiceExt};
use rustman_core::{SecretStore, Workspace};
use rustman_mcp::server::Sessions;
use rustman_test_support::bad_api::{self, BadApi};
use serde_json::{Value, json};

/// Progress notifications as they arrive, with their arrival time.
type Seen = Arc<Mutex<Vec<(Instant, ProgressNotificationParam)>>>;

#[derive(Clone, Default)]
struct Agent {
    progress: Seen,
}

impl ClientHandler for Agent {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("jobs-agent", "1.0"),
        )
    }

    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _: NotificationContext<RoleClient>,
    ) {
        self.progress.lock().unwrap().push((Instant::now(), params));
    }
}

type Client = RunningService<RoleClient, Agent>;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A workspace whose `local` environment points at `api`, sending at most
/// `rate` requests per second.
fn workspace(api: &BadApi, rate: u32) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!(
        "[workspace]\nname = \"t\"\nversion = 1\n\n[safety]\nrate = {rate}\nconcurrency = 1\n"
    );
    write(dir.path(), "rustman.toml", &toml);
    let env = format!("variables:\n  base_url: {}\n", api.url(""));
    write(dir.path(), "environments/local.yaml", &env);
    write(dir.path(), ".gitignore", ".rustman/\n");
    write(dir.path(), "api/openapi.yaml", bad_api::OPENAPI);
    dir
}

fn sessions(root: &Path) -> Sessions {
    rustman_core::secrets::workspace_id::load_or_create(root).unwrap();
    let ws = Workspace::load(root).unwrap();
    let local = ws.environments.iter().find(|e| e.name == "local").cloned();
    let secrets = SecretStore::with_keychain(root, None);
    Sessions::new(ws, secrets, local)
}

async fn connect(root: &Path, agent: Agent) -> Client {
    open(&sessions(root), agent).await
}

/// A new session of `sessions`, as a second client of one HTTP server gets.
async fn open(sessions: &Sessions, agent: Agent) -> Client {
    let server = sessions.open();
    let (server_io, client_io) = tokio::io::duplex(1 << 22);
    tokio::spawn(async move {
        let service = server.serve(server_io).await.unwrap();
        let _ = service.waiting().await;
    });
    agent.serve(client_io).await.unwrap()
}

fn params(tool: &str, arguments: &Value) -> CallToolRequestParams {
    CallToolRequestParams::new(tool.to_owned())
        .with_arguments(arguments.as_object().unwrap().clone())
}

async fn call(client: &Client, tool: &str, arguments: Value) -> CallToolResult {
    client.call_tool(params(tool, &arguments)).await.unwrap()
}

async fn ok(client: &Client, tool: &str, arguments: Value) -> Value {
    let result = call(client, tool, arguments).await;
    assert_eq!(result.is_error, Some(false), "{tool}: {result:?}");
    result.structured_content.unwrap()
}

/// An exploration of many probes: at a low rate it runs for a while.
fn exploration(api: &BadApi) -> Value {
    json!({
        "spec": "api/openapi.yaml",
        "target": api.url(""),
        "probes": ["conformance", "auth", "fuzz"],
        "fuzz_cases": 8,
        "seed": 3,
    })
}

/// Start an exploration and return its `job_id`, checking that the call
/// answered within 2 s.
async fn start(client: &Client, api: &BadApi) -> String {
    let began = Instant::now();
    let started = ok(client, "explore_run", exploration(api)).await;
    let took = began.elapsed();
    assert!(took < Duration::from_secs(2), "explore_run took {took:?}");
    started["job_id"].as_str().unwrap().to_owned()
}

/// Poll `explore_status` until it reports `state`.
async fn wait_for(client: &Client, job: &str, state: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let status = ok(client, "explore_status", json!({"id": job})).await;
        if status["state"] == state {
            return status;
        }
        assert!(Instant::now() < deadline, "never {state}: {status:#}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn jobs_answer_at_once_on_a_slow_target_and_a_third_is_busy() {
    // Every GET is received but never answered: probes hang.
    let api = BadApi::start_stalling("GET", "/").await;
    let ws = workspace(&api, 1000);
    let client = connect(ws.path(), Agent::default()).await;
    let first = start(&client, &api).await;
    let second = start(&client, &api).await;
    let third = call(&client, "explore_run", exploration(&api)).await;
    assert_eq!(third.is_error, Some(true), "{third:?}");
    assert_eq!(third.structured_content.unwrap()["code"], "busy");
    let run = call(&client, "explore_status", json!({"id": first})).await;
    assert_eq!(run.structured_content.unwrap()["state"], "running");
    let (a, b) = tokio::join!(
        ok(&client, "explore_cancel", json!({"job_id": first})),
        ok(&client, "explore_cancel", json!({"job_id": second})),
    );
    assert_eq!(a["state"], "cancelled", "{a:#}");
    assert_eq!(b["state"], "cancelled", "{b:#}");
    // A freed slot takes a new job.
    let fourth = start(&client, &api).await;
    ok(&client, "explore_cancel", json!({"job_id": fourth})).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn max_jobs_holds_across_the_sessions_of_one_server() {
    let api = BadApi::start_stalling("GET", "/").await;
    let ws = workspace(&api, 1000);
    let server = sessions(ws.path());
    let one = open(&server, Agent::default()).await;
    let two = open(&server, Agent::default()).await;
    let first = start(&one, &api).await;
    let second = start(&two, &api).await;
    for client in [&one, &two] {
        let third = call(client, "explore_run", exploration(&api)).await;
        assert_eq!(third.is_error, Some(true), "{third:?}");
        assert_eq!(third.structured_content.unwrap()["code"], "busy");
    }
    ok(&one, "explore_cancel", json!({"job_id": first})).await;
    let third = start(&two, &api).await;
    ok(&two, "explore_cancel", json!({"job_id": second})).await;
    ok(&two, "explore_cancel", json!({"job_id": third})).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_job_reports_progress_at_least_every_five_seconds() {
    let api = BadApi::start().await;
    let ws = workspace(&api, 1);
    let agent = Agent::default();
    let seen = agent.progress.clone();
    let client = connect(ws.path(), agent).await;
    let plan = ok(&client, "explore_plan", exploration(&api)).await;
    // At 1 request per second the job far outlasts the window below.
    assert!(plan["allowed"].as_u64().unwrap() >= 15, "{plan:#}");
    let began = Instant::now();
    let job = start(&client, &api).await;
    tokio::time::sleep(Duration::from_secs(7)).await;
    let status = ok(&client, "explore_status", json!({"id": job})).await;
    assert_eq!(status["state"], "running", "{status:#}");
    assert!(status["done"].as_u64().unwrap() > 0, "{status:#}");
    let cancelled = ok(&client, "explore_cancel", json!({"job_id": job})).await;
    assert_eq!(cancelled["state"], "cancelled");

    let seen = seen.lock().unwrap().clone();
    let window: Vec<_> = seen
        .iter()
        .filter(|(at, _)| *at < began + Duration::from_secs(7))
        .collect();
    assert!(window.len() >= 2, "{} notifications", window.len());
    let mut last = began;
    for (at, _) in &window {
        let gap = at.duration_since(last);
        assert!(gap <= Duration::from_secs(5), "a {gap:?} gap");
        last = *at;
    }
    let token = &seen[0].1.progress_token;
    assert!(seen.iter().all(|(_, p)| &p.progress_token == token));
    let values: Vec<f64> = seen.iter().map(|(_, p)| p.progress).collect();
    assert!(values.windows(2).all(|w| w[0] <= w[1]), "{values:?}");
    let total = plan["total"].as_f64().unwrap();
    assert!(seen.iter().all(|(_, p)| p.total == Some(total)));
    let message = seen[0].1.message.as_deref().unwrap();
    assert!(
        message.starts_with(&format!("{job} running: ")),
        "{message}"
    );
    assert!(message.ends_with(" probes"), "{message}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_mcp_cancellation_of_explore_run_cancels_its_job() {
    let api = BadApi::start().await;
    let ws = workspace(&api, 2);
    let client = connect(ws.path(), Agent::default()).await;
    let request = ClientRequest::CallToolRequest(CallToolRequest::new(params(
        "explore_run",
        &exploration(&api),
    )));
    let handle = client
        .send_request_with_option(request, PeerRequestOptions::no_options())
        .await
        .unwrap();
    let request_id = handle.id.clone();
    let ServerResult::CallToolResult(started) = handle.await_response().await.unwrap() else {
        panic!("a tool result");
    };
    let job = started.structured_content.unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    client
        .notify_cancelled(CancelledNotificationParam::new(
            Some(request_id),
            Some("the user stopped it".into()),
        ))
        .await
        .unwrap();
    let status = wait_for(&client, &job, "cancelled").await;
    assert_eq!(status["phase"], "execute", "{status:#}");
    let total = status["total"].as_u64().unwrap();
    assert!(status["done"].as_u64().unwrap() < total, "{status:#}");
    // The exploration stays resumable: interrupted on disk.
    let id = status["exploration_id"].as_str().unwrap();
    let state = std::fs::read_to_string(
        ws.path()
            .join(".rustman/explorations")
            .join(id)
            .join("state.json"),
    )
    .unwrap();
    let state: Value = serde_json::from_str(&state).unwrap();
    assert_eq!(state["state"], "interrupted", "{state:#}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_job_reports_its_progress_while_running() {
    let api = BadApi::start_stalling("GET", "/users").await;
    let ws = workspace(&api, 1000);
    write(
        ws.path(),
        "users/list.rman.yaml",
        "version: 1\nkind: http\nname: List users\nmethod: GET\nurl: \"{{base_url}}/users\"\n",
    );
    let client = connect(ws.path(), Agent::default()).await;
    let began = Instant::now();
    let started = ok(&client, "run_collection", json!({"path": "users"})).await;
    assert!(began.elapsed() < Duration::from_secs(2));
    let job = started["job_id"].as_str().unwrap();
    let running = call(&client, "get_report", json!({"id": job})).await;
    let message = running.structured_content.unwrap()["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        message,
        format!("{job} is still running (0/1 requests); check again later")
    );
}

/// Wait until `api` has received at least `count` requests.
async fn wait_for_hits(api: &BadApi, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while api.hits().len() < count {
        assert!(Instant::now() < deadline, "only {} hits", api.hits().len());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_job_stops_sending_once_its_audit_log_cannot_be_written() {
    let api = BadApi::start().await;
    let ws = workspace(&api, 1);
    for n in 1..=5 {
        let request = format!(
            "version: 1\nkind: http\nname: User {n}\nmethod: GET\nurl: \"{{{{base_url}}}}/users/{n}\"\n"
        );
        write(ws.path(), &format!("users/u{n}.rman.yaml"), &request);
    }
    let client = connect(ws.path(), Agent::default()).await;
    let started = ok(&client, "run_collection", json!({"path": "users"})).await;
    let job = started["job_id"].as_str().unwrap().to_owned();
    // At one request per second, break the audit log right after the first.
    wait_for_hits(&api, 1).await;
    let audit = ws.path().join(".rustman/audit");
    std::fs::remove_dir_all(&audit).unwrap();
    std::fs::write(&audit, "not a folder").unwrap();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    std::fs::remove_file(&audit).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let report = call(&client, "get_report", json!({"id": job})).await;
        let text = serde_json::to_string(&report.structured_content).unwrap();
        if !text.contains("still running") {
            break;
        }
        assert!(Instant::now() < deadline, "the run never finished: {text}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // The request already waiting for its turn may go out; no later one.
    let hits = api.hits().len();
    assert!(hits <= 2, "{hits} requests were sent without an audit line");
}
