//! The guardrails of agent sessions (agent-mcp-interface tasks 2.1-2.5),
//! driven through real MCP traffic: a probe handler wraps `AgentContext`
//! the way tools do, and an rmcp client with or without elicitation plays
//! the agent and the human. Requests go only to the in-process test server.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities, ClientConfig,
    ElicitRequestParams, ElicitResult, ElicitationAction, Implementation,
};
use rmcp::service::{RequestContext, RoleClient, RoleServer, RunningService};
use rmcp::{ClientHandler, ErrorData, ServerHandler, ServiceExt};
use rustman_core::model::RequestSettings;
use rustman_core::secrets::{Keychain, KeychainError};
use rustman_core::vars::{Resolved, ResolvedBody, ResolvedPair, ResolvedRequest, TlsFiles};
use rustman_core::{Secret, SecretStore, Workspace};
use rustman_http::SendOptions;
use rustman_mcp::server::audit::AUDIT_DIR;
use rustman_mcp::server::{AgentContext, Caller, ErrorCode, ToolOutput};
use rustman_test_support::TestServer;
use rustman_test_support::routes::timed_hits;
use serde_json::{Value, json};

const TOKEN: &str = "tok-81c2f0e94b7d4a35";

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

/// A workspace whose `local` environment points at `server` and declares
/// the secret `api_token`; `agent` is the `[agent]` table.
fn workspace(server: &TestServer, agent: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!("[workspace]\nname = \"t\"\nversion = 1\n{agent}");
    write(dir.path(), "rustman.toml", &toml);
    let env = format!(
        "variables:\n  base_url: {}\nsecrets:\n  - api_token\n",
        server.url("")
    );
    write(dir.path(), "environments/local.yaml", &env);
    write(dir.path(), "environments/staging.yaml", "variables: {}\n");
    write(dir.path(), ".gitignore", ".rustman/\n");
    dir
}

fn context(root: &Path) -> Arc<AgentContext> {
    // The keychain holds secrets under the workspace id.
    rustman_core::secrets::workspace_id::load_or_create(root).unwrap();
    let ws = Workspace::load(root).unwrap();
    let local = ws.environments.iter().find(|e| e.name == "local").cloned();
    let secrets = SecretStore::with_keychain(root, Some(Box::new(OneSecret)));
    Arc::new(AgentContext::new(Arc::new(ws), Arc::new(secrets), local))
}

fn resolved(method: &str, url: &str, headers: &[(&str, &str)]) -> ResolvedRequest {
    let pair = |(name, value): &(&str, &str)| ResolvedPair {
        name: Resolved::literal(*name),
        value: Resolved::literal(*value),
    };
    ResolvedRequest {
        method: method.into(),
        url: Resolved::literal(url),
        query: Vec::new(),
        headers: headers.iter().map(pair).collect(),
        body: ResolvedBody::None,
        auth: None,
        settings: RequestSettings::default(),
        tls: TlsFiles::default(),
        secrets: vec![Secret::new("api_token", TOKEN.to_owned())],
    }
}

/// A server with one tool, `send`, that sends `{method, url}` through the
/// session's guardrails and returns the exchange as tools do.
#[derive(Clone)]
struct Probe {
    ctx: Arc<AgentContext>,
}

impl ServerHandler for Probe {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        let client = context
            .peer
            .peer_info()
            .map(|info| info.client_info.name.clone())
            .unwrap_or_default();
        let caller = Caller {
            client: &client,
            tool: &request.name,
            arguments: &arguments,
        };
        let ctx = &self.ctx;
        let arguments = &arguments;
        let result = ctx
            .audited(&caller, |target| async move {
                let method = arguments["method"].as_str().unwrap_or("GET");
                let url = arguments["url"].as_str().unwrap_or_default();
                let auth = format!("Bearer {TOKEN}");
                let sent = resolved(method, url, &[("Authorization", &auth)]);
                let exchange = ctx
                    .send(&sent, &SendOptions::default(), &context.peer, &target)
                    .await?;
                let body = ctx.body_view(&exchange, &sent);
                let structured = json!({"status": exchange.status, "body": body});
                Ok(ToolOutput::new(structured, format!("{}", exchange.status)))
            })
            .await;
        Ok(result.into())
    }
}

