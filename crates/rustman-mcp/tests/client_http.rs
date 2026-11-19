//! The MCP client over HTTP against the fake servers of
//! `rustman-test-support`: Streamable HTTP session ids, the definition's
//! headers and auth, and the legacy HTTP+SSE fallback.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustman_core::model::Auth;
use rustman_core::trust::{TrustKey, TrustStore};
use rustman_core::vars::Resolver;
use rustman_mcp::client::{
    ClientError, ConnectOptions, HttpDefinition, McpSession, ResolvedServer, ServerDefinition,
    TransportDefinition, TransportKind, TrustCheck, check_trust,
};
use rustman_test_support::McpHttpServer;
use rustman_test_support::mcp_http::{LEGACY_VERSION, SESSION_ID};

const PATIENCE: Duration = Duration::from_secs(30);

fn server(url: &str, headers: &[(&str, &str)], auth: Option<Auth>) -> ResolvedServer {
    let definition = ServerDefinition {
        name: "remote".into(),
        transport: TransportDefinition::Http(HttpDefinition {
            url: url.to_owned(),
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect::<BTreeMap<_, _>>(),
            auth,
            oauth: None,
        }),
        roots: None,
        timeout_ms: None,
        source: PathBuf::from("rustman.toml"),
    };
    definition
        .resolve(&Resolver::new(), Path::new("."))
        .unwrap()
}

async fn connect(server: &ResolvedServer) -> Result<McpSession, ClientError> {
    let dir = tempfile::tempdir().unwrap();
    let store =
        TrustStore::load_with_key(dir.path(), TrustKey::in_dir(dir.path().join("keys"))).unwrap();
    let TrustCheck::Approved(approved) = check_trust(&store, server) else {
        panic!("HTTP servers need no approval");
    };
    tokio::time::timeout(
        PATIENCE,
        McpSession::connect(server, &approved, &ConnectOptions::default()),
    )
    .await
    .expect("connected in time")
}

#[tokio::test(flavor = "multi_thread")]
async fn streamable_http_sends_the_session_id_on_every_follow_up() {
    let fake = McpHttpServer::streamable().await;
    let session = connect(&server(&fake.url(), &[], None)).await.unwrap();
    assert_eq!(session.info().transport, TransportKind::StreamableHttp);
    assert_eq!(session.info().transport.label(), "streamable HTTP");
    assert_eq!(session.info().server_name, "fake-http");
    session.ping().await.unwrap();
    session.peer().list_tools(None).await.unwrap();
    session.shutdown().await;

    let hits = fake.hits();
    let initialize = &hits[0];
    assert_eq!(initialize.method, "POST");
    assert_eq!(initialize.json()["method"], "initialize");
    assert_eq!(initialize.header("mcp-session-id"), None);
    let follow_ups = &hits[1..];
    assert!(follow_ups.len() >= 3, "{follow_ups:?}");
    for hit in follow_ups {
        assert_eq!(
            hit.header("mcp-session-id"),
            Some(SESSION_ID),
            "{} {}",
            hit.method,
            String::from_utf8_lossy(&hit.body)
        );
    }
    let methods: Vec<String> = follow_ups
        .iter()
        .filter(|h| h.method == "POST")
        .map(|h| h.json()["method"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(methods.contains(&"ping".to_owned()), "{methods:?}");
    assert!(methods.contains(&"tools/list".to_owned()), "{methods:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn definition_headers_and_auth_are_sent() {
    let fake = McpHttpServer::streamable().await;
    let auth = Auth::Bearer {
        token: "t0ken".into(),
    };
    let session = connect(&server(&fake.url(), &[("X-Team", "core")], Some(auth)))
        .await
        .unwrap();
    session.shutdown().await;
    let initialize = &fake.hits()[0];
    assert_eq!(initialize.header("x-team"), Some("core"));
    assert_eq!(initialize.header("authorization"), Some("Bearer t0ken"));
    let accept = initialize.header("accept").unwrap();
    assert!(
        accept.contains("application/json") && accept.contains("text/event-stream"),
        "{accept}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_legacy_server_is_reached_over_http_and_sse() {
    let fake = McpHttpServer::legacy().await;
    let session = connect(&server(&fake.url(), &[], None)).await.unwrap();
    assert_eq!(session.info().transport, TransportKind::LegacySse);
    assert_eq!(session.info().transport.to_string(), "legacy SSE");
    assert_eq!(session.info().protocol_version, LEGACY_VERSION);
    session.ping().await.unwrap();
    let tools = session.peer().list_tools(None).await.unwrap();
    assert_eq!(tools.tools[0].name, "echo");
    session.shutdown().await;

    let hits = fake.hits();
    assert_eq!(
        (hits[0].method.as_str(), hits[0].uri.as_str()),
        ("POST", "/mcp")
    );
    assert!(hits.iter().any(|h| h.method == "GET" && h.uri == "/mcp"));
    assert!(
        hits.iter()
            .any(|h| h.method == "POST" && h.uri == "/messages?sessionId=s1"),
        "{hits:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_server_fails_to_connect() {
    let fake = McpHttpServer::streamable().await;
    let url = fake.url().replace("/mcp", "/missing");
    drop(fake);
    let err = connect(&server(&url, &[], None)).await.unwrap_err();
    assert!(
        err.to_string()
            .starts_with("mcp: cannot connect to server remote:"),
        "{err}"
    );
}
