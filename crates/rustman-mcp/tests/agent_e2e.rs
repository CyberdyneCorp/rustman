//! The end-to-end agent scenario (agent-mcp-interface task 5.1), as an rmcp
//! client drives it against the `bad-api` fixture: list the requests, plan
//! an exploration, run it read-only, poll its status, list the findings,
//! then read the report. Every step is audited, and nothing but safe
//! methods reaches the API.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation,
    ProgressNotificationParam,
};
use rmcp::service::{NotificationContext, RoleClient, RunningService};
use rmcp::{ClientHandler, ServiceExt};
use rustman_core::{SecretStore, Workspace};
use rustman_mcp::server::Sessions;
use rustman_test_support::bad_api::{self, BadApi};
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct Agent {
    progress: Arc<Mutex<Vec<ProgressNotificationParam>>>,
}

impl ClientHandler for Agent {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("e2e-agent", "1.0"),
        )
    }

    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _: NotificationContext<RoleClient>,
    ) {
        self.progress.lock().unwrap().push(params);
    }
}

type Client = RunningService<RoleClient, Agent>;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn workspace(api: &BadApi) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"bad\"\nversion = 1\n\n[safety]\nrate = 1000\nconcurrency = 4\n",
    );
    let env = format!("variables:\n  base_url: {}\n", api.url(""));
    write(dir.path(), "environments/local.yaml", &env);
    write(dir.path(), ".gitignore", ".rustman/\n");
    write(dir.path(), "api/openapi.yaml", bad_api::OPENAPI);
    write(
        dir.path(),
        "users/get-user.rman.yaml",
        "version: 1\nkind: http\nname: Get user\nmethod: GET\nurl: \"{{base_url}}/users/1\"\n",
    );
    dir
}

async fn connect(root: &Path, agent: Agent) -> Client {
    rustman_core::secrets::workspace_id::load_or_create(root).unwrap();
    let ws = Workspace::load(root).unwrap();
    let local = ws.environments.iter().find(|e| e.name == "local").cloned();
    let secrets = SecretStore::with_keychain(root, None);
    let server = Sessions::new(ws, secrets, local).open();
    let (server_io, client_io) = tokio::io::duplex(1 << 22);
    tokio::spawn(async move {
        let service = server.serve(server_io).await.unwrap();
        let _ = service.waiting().await;
    });
    agent.serve(client_io).await.unwrap()
}

async fn ok(client: &Client, tool: &str, arguments: Value) -> Value {
    let params = CallToolRequestParams::new(tool.to_owned())
        .with_arguments(arguments.as_object().unwrap().clone());
    let result = client.call_tool(params).await.unwrap();
    assert_eq!(result.is_error, Some(false), "{tool}: {result:?}");
    result.structured_content.unwrap()
}

/// The `tool` of every audit line, in order.
fn audited_tools(root: &Path) -> Vec<String> {
    let dir = root.join(".rustman").join("audit");
    let mut tools = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        for line in text.lines() {
            let line: Value = serde_json::from_str(line).unwrap();
            assert_eq!(line["outcome"], "ok", "{line}");
            tools.push(line["tool"].as_str().unwrap().to_owned());
        }
    }
    tools
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_explores_the_bad_api_and_triages_its_report() {
    let api = BadApi::start().await;
    let ws = workspace(&api);
    let agent = Agent::default();
    let progress = agent.progress.clone();
    let client = connect(ws.path(), agent).await;

    let requests = ok(&client, "list_requests", json!({})).await;
    assert_eq!(requests["total"], 1);
    assert_eq!(requests["items"][0]["id"], "users/get-user");

    let target = api.url("");
    let exploration = json!({"spec": "api/openapi.yaml", "target": target});
    let plan = ok(&client, "explore_plan", exploration.clone()).await;
    assert!(plan["allowed"].as_u64().unwrap() > 0);
    assert!(api.hits().is_empty(), "the plan sent requests");

    let started = ok(&client, "explore_run", exploration).await;
    let job = started["job_id"].as_str().unwrap();
    let id = started["exploration_id"].as_str().unwrap();
    assert_eq!(started["probes"], plan["total"]);

    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        let status = ok(&client, "explore_status", json!({"id": job})).await;
        if status["state"] != "running" {
            break status;
        }
        assert!(Instant::now() < deadline, "the exploration did not finish");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(status["state"], "completed", "{status:#}");
    assert_eq!(status["done"], status["total"]);
    let hits = api.hits();
    assert!(!hits.is_empty());
    assert!(
        hits.iter()
            .all(|h| ["GET", "HEAD", "OPTIONS"].contains(&h.method.as_str())),
        "a read-only exploration sent a mutation"
    );

    let findings = ok(&client, "list_findings", json!({"exploration_id": id})).await;
    let total = findings["total"].as_u64().unwrap();
    assert!(total > 0);
    let counted: u64 = status["findings"]
        .as_object()
        .unwrap()
        .values()
        .map(|n| n.as_u64().unwrap())
        .sum();
    assert_eq!(counted, total, "{status:#}");
    let high = ok(
        &client,
        "list_findings",
        json!({"exploration_id": id, "severity": "high"}),
    )
    .await;
    assert!(high["total"].as_u64().unwrap() > 0);

    let report = ok(&client, "get_report", json!({"id": id})).await;
    assert_eq!(report["kind"], "exploration");
    assert_eq!(report["uri"], format!("rustman://reports/{id}"));
    let markdown = ok(
        &client,
        "get_report",
        json!({"id": id, "format": "markdown"}),
    )
    .await;
    assert!(markdown["text"].as_str().unwrap().contains(&target));

    // The job reported its progress, ending at its total.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let last = progress.lock().unwrap().last().cloned();
        if let Some(last) = last.filter(|p| Some(p.progress) == p.total) {
            assert!(
                last.message
                    .unwrap()
                    .starts_with(&format!("{job} completed"))
            );
            break;
        }
        assert!(Instant::now() < deadline, "no final progress notification");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let tools = audited_tools(ws.path());
    for tool in [
        "list_requests",
        "explore_plan",
        "explore_run",
        "explore_status",
        "list_findings",
        "get_report",
    ] {
        assert!(tools.iter().any(|t| t == tool), "{tool} not audited");
    }
}