/// The client: `answer` is the human's answer to every confirmation, or
/// `None` for a client without elicitation.
#[derive(Clone, Default)]
struct Agent {
    answer: Option<ElicitationAction>,
    asked: Arc<Mutex<Vec<String>>>,
}

impl ClientHandler for Agent {
    fn get_info(&self) -> ClientConfig {
        let capabilities = match self.answer {
            Some(_) => ClientCapabilities::builder().enable_elicitation().build(),
            None => ClientCapabilities::default(),
        };
        ClientConfig::new(capabilities, Implementation::new("probe-agent", "1.0"))
    }

    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        _: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, ErrorData> {
        if let ElicitRequestParams::FormElicitationParams { message, .. } = request {
            self.asked.lock().unwrap().push(message);
        }
        let action = self.answer.clone().unwrap_or(ElicitationAction::Decline);
        Ok(ElicitResult::new(action).with_content(json!({"confirm": true})))
    }
}

async fn connect(ctx: Arc<AgentContext>, agent: Agent) -> RunningService<RoleClient, Agent> {
    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        let service = Probe { ctx }.serve(server_io).await.unwrap();
        let _ = service.waiting().await;
    });
    agent.serve(client_io).await.unwrap()
}

async fn call(
    client: &RunningService<RoleClient, Agent>,
    method: &str,
    url: &str,
) -> CallToolResult {
    let arguments = json!({"method": method, "url": url});
    let params =
        CallToolRequestParams::new("send").with_arguments(arguments.as_object().unwrap().clone());
    client.call_tool(params).await.unwrap()
}

fn code(result: &CallToolResult) -> Option<String> {
    let structured = result.structured_content.as_ref()?;
    structured["code"].as_str().map(str::to_owned)
}

