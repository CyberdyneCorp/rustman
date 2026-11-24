//! `list_webhook_captures` (agent-mcp-interface "Conditional protocol
//! tools", the `webhook` feature), driven through real MCP traffic: the
//! captures a human saved to history from the webhook inspector, newest
//! first, filtered and paged, redacted, and never the outbound exchanges.

#![cfg(feature = "webhook")]

use std::path::Path;

use bytes::Bytes;
use http::Version;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rustman_core::history::HistoryStore;
use rustman_core::secrets::{Keychain, KeychainError};
use rustman_core::{Redactor, SecretStore, Workspace};
use rustman_mcp::server::Sessions;
use rustman_webhook::save::save_to_history;
use rustman_webhook::{Capture, HeaderEntry, Signature};
use serde_json::{Value, json};
use time::OffsetDateTime;

const CANARY: &str = "webhook-canary-3f9a61d2c8";

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

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(
        dir.path(),
        "environments/local.yaml",
        "variables:\n  base: 'http://127.0.0.1:9'\nsecrets:\n  - api_token\n",
    );
    write(dir.path(), ".gitignore", ".rustman/\n");
    dir
}

fn capture(id: u64, path: &str, body: &[u8], signature: Option<Signature>) -> Capture {
    Capture {
        id,
        received_at: OffsetDateTime::UNIX_EPOCH,
        remote: "127.0.0.1:5000".parse().unwrap(),
        version: Version::HTTP_11,
        method: "POST".into(),
        path: path.into(),
        query: None,
        headers: vec![
            HeaderEntry::new("Content-Type", "application/json"),
            HeaderEntry::new("Authorization", "Bearer hook-token-91c3"),
        ],
        body: Bytes::copy_from_slice(body),
        status: 200,
        mock: None,
        signature,
    }
}

/// Save the captures to the workspace's history, oldest first, as the
/// webhook inspector's "save to history" does.
fn save(root: &Path) {
    let ws = Workspace::load(root).unwrap();
    let store = HistoryStore::for_workspace(root, &ws.settings.history);
    let redactor = Redactor::new();
    let body = format!("{{\"action\":\"opened\",\"key\":\"{CANARY}\"}}");
    let captures = [
        capture(1, "/hooks/github", body.as_bytes(), Some(Signature::Valid)),
        capture(2, "/hooks/github", b"{}", Some(Signature::Invalid)),
        capture(3, "/hooks/stripe", &[0xff, 0x00, 0xfe], None),
    ];
    for capture in &captures {
        save_to_history(capture, &store, &redactor).unwrap();
        // History ids are ULIDs: keep them in saving order.
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

type Client = RunningService<RoleClient, ()>;

async fn connect(root: &Path) -> Client {
    rustman_core::secrets::workspace_id::load_or_create(root).unwrap();
    let ws = Workspace::load(root).unwrap();
    let local = ws.environments.iter().find(|e| e.name == "local").cloned();
    let secrets = SecretStore::with_keychain(root, Some(Box::new(OneSecret)));
    let server = Sessions::new(ws, secrets, local).open();
    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        let service = server.serve(server_io).await.unwrap();
        let _ = service.waiting().await;
    });
    ().serve(client_io).await.unwrap()
}

async fn call(client: &Client, tool: &str, arguments: Value) -> CallToolResult {
    let params = CallToolRequestParams::new(tool.to_owned())
        .with_arguments(arguments.as_object().unwrap().clone());
    client.call_tool(params).await.unwrap()
}

async fn ok(client: &Client, tool: &str, arguments: Value) -> Value {
    let result = call(client, tool, arguments).await;
    assert_eq!(result.is_error, Some(false), "{tool}: {result:?}");
    result.structured_content.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn saved_captures_are_listed_newest_first_and_redacted() {
    let ws = workspace();
    save(ws.path());
    let client = connect(ws.path()).await;
    // An outbound exchange in the same history is not a capture.
    ok(
        &client,
        "send_request",
        json!({"method": "GET", "url": "{{base}}/health"}),
    )
    .await;
    let all = ok(&client, "list_webhook_captures", json!({})).await;
    assert_eq!(all["total"], 3);
    let ids: Vec<u64> = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["capture_id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, [3, 2, 1]);
    let github = &all["items"][2];
    assert_eq!(github["method"], "POST");
    assert_eq!(github["target"], "/hooks/github");
    assert_eq!(github["status"], 200);
    assert_eq!(github["signature"], "valid");
    assert_eq!(github["remote"], "127.0.0.1:5000");
    assert_eq!(github["body"]["encoding"], "utf-8");
    let text = all.to_string();
    assert!(!text.contains(CANARY), "a secret leaked: {text}");
    assert!(
        !text.contains("hook-token-91c3"),
        "a credential leaked: {text}"
    );
    let stripe = &all["items"][0];
    assert_eq!(stripe["body"]["encoding"], "base64");
    assert_eq!(stripe["body"]["text"], "/wD+");
}

#[tokio::test(flavor = "multi_thread")]
async fn captures_filter_by_path_signature_and_method_and_page() {
    let ws = workspace();
    save(ws.path());
    let client = connect(ws.path()).await;
    let valid = ok(
        &client,
        "list_webhook_captures",
        json!({"verdict": "valid"}),
    )
    .await;
    assert_eq!(valid["total"], 1);
    assert_eq!(valid["items"][0]["capture_id"], 1);
    let stripe = ok(&client, "list_webhook_captures", json!({"path": "STRIPE"})).await;
    assert_eq!(stripe["total"], 1);
    let gets = ok(&client, "list_webhook_captures", json!({"method": "get"})).await;
    assert_eq!(gets["total"], 0);
    let first = ok(&client, "list_webhook_captures", json!({"limit": 2})).await;
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    let cursor = first["next_cursor"].as_str().unwrap();
    let second = ok(
        &client,
        "list_webhook_captures",
        json!({"limit": 2, "cursor": cursor}),
    )
    .await;
    assert_eq!(second["items"][0]["capture_id"], 1);
    assert!(second.get("next_cursor").is_none());
    let bad = call(
        &client,
        "list_webhook_captures",
        json!({"verdict": "forged"}),
    )
    .await;
    assert_eq!(bad.is_error, Some(true));
    assert_eq!(bad.structured_content.unwrap()["code"], "invalid_input");
}
