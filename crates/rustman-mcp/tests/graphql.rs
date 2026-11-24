//! `graphql_introspect` and `graphql_query` (agent-mcp-interface
//! "Conditional protocol tools", the `graphql` feature), driven through
//! real MCP traffic against in-process GraphQL servers: the schema from
//! introspection then from the cache, a host outside the allowlist, a
//! redacted result, validation before sending, and the mutation rule: a
//! mutation needs the agent's mutation permission and the human's
//! confirmation, and subscriptions are refused.

#![cfg(feature = "graphql")]

use std::path::Path;

use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, ElicitRequestParams,
    ElicitResult, ElicitationAction, Implementation,
};
use rmcp::service::{RequestContext, RoleClient, RunningService};
use rmcp::{ClientHandler, ErrorData, ServiceExt};
use rustman_core::secrets::{Keychain, KeychainError};
use rustman_core::{SecretStore, Workspace};
use rustman_mcp::server::Sessions;
use rustman_test_support::{GraphqlHttpServer, GraphqlServer, Reply};
use serde_json::{Value, json};

const CANARY: &str = "graphql-canary-7b42e0c9a1";

const SDL: &str = "type Query { user(id: ID!): User }\ntype Mutation { rename(name: String!): User }\ntype User { id: ID! name: String email: String @deprecated(reason: \"Use contact\") }\n";

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

