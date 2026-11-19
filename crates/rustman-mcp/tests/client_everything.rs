//! Contract test against the reference MCP server
//! (`@modelcontextprotocol/server-everything`, design "Risks"). It needs
//! Node and the package installed beforehand, so it is ignored by default
//! and runs with `--ignored` when `RUSTMAN_MCP_EVERYTHING` names the
//! package's `dist/index.js` (CI sets it up; see
//! `.github/workflows/ci.yml`). The server runs locally over stdio:
//! nothing touches the network.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use rustman_core::trust::{TrustKey, TrustStore};
use rustman_core::vars::Resolver;
use rustman_mcp::client::{
    CallOptions, CompletionTarget, ConnectOptions, McpSession, RequestOptions, ServerDefinition,
    StdioDefinition, TransportDefinition, require_trust,
};
use serde_json::json;

const PATIENCE: Duration = Duration::from_secs(60);

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Node and RUSTMAN_MCP_EVERYTHING (the mcp-reference-server CI job)"]
async fn the_reference_server_is_explored_and_called() {
    let entry = std::env::var("RUSTMAN_MCP_EVERYTHING")
        .expect("RUSTMAN_MCP_EVERYTHING names server-everything's dist/index.js");
    let dir = tempfile::tempdir().unwrap();
    let definition = ServerDefinition {
        name: "everything".into(),
        transport: TransportDefinition::Stdio(StdioDefinition {
            command: "node".into(),
            args: vec![entry, "stdio".into()],
            ..StdioDefinition::default()
        }),
        roots: None,
        timeout_ms: None,
        source: PathBuf::from("rustman.toml"),
    };
    let server = definition.resolve(&Resolver::new(), dir.path()).unwrap();
    let mut store =
        TrustStore::load_with_key(dir.path(), TrustKey::in_dir(dir.path().join("keys"))).unwrap();
    let approved = require_trust(&mut store, &server, true).unwrap();
    let session = tokio::time::timeout(
        PATIENCE,
        McpSession::connect(&server, &approved, &ConnectOptions::default()),
    )
    .await
    .expect("connected in time")
    .unwrap();
    let options = RequestOptions::default();

    let tools = session.list_tools(&options).await.unwrap().result;
    assert!(tools.iter().any(|t| t.name == "echo"));
    let echoed = session
        .call_tool("echo", json!({"message": "hi"}), &CallOptions::default())
        .await
        .unwrap()
        .result;
    assert_eq!(echoed.text(), "Echo: hi");
    assert!(!echoed.is_error);

    let prompts = session.list_prompts(&options).await.unwrap().result;
    let prompt = prompts
        .iter()
        .find(|p| p.name == "completable-prompt")
        .expect("the completable prompt");
    let target = CompletionTarget::Prompt(prompt.name.clone());
    let values = session
        .complete(&target, "department", "Eng", &options)
        .await
        .unwrap();
    assert_eq!(values, ["Engineering"]);
    let arguments = BTreeMap::from([
        ("department".to_owned(), "Engineering".to_owned()),
        ("name".to_owned(), "Alice".to_owned()),
    ]);
    let got = session
        .get_prompt(&prompt.name, &arguments, Some(prompt), &options)
        .await
        .unwrap()
        .result;
    assert_eq!(got.messages[0].role, "user");

    let resources = session.list_resources(&options).await.unwrap().result;
    let first = resources.first().expect("a resource");
    let read = session.read_resource(&first.uri, &options).await.unwrap();
    assert!(!read.result.is_empty());
    session.shutdown().await;
}