fn audit_lines(root: &Path) -> Vec<Value> {
    let Ok(entries) = std::fs::read_dir(root.join(AUDIT_DIR)) else {
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
async fn post_is_refused_by_default_and_audited_as_blocked() {
    let server = TestServer::http().await;
    let ws = workspace(&server, "");
    let client = connect(context(ws.path()), Agent::default()).await;
    let result = call(&client, "POST", &server.url("/echo")).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(code(&result).as_deref(), Some("blocked"));
    let message = &result.structured_content.unwrap()["message"];
    assert_eq!(message, "mutations not allowed for agents");
    let lines = audit_lines(ws.path());
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["tool"], "send");
    assert_eq!(lines[0]["outcome"], "blocked");
    assert_eq!(lines[0]["reason"], "mutations not allowed for agents");
    assert_eq!(lines[0]["client"], "probe-agent");
    assert_eq!(lines[0]["method"], "POST");
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_needs_allow_destructive_and_destructive_implies_mutations() {
    let server = TestServer::http().await;
    let ws = workspace(&server, "[agent]\nallow_mutations = true\n");
    let client = connect(context(ws.path()), Agent::default()).await;
    let result = call(&client, "DELETE", &server.url("/echo")).await;
    assert_eq!(code(&result).as_deref(), Some("blocked"));
    let message = &result.structured_content.unwrap()["message"];
    assert_eq!(message, "destructive not allowed for agents");

    // `allow_destructive` alone passes the method gate for POST: the
    // missing elicitation is what refuses it, not the method.
    let ws = workspace(&server, "[agent]\nallow_destructive = true\n");
    let client = connect(context(ws.path()), Agent::default()).await;
    let result = call(&client, "POST", &server.url("/echo")).await;
    assert_eq!(code(&result).as_deref(), Some("confirmation_unavailable"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unlisted_host_is_blocked_without_sending() {
    let server = TestServer::http().await;
    let ws = workspace(&server, "");
    let client = connect(context(ws.path()), Agent::default()).await;
    let result = call(&client, "GET", "https://example.org/").await;
    assert_eq!(code(&result).as_deref(), Some("blocked"));
    assert_eq!(
        result.structured_content.unwrap()["message"],
        "host not allowed"
    );
    assert_eq!(audit_lines(ws.path())[0]["outcome"], "blocked");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_human_declines_a_mutation() {
    let server = TestServer::http().await;
    let ws = workspace(&server, "[agent]\nallow_destructive = true\n");
    let agent = Agent {
        answer: Some(ElicitationAction::Decline),
        ..Agent::default()
    };
    let asked = agent.asked.clone();
    let client = connect(context(ws.path()), agent).await;
    let url = server.url("/timed/0?id=declined-delete");
    let result = call(&client, "DELETE", &url).await;
    assert_eq!(code(&result).as_deref(), Some("declined"));
    let asked = asked.lock().unwrap().clone();
    assert_eq!(
        asked,
        [format!(
            "An agent wants to send DELETE {url} (environment 'local'). Allow it?"
        )]
    );
    assert_eq!(audit_lines(ws.path())[0]["outcome"], "declined");
    assert!(timed_hits("declined-delete").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_human_confirms_a_mutation() {
    let server = TestServer::http().await;
    let ws = workspace(&server, "[agent]\nallow_mutations = true\n");
    let agent = Agent {
        answer: Some(ElicitationAction::Accept),
        ..Agent::default()
    };
    let client = connect(context(ws.path()), agent).await;
    let result = call(&client, "POST", &server.url("/echo")).await;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(result.structured_content.unwrap()["status"], 200);
    assert_eq!(audit_lines(ws.path())[0]["outcome"], "ok");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_without_elicitation_cannot_mutate() {
    let server = TestServer::http().await;
    let ws = workspace(&server, "[agent]\nallow_mutations = true\n");
    let client = connect(context(ws.path()), Agent::default()).await;
    let result = call(&client, "PUT", &server.url("/echo")).await;
    assert_eq!(code(&result).as_deref(), Some("confirmation_unavailable"));
    assert_eq!(audit_lines(ws.path())[0]["outcome"], "error");
    // Reads need no confirmation.
    let result = call(&client, "GET", &server.url("/echo")).await;
    assert_eq!(result.is_error, Some(false), "{result:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn require_confirmation_false_accepts_unattended_mutations() {
    let server = TestServer::http().await;
    let agent_table = "[agent]\nallow_mutations = true\nrequire_confirmation = false\n";
    let ws = workspace(&server, agent_table);
    let client = connect(context(ws.path()), Agent::default()).await;
    let result = call(&client, "PATCH", &server.url("/echo")).await;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    // A client that can ask still asks.
    let agent = Agent {
        answer: Some(ElicitationAction::Cancel),
        ..Agent::default()
    };
    let client = connect(context(ws.path()), agent).await;
    let result = call(&client, "PATCH", &server.url("/echo")).await;
    assert_eq!(code(&result).as_deref(), Some("declined"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_echoed_by_the_server_is_redacted_everywhere() {
    let server = TestServer::http().await;
    let ws = workspace(&server, "");
    let client = connect(context(ws.path()), Agent::default()).await;
    let url = format!("{}?token={TOKEN}", server.url("/echo"));
    let result = call(&client, "GET", &url).await;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let text = serde_json::to_string(&result).unwrap();
    assert!(!text.contains(TOKEN), "{text}");
    let body = result.structured_content.unwrap()["body"]["text"].clone();
    let body = body.as_str().unwrap();
    assert!(body.contains("«redacted:api_token»"), "{body}");
    // The audit log holds the redacted arguments and URL only.
    let audit = std::fs::read_dir(ws.path().join(AUDIT_DIR)).unwrap();
    for entry in audit {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(!text.contains(TOKEN), "{text}");
        assert!(text.contains("«redacted:api_token»"), "{text}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_2_mib_body_is_truncated_with_its_size_and_a_body_ref() {
    let server = TestServer::http().await;
    let ws = workspace(&server, "");
    let ctx = context(ws.path());
    let client = connect(ctx.clone(), Agent::default()).await;
    let result = call(&client, "GET", &server.url("/bytes/2097152")).await;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let body = &result.structured_content.unwrap()["body"];
    assert_eq!(body["truncated"], true);
    assert_eq!(body["size"], 2_097_152);
    assert_eq!(body["text"].as_str().unwrap().len(), 32 * 1024);
    let body_ref = body["body_ref"].as_str().unwrap();
    let id = body_ref.strip_prefix("rustman://bodies/").unwrap();
    assert_eq!(ctx.bodies().get(id).unwrap().len(), 2_097_152);
}

#[tokio::test(flavor = "multi_thread")]
async fn results_over_max_output_bytes_are_refused() {
    let server = TestServer::http().await;
    let ws = workspace(&server, "[agent]\nmax_output_bytes = 1024\n");
    let client = connect(context(ws.path()), Agent::default()).await;
    let result = call(&client, "GET", &server.url("/bytes/4096")).await;
    assert_eq!(code(&result).as_deref(), Some("invalid_input"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unwritable_audit_log_fails_the_call_before_it_runs() {
    let server = TestServer::http().await;
    let ws = workspace(&server, "");
    std::fs::create_dir_all(ws.path().join(".rustman")).unwrap();
    std::fs::write(ws.path().join(AUDIT_DIR), "a file, not a folder").unwrap();
    let ctx = context(ws.path());
    let arguments = json!({});
    let caller = Caller {
        client: "c",
        tool: "send",
        arguments: &arguments,
    };
    let ran = Arc::new(Mutex::new(false));
    let flag = ran.clone();
    let result = ctx
        .audited(&caller, |_| async move {
            *flag.lock().unwrap() = true;
            Ok(ToolOutput::new(json!({}), "ok"))
        })
        .await;
    assert_eq!(code(&result).as_deref(), Some(ErrorCode::Internal.as_str()));
    assert!(!*ran.lock().unwrap());
    // Through MCP too.
    let client = connect(ctx, Agent::default()).await;
    let result = call(&client, "GET", &server.url("/echo")).await;
    assert_eq!(code(&result).as_deref(), Some("internal"));
}

#[tokio::test(flavor = "multi_thread")]
async fn select_environment_is_session_scoped_and_changes_the_allowlist() {
    let server = TestServer::http().await;
    let ws = workspace(&server, "");
    let ctx = context(ws.path());
    // The environment's secrets are redacted from the start of the session.
    assert_eq!(ctx.redactor().text(TOKEN), "«redacted:api_token»");
    let before = std::fs::read_to_string(ws.path().join("rustman.toml")).unwrap();
    ctx.select_environment("staging").unwrap();
    assert_eq!(ctx.environment().unwrap().name, "staging");
    // `staging` declares no secret, but the ones seen stay redacted.
    assert_eq!(ctx.redactor().text(TOKEN), "«redacted:api_token»");
    let err = ctx.sender().unwrap_err();
    assert_eq!(err.message, "host not allowed");
    let missing = ctx.select_environment("qa").unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);
    let after = std::fs::read_to_string(ws.path().join("rustman.toml")).unwrap();
    assert_eq!(before, after);
    // A new session starts from the workspace's environment again.
    assert_eq!(context(ws.path()).environment().unwrap().name, "local");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_binary_body_resource_is_redacted_before_encoding() {
    use base64::Engine;
    use rmcp::model::ResourceContents;
    let server = TestServer::http().await;
    let dir = workspace(&server, "");
    let ctx = context(dir.path());
    // A non-UTF-8 body holding the secret, stored as received.
    let mut body = vec![0xff, 0xfe, 0x00];
    body.extend_from_slice(TOKEN.as_bytes());
    body.push(0xff);
    let id = ctx.bodies().insert(body.into()).unwrap();
    let uri = format!("rustman://bodies/{id}");
    let read = rustman_mcp::server::resources::read(&ctx, &uri).unwrap();
    let ResourceContents::BlobResourceContents { blob, .. } = read else {
        panic!("a blob resource");
    };
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(blob)
        .unwrap();
    let shown = String::from_utf8_lossy(&decoded);
    assert!(!shown.contains(TOKEN), "{shown}");
    assert!(shown.contains("«redacted:api_token»"), "{shown}");
}