/// A workspace whose `local` environment allows `base` (the agents'
/// allowlist comes from its values) and holds the secret `api_token`;
/// `agent` goes into `rustman.toml`.
fn workspace(base: &str, agent: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!("[workspace]\nname = \"t\"\nversion = 1\n{agent}");
    write(dir.path(), "rustman.toml", &toml);
    let env = format!("variables:\n  base: '{base}'\nsecrets:\n  - api_token\n");
    write(dir.path(), "environments/local.yaml", &env);
    write(dir.path(), ".gitignore", ".rustman/\n");
    write(dir.path(), "schema.graphql", SDL);
    write(
        dir.path(),
        "gql/user.rman.yaml",
        "version: 1\nkind: graphql\nname: User\nendpoint: '{{base}}/graphql'\ndocument: 'query U { user(id: 1) { name } }'\nheaders:\n  - name: Authorization\n    value: 'Bearer {{api_token}}'\nassertions:\n  - no_errors: true\n",
    );
    write(
        dir.path(),
        "gql/typo.rman.yaml",
        "version: 1\nkind: graphql\nname: Typo\nendpoint: '{{base}}/graphql'\nschema_file: ../schema.graphql\ndocument: '{ user(id: 1) { emial } }'\n",
    );
    write(
        dir.path(),
        "gql/ticks.rman.yaml",
        "version: 1\nkind: graphql\nname: Ticks\nendpoint: '{{base}}/graphql'\ntransport: sse\ndocument: 'subscription { ticks }'\n",
    );
    dir
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
        ClientConfig::new(capabilities, Implementation::new("graphql-agent", "1.0"))
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

/// The `{code, message}` of a refused call.
fn refusal(result: &CallToolResult) -> (String, String) {
    assert_eq!(result.is_error, Some(true), "{result:?}");
    let error = result.structured_content.clone().unwrap();
    (
        error["code"].as_str().unwrap().to_owned(),
        error["message"].as_str().unwrap().to_owned(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn introspection_is_cached_and_types_are_described() {
    let server = GraphqlServer::start(SDL).await;
    let ws = workspace(&server.url(""), "");
    let client = connect(ws.path()).await;
    let endpoint = server.url("/graphql");
    let first = ok(&client, "graphql_introspect", json!({"endpoint": endpoint})).await;
    assert_eq!(first["source"], "introspected");
    assert_eq!(first["overview"]["query"], "Query");
    assert_eq!(first["overview"]["mutation"], "Mutation");
    let names: Vec<&str> = first["overview"]["types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"User"), "{names:?}");
    assert_eq!(server.introspection_hits(), 1);
    let cache = ws.path().join(".rustman/graphql/schemas");
    assert_eq!(std::fs::read_dir(cache).unwrap().count(), 1);
    // The cache answers until refreshed: no second introspection.
    let user = ok(
        &client,
        "graphql_introspect",
        json!({"endpoint": endpoint, "type_name": "User"}),
    )
    .await;
    assert_eq!(user["source"], "cache");
    assert_eq!(user["type_detail"]["kind"], "OBJECT");
    let email = &user["type_detail"]["fields"][2];
    assert_eq!(email["name"], "email");
    assert_eq!(email["deprecated"], "Use contact");
    assert_eq!(server.introspection_hits(), 1);
    let missing = call(
        &client,
        "graphql_introspect",
        json!({"endpoint": endpoint, "type_name": "Nope"}),
    )
    .await;
    assert_eq!(refusal(&missing).0, "not_found");
    ok(
        &client,
        "graphql_introspect",
        json!({"endpoint": endpoint, "refresh": true}),
    )
    .await;
    assert_eq!(server.introspection_hits(), 2);
    // A saved request with a schema_file reads it and sends nothing.
    let file = ok(&client, "graphql_introspect", json!({"path": "gql/typo"})).await;
    assert_eq!(file["source"], "schema_file");
    assert_eq!(file["schema_file"], "../schema.graphql");
    assert_eq!(server.introspection_hits(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_outside_the_allowlist_is_refused_and_not_reached() {
    let allowed = GraphqlServer::start(SDL).await;
    let other = GraphqlServer::start(SDL).await;
    let ws = workspace(&allowed.url(""), "");
    let client = connect(ws.path()).await;
    let endpoint = other.url("/graphql");
    let schema = call(&client, "graphql_introspect", json!({"endpoint": endpoint})).await;
    assert_eq!(
        refusal(&schema),
        ("blocked".to_owned(), "host not allowed".to_owned())
    );
    let query = call(
        &client,
        "graphql_query",
        json!({"endpoint": endpoint, "query": "{ user(id: 1) { name } }"}),
    )
    .await;
    assert_eq!(refusal(&query).1, "host not allowed");
    assert!(other.hits().is_empty(), "nothing reached the other server");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_query_returns_redacted_data_and_errors() {
    let server = GraphqlHttpServer::start(|hit| {
        let echoed = hit.header("authorization").unwrap_or_default().to_owned();
        Reply::graphql(json!({
            "data": {"user": {"name": "Ada", "token": echoed}},
            "errors": [{"message": "email is private", "path": ["user", "email"]}],
        }))
    })
    .await;
    let ws = workspace(&server.url(""), "");
    let client = connect(ws.path()).await;
    let result = ok(&client, "graphql_query", json!({"path": "gql/user"})).await;
    assert_eq!(result["request"], "gql/user");
    assert_eq!(result["operation_type"], "query");
    assert_eq!(result["operation_name"], "U");
    assert_eq!(result["result"], "partial");
    assert_eq!(result["status"], 200);
    assert_eq!(result["data"]["user"]["name"], "Ada");
    assert_eq!(result["errors"][0]["message"], "email is private");
    // `no_errors` failed on the partial result.
    assert_eq!(result["outcome"], "failed");
    let text = result.to_string();
    assert!(!text.contains(CANARY), "the secret leaked: {text}");
    assert_eq!(server.hits().len(), 1);
    assert_eq!(
        server.hits()[0].header("authorization"),
        Some(format!("Bearer {CANARY}").as_str()),
        "the request carried the secret"
    );
    let adhoc = ok(
        &client,
        "graphql_query",
        json!({
            "endpoint": "{{base}}/graphql",
            "query": "query Q($id: ID!) { user(id: $id) { name } }",
            "variables": {"id": "7"},
            "headers": {"X-Trace": "on"},
        }),
    )
    .await;
    assert_eq!(adhoc["request"], "ad-hoc");
    let sent = server.hits()[1].json();
    assert_eq!(sent["variables"], json!({"id": "7"}));
    assert_eq!(server.hits()[1].header("x-trace"), Some("on"));
    // Recorded in history like every request.
    let history = ok(&client, "search_history", json!({})).await;
    assert_eq!(history["total"], 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_document_is_not_sent() {
    let server = GraphqlHttpServer::start(|_| Reply::graphql(json!({"data": {}}))).await;
    let ws = workspace(&server.url(""), "");
    let client = connect(ws.path()).await;
    let result = ok(&client, "graphql_query", json!({"path": "gql/typo"})).await;
    assert_eq!(result["outcome"], "errored");
    let error = result["error"].as_str().unwrap();
    assert!(error.contains("Cannot query field \"emial\""), "{error}");
    assert!(server.hits().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn mutations_need_the_permission_and_the_human() {
    let server =
        GraphqlHttpServer::start(|_| Reply::graphql(json!({"data": {"rename": {"id": "1"}}})))
            .await;
    let mutation = |base: &str| {
        json!({
            "endpoint": format!("{base}/graphql"),
            "query": "mutation { rename(name: \"x\") { id } }",
        })
    };
    let base = server.url("");
    // Without allow_mutations: refused, nothing sent.
    let ws = workspace(&base, "");
    let client = connect(ws.path()).await;
    let refused = call(&client, "graphql_query", mutation(&base)).await;
    assert_eq!(
        refusal(&refused),
        (
            "blocked".to_owned(),
            "mutations not allowed for agents".to_owned()
        )
    );
    // Allowed, but the human declines.
    let ws = workspace(&base, "\n[agent]\nallow_mutations = true\n");
    let agent = Agent {
        answer: Some(ElicitationAction::Decline),
    };
    let client = connect_with(ws.path(), agent).await;
    let declined = call(&client, "graphql_query", mutation(&base)).await;
    assert_eq!(refusal(&declined).0, "declined");
    // A client that cannot ask the human.
    let client = connect(ws.path()).await;
    let unavailable = call(&client, "graphql_query", mutation(&base)).await;
    assert_eq!(refusal(&unavailable).0, "confirmation_unavailable");
    assert!(server.hits().is_empty(), "no mutation was sent");
    // The human confirms.
    let agent = Agent {
        answer: Some(ElicitationAction::Accept),
    };
    let client = connect_with(ws.path(), agent).await;
    let sent = ok(&client, "graphql_query", mutation(&base)).await;
    assert_eq!(sent["operation_type"], "mutation");
    assert_eq!(sent["data"]["rename"]["id"], "1");
    assert_eq!(server.hits().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn subscriptions_are_refused() {
    let server = GraphqlHttpServer::start(|_| Reply::graphql(json!({"data": {}}))).await;
    let base = server.url("");
    let ws = workspace(
        &base,
        "\n[agent]\nallow_mutations = true\nrequire_confirmation = false\n",
    );
    let client = connect(ws.path()).await;
    let saved = call(&client, "graphql_query", json!({"path": "gql/ticks"})).await;
    assert_eq!(refusal(&saved).0, "blocked");
    let adhoc = call(
        &client,
        "graphql_query",
        json!({"endpoint": format!("{base}/graphql"), "query": "subscription { ticks }"}),
    )
    .await;
    assert_eq!(refusal(&adhoc).0, "blocked");
    assert!(server.hits().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_operation_in_the_endpoint_url_is_refused_and_not_sent() {
    let server = GraphqlHttpServer::start(|_| Reply::graphql(json!({"data": {}}))).await;
    let base = server.url("");
    let ws = workspace(&base, "");
    let client = connect(ws.path()).await;
    // Some servers let URL parameters override the POSTed body.
    for suffix in [
        "?query=mutation%7Brename(name:%22x%22)%7Bid%7D%7D",
        "?operationName=B",
        "?a=1&extensions=%7B%7D",
    ] {
        let endpoint = format!("{base}/graphql{suffix}");
        let refused = call(
            &client,
            "graphql_query",
            json!({"endpoint": endpoint, "query": "{ __typename }"}),
        )
        .await;
        let (code, message) = refusal(&refused);
        assert_eq!(code, "blocked", "{suffix}");
        assert!(message.contains("operation parameters"), "{message}");
        let introspect = call(&client, "graphql_introspect", json!({"endpoint": endpoint})).await;
        assert_eq!(refusal(&introspect).0, "blocked", "{suffix}");
    }
    assert!(server.hits().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn ad_hoc_variables_cannot_upload_workspace_files() {
    let server = GraphqlHttpServer::start(|_| Reply::graphql(json!({"data": {}}))).await;
    let base = server.url("");
    let ws = workspace(
        &base,
        "\n[agent]\nallow_mutations = true\nrequire_confirmation = false\n",
    );
    write(ws.path(), "certs/client.key", "PRIVATE KEY");
    let client = connect(ws.path()).await;
    for variables in [
        json!({"f": {"$file": "certs/client.key"}}),
        json!({"input": {"files": [{"$file": "certs/client.key"}]}}),
    ] {
        let refused = call(
            &client,
            "graphql_query",
            json!({
                "endpoint": format!("{base}/graphql"),
                "query": "mutation($f: Upload) { upload(file: $f) }",
                "variables": variables,
            }),
        )
        .await;
        let (code, message) = refusal(&refused);
        assert_eq!(code, "blocked");
        assert!(message.contains("file uploads"), "{message}");
    }
    assert!(server.hits().is_empty());
}
