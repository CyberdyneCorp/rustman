//! `tools/list` per feature set (agent-mcp-interface "Conditional protocol
//! tools"): the tools a build registers, with their arguments and hints,
//! snapshotted once per combination of the `grpc`, `graphql` and `webhook`
//! features. A build without a capability lists none of its tools. CI runs
//! this test for every set:
//!
//! ```text
//! cargo test -p rustman-mcp --no-default-features --features <set> --test tools_list
//! ```

use std::path::Path;

use rmcp::ServiceExt;
use rmcp::model::Tool;
use rustman_core::{SecretStore, Workspace};
use rustman_mcp::server::Sessions;

/// The snapshot of this build: its features joined with `-`, or `none`.
fn feature_set() -> &'static str {
    match (
        cfg!(feature = "grpc"),
        cfg!(feature = "graphql"),
        cfg!(feature = "webhook"),
    ) {
        (false, false, false) => "none",
        (false, true, false) => "graphql",
        (false, false, true) => "webhook",
        (false, true, true) => "graphql-webhook",
        (true, false, false) => "grpc",
        (true, true, false) => "grpc-graphql",
        (true, false, true) => "grpc-webhook",
        (true, true, true) => "grpc-graphql-webhook",
    }
}

/// One line per tool: its name, hints and arguments (required ones with
/// `*`).
fn render(tools: &[Tool]) -> String {
    let mut lines = Vec::new();
    for tool in tools {
        let annotations = tool.annotations.as_ref().expect("annotations");
        let hint = match (
            annotations.read_only_hint.unwrap_or(false),
            annotations.destructive_hint.unwrap_or(false),
        ) {
            (true, _) => "read-only",
            (false, true) => "sends",
            (false, false) => "writes",
        };
        let required: Vec<&str> = tool
            .input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .collect();
        let arguments: Vec<String> = tool
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .into_iter()
            .flatten()
            .map(|(name, _)| match required.contains(&name.as_str()) {
                true => format!("{name}*"),
                false => name.clone(),
            })
            .collect();
        lines.push(format!("{} [{hint}] ({})", tool.name, arguments.join(", ")));
    }
    lines.join("\n") + "\n"
}

async fn tools_list(root: &Path) -> Vec<Tool> {
    std::fs::write(
        root.join("rustman.toml"),
        "[workspace]\nname = \"t\"\nversion = 1\n",
    )
    .unwrap();
    rustman_core::secrets::workspace_id::load_or_create(root).unwrap();
    let ws = Workspace::load(root).unwrap();
    let secrets = SecretStore::with_keychain(root, None);
    let server = Sessions::new(ws, secrets, None).open();
    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        let service = server.serve(server_io).await.unwrap();
        let _ = service.waiting().await;
    });
    let client = ().serve(client_io).await.unwrap();
    client.list_all_tools().await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn tools_list_matches_the_snapshot_of_this_feature_set() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools_list(dir.path()).await;
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    // Scenario "Build without webhook inspector", and its GraphQL twin.
    for name in ["graphql_introspect", "graphql_query"] {
        assert_eq!(names.contains(&name), cfg!(feature = "graphql"), "{name}");
    }
    assert_eq!(
        names.contains(&"list_webhook_captures"),
        cfg!(feature = "webhook")
    );
    for name in ["grpc_list", "grpc_describe", "grpc_call"] {
        assert_eq!(names.contains(&name), cfg!(feature = "grpc"), "{name}");
    }
    insta::assert_snapshot!(feature_set(), render(&tools));
}
