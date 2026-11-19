//! The MCP client over stdio against the scripted fixture server
//! (`tests/support/mcp_fixture.rs`): spawn, env and cwd, stderr capture,
//! unexpected exit, graceful shutdown, trust and version negotiation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rmcp::model::CallToolRequestParams;
use rustman_core::Secret;
use rustman_core::trust::{TrustKey, TrustStore};
use rustman_core::vars::{Layer, Lookup, Resolver, VarSource};
use rustman_mcp::client::{
    CallOptions, ClientError, ConnectOptions, McpSession, Operation, ProcessExit, ResolvedServer,
    ServerDefinition, SessionStatus, StdioDefinition, TransportDefinition, TransportKind,
    require_trust,
};

const FIXTURE: &str = env!("CARGO_BIN_EXE_rustman-mcp-fixture");

/// Generous: process start-up on a loaded CI machine.
const PATIENCE: Duration = Duration::from_secs(30);

/// Variables for the definitions: `secret` is a secret, the rest plain.
struct Vars;

impl VarSource for Vars {
    fn layer(&self) -> Layer {
        Layer::Settings
    }

    fn lookup(&self, name: &str) -> Option<Lookup> {
        match name {
            "secret" => Some(Lookup::Secret(Secret::new(
                "secret",
                "canary-s3cr3t".to_owned(),
            ))),
            "missing_tool" => Some(Lookup::Secret(Secret::new(
                "missing_tool",
                "/nonexistent/canary-s3cr3t".to_owned(),
            ))),
            "plain" => Some(Lookup::Value("plain-value".into())),
            _ => None,
        }
    }
}

fn definition(command: &str, args: &[&str], env: &[(&str, &str)]) -> ServerDefinition {
    ServerDefinition {
        name: "fixture".into(),
        transport: TransportDefinition::Stdio(StdioDefinition {
            command: command.to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            env: env
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect::<BTreeMap<_, _>>(),
            cwd: None,
        }),
        roots: None,
        timeout_ms: None,
        source: PathBuf::from("rustman.toml"),
    }
}

fn resolve(definition: &ServerDefinition, root: &Path) -> ResolvedServer {
    let resolver = Resolver::new().with(Vars);
    definition.resolve(&resolver, root).unwrap()
}

async fn connect(server: &ResolvedServer, root: &Path) -> Result<McpSession, ClientError> {
    connect_with(server, root, &ConnectOptions::default()).await
}

async fn connect_with(
    server: &ResolvedServer,
    root: &Path,
    options: &ConnectOptions,
) -> Result<McpSession, ClientError> {
    let mut store = TrustStore::load_with_key(root, TrustKey::in_dir(root.join("keys"))).unwrap();
    let approved = require_trust(&mut store, server, true)?;
    tokio::time::timeout(PATIENCE, McpSession::connect(server, &approved, options))
        .await
        .expect("connected in time")
}

async fn call_text(session: &McpSession, tool: &str, arguments: serde_json::Value) -> String {
    let arguments = arguments.as_object().cloned().unwrap_or_default();
    let result = session
        .peer()
        .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(arguments))
        .await
        .unwrap();
    result.content[0].as_text().unwrap().text.clone()
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a.canonicalize().unwrap() == b.canonicalize().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn stderr_is_captured_without_corrupting_the_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let server = resolve(&definition(FIXTURE, &["--stderr", "3"], &[]), dir.path());
    let session = connect(&server, dir.path()).await.unwrap();
    session.ping().await.unwrap();
    assert!(
        !call_text(&session, "cwd", serde_json::json!({}))
            .await
            .is_empty()
    );
    assert_eq!(session.status(), SessionStatus::Connected);
    let log = session.stderr().unwrap().clone();
    let exit = session.shutdown().await.unwrap();
    assert_eq!(
        exit,
        ProcessExit {
            code: Some(0),
            killed: false
        }
    );
    // The exit is reported once stderr is drained, so the log is complete:
    // 3 lines at start and 3 before each of the 3 answers.
    let lines = log.lines();
    assert_eq!(lines.len(), 12, "{lines:?}");
    assert!(
        lines[0].starts_with("warning 0: fixture noise"),
        "{lines:?}"
    );
}

/// The child sees the declared `env` and a few launcher variables (`PATH`,
/// `HOME`...), never the rest of Rustman's environment: a variable cargo
/// sets for this test (as a token in the user's shell would be) is absent.
#[tokio::test(flavor = "multi_thread")]
async fn env_is_an_allowlist_plus_the_definition_and_cwd_defaults_to_the_root() {
    let dir = tempfile::tempdir().unwrap();
    let server = resolve(
        &definition(FIXTURE, &[], &[("FIXTURE_VAR", "{{plain}}")]),
        dir.path(),
    );
    let session = connect(&server, dir.path()).await.unwrap();
    let value = call_text(&session, "env", serde_json::json!({"name": "FIXTURE_VAR"})).await;
    assert_eq!(value, "plain-value");
    let path = call_text(&session, "env", serde_json::json!({"name": "PATH"})).await;
    assert!(!path.is_empty(), "PATH is inherited");
    let unrelated = "CARGO_PKG_NAME";
    assert!(
        std::env::var_os(unrelated).is_some(),
        "cargo sets {unrelated}"
    );
    let leaked = call_text(&session, "env", serde_json::json!({"name": unrelated})).await;
    assert_eq!(leaked, "", "{unrelated} must not reach the server");
    let cwd = call_text(&session, "cwd", serde_json::json!({})).await;
    assert!(same_dir(Path::new(&cwd), dir.path()), "{cwd}");
    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unexpected_exit_reports_the_code_and_the_last_50_stderr_lines() {
    let dir = tempfile::tempdir().unwrap();
    let server = resolve(
        &definition(FIXTURE, &["--exit-after-init", "1"], &[]),
        dir.path(),
    );
    let mut session = connect(&server, dir.path()).await.unwrap();
    let status = tokio::time::timeout(PATIENCE, session.disconnected())
        .await
        .expect("the exit is noticed");
    let SessionStatus::Disconnected { exit, stderr_tail } = status else {
        panic!("expected a disconnection, got {status:?}");
    };
    assert_eq!(
        exit,
        Some(ProcessExit {
            code: Some(1),
            killed: false
        })
    );
    assert_eq!(stderr_tail.len(), 50);
    assert_eq!(stderr_tail[0], "exit line 11");
    assert_eq!(stderr_tail[49], "exit line 60");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_ignoring_eof_is_killed_after_the_grace_period() {
    let dir = tempfile::tempdir().unwrap();
    let server = resolve(&definition(FIXTURE, &["--ignore-eof"], &[]), dir.path());
    let options = ConnectOptions {
        shutdown_grace: Duration::from_millis(300),
        ..ConnectOptions::default()
    };
    let session = connect_with(&server, dir.path(), &options).await.unwrap();
    let exit = tokio::time::timeout(PATIENCE, session.shutdown())
        .await
        .expect("stopped in time");
    assert_eq!(
        exit,
        Some(ProcessExit {
            code: None,
            killed: true
        })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_spawned_without_approval() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("spawned");
    let marker_arg = marker.to_str().unwrap();
    let server = resolve(
        &definition(FIXTURE, &["--marker", marker_arg], &[]),
        dir.path(),
    );
    let mut store =
        TrustStore::load_with_key(dir.path(), TrustKey::in_dir(dir.path().join("keys"))).unwrap();
    let err = require_trust(&mut store, &server, false).unwrap_err();
    assert_eq!(
        err.to_string(),
        "mcp: server fixture is not trusted; review it or pass --trust"
    );
    let other = resolve(&definition(FIXTURE, &[], &[]), dir.path());
    let approved = require_trust(&mut store, &other, true).unwrap();
    let err = McpSession::connect(&server, &approved, &ConnectOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Untrusted(_)), "{err}");
    assert!(!marker.exists(), "an untrusted server must not be spawned");
}

#[tokio::test(flavor = "multi_thread")]
async fn initialization_shows_the_server_and_negotiated_version() {
    let dir = tempfile::tempdir().unwrap();
    let server = resolve(&definition(FIXTURE, &[], &[]), dir.path());
    let session = connect(&server, dir.path()).await.unwrap();
    let info = session.info();
    assert_eq!(info.server_name, "fixture");
    assert_eq!(info.server_version, "1.2.3");
    assert_eq!(info.server_title.as_deref(), Some("Fixture server"));
    assert_eq!(info.protocol_version, "2025-11-25");
    assert_eq!(info.instructions.as_deref(), Some("Use the env tool."));
    assert_eq!(info.transport, TransportKind::Stdio);
    assert!(info.capabilities.check(Operation::GetPrompt).is_ok());
    let meta = session.peer().peer_info().unwrap().meta.clone().unwrap();
    assert_eq!(meta["client"]["name"], "rustman");
    assert_eq!(meta["client"]["version"], rustman_core::VERSION);
    let capabilities = &meta["clientCapabilities"];
    for capability in ["roots", "sampling", "elicitation"] {
        assert!(capabilities.get(capability).is_some(), "{capabilities}");
    }
    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_older_supported_version_is_used() {
    let dir = tempfile::tempdir().unwrap();
    let server = resolve(
        &definition(FIXTURE, &["--version", "2025-03-26"], &[]),
        dir.path(),
    );
    let session = connect(&server, dir.path()).await.unwrap();
    assert_eq!(session.info().protocol_version, "2025-03-26");
    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unsupported_version_closes_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let server = resolve(
        &definition(FIXTURE, &["--version", "1999-01-01"], &[]),
        dir.path(),
    );
    let err = connect(&server, dir.path()).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "mcp: unsupported protocol version 1999-01-01"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn undeclared_prompts_disable_prompt_operations() {
    let dir = tempfile::tempdir().unwrap();
    let server = resolve(&definition(FIXTURE, &["--no-prompts"], &[]), dir.path());
    let session = connect(&server, dir.path()).await.unwrap();
    let disabled = session
        .info()
        .capabilities
        .check(Operation::ListPrompts)
        .unwrap_err();
    assert_eq!(disabled.reason, "server does not declare prompts");
    assert!(
        session
            .info()
            .capabilities
            .check(Operation::ListTools)
            .is_ok()
    );
    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn secrets_never_reach_stderr_logs_or_errors() {
    let dir = tempfile::tempdir().unwrap();
    let server = resolve(
        &definition(
            FIXTURE,
            &["--echo-env", "TOKEN"],
            &[("TOKEN", "{{secret}}")],
        ),
        dir.path(),
    );
    let session = connect(&server, dir.path()).await.unwrap();
    let log = session.stderr().unwrap().clone();
    session.shutdown().await;
    let lines = log.lines();
    assert!(lines.iter().any(|l| l.starts_with("TOKEN=")), "{lines:?}");
    assert!(
        lines.iter().all(|l| !l.contains("canary-s3cr3t")),
        "{lines:?}"
    );

    let missing = resolve(&definition("{{missing_tool}}", &[], &[]), dir.path());
    let err = connect(&missing, dir.path()).await.unwrap_err();
    let message = err.to_string();
    assert!(
        message.starts_with("mcp: cannot start server fixture:"),
        "{message}"
    );
    assert!(!message.contains("canary-s3cr3t"), "{message}");
    assert!(!format!("{err:?}").contains("canary-s3cr3t"));
}

/// Canary: a secret the server echoes back never reaches a tool result, a
/// structured result, the notifications stream or an error.
#[tokio::test(flavor = "multi_thread")]
async fn secrets_never_reach_results_or_notifications() {
    let dir = tempfile::tempdir().unwrap();
    let server = resolve(
        &definition(FIXTURE, &[], &[("TOKEN", "{{secret}}")]),
        dir.path(),
    );
    let session = connect(&server, dir.path()).await.unwrap();
    let options = CallOptions::default();
    let echoed = session
        .call_tool("env", serde_json::json!({"name": "TOKEN"}), &options)
        .await
        .unwrap();
    let logged = session
        .call_tool(
            "log",
            serde_json::json!({"text": "token canary-s3cr3t"}),
            &options,
        )
        .await
        .unwrap();
    let counted = session
        .call_tool(
            "count",
            serde_json::json!({"value": "canary-s3cr3t"}),
            &options,
        )
        .await
        .unwrap();
    let failed = session
        .call_tool(
            "nope canary-s3cr3t",
            serde_json::json!({}),
            &CallOptions {
                validate: false,
                ..options
            },
        )
        .await
        .unwrap_err();
    let shown = format!(
        "{:?} {:?} {:?} {:?} {failed} {:?}",
        echoed.result,
        logged.notifications,
        counted.result,
        session.notifications().entries(),
        failed
    );
    assert!(shown.contains("«redacted:secret»"), "{shown}");
    assert!(!shown.contains("canary-s3cr3t"), "{shown}");
    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_never_initializes_times_out_and_is_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let mut silent = definition(FIXTURE, &["--mute"], &[]);
    silent.timeout_ms = Some(500);
    let server = resolve(&silent, dir.path());
    let options = ConnectOptions {
        shutdown_grace: Duration::from_millis(100),
        ..ConnectOptions::default()
    };
    let err = connect_with(&server, dir.path(), &options)
        .await
        .expect_err("a mute server cannot be connected to");
    assert_eq!(err.to_string(), "mcp: request timed out after 500ms");
}

/// The connect options' timeout (the CLI's `--timeout`) bounds
/// `initialize` too, above the server's own `timeout_ms`.
#[tokio::test(flavor = "multi_thread")]
async fn the_connect_timeout_bounds_initialize() {
    let dir = tempfile::tempdir().unwrap();
    let mut silent = definition(FIXTURE, &["--mute"], &[]);
    silent.timeout_ms = Some(600_000);
    let server = resolve(&silent, dir.path());
    let options = ConnectOptions {
        shutdown_grace: Duration::from_millis(100),
        timeout: Some(Duration::from_millis(700)),
        ..ConnectOptions::default()
    };
    let err = connect_with(&server, dir.path(), &options)
        .await
        .expect_err("a mute server cannot be connected to");
    assert_eq!(err.to_string(), "mcp: request timed out after 700ms");
}

/// A message line over 16 MiB ends the session with a clear error rather
/// than being buffered (or silently dropped, leaving the call to time out).
#[tokio::test(flavor = "multi_thread")]
async fn an_oversized_message_closes_the_session_with_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let server = resolve(&definition(FIXTURE, &[], &[]), dir.path());
    let session = connect(&server, dir.path()).await.unwrap();
    let options = CallOptions {
        validate: false,
        ..CallOptions::default()
    };
    let call = session.call_tool("huge", serde_json::json!({}), &options);
    let err = tokio::time::timeout(PATIENCE, call)
        .await
        .expect("the call ends at once")
        .expect_err("the answer is too large");
    assert_eq!(
        err.to_string(),
        "mcp: server fixture: the server sent a message larger than 16 MiB; \
         the session was closed"
    );
    let mut closed = false;
    for _ in 0..100 {
        closed = matches!(session.status(), SessionStatus::Disconnected { .. });
        if closed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(closed, "the session is closed");
    session.shutdown().await;
}

/// The size of the heartbeat file, once it exists.
async fn beats(path: &Path) -> u64 {
    for _ in 0..600 {
        if let Ok(meta) = std::fs::metadata(path) {
            return meta.len();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the grandchild never started");
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_kills_the_processes_a_launcher_started() {
    let dir = tempfile::tempdir().unwrap();
    let heartbeat = dir.path().join("heartbeat");
    let beat_arg = heartbeat.to_str().unwrap();
    let server = resolve(
        &definition(FIXTURE, &["--grandchild", beat_arg, "--ignore-eof"], &[]),
        dir.path(),
    );
    let options = ConnectOptions {
        shutdown_grace: Duration::from_millis(300),
        ..ConnectOptions::default()
    };
    let session = connect_with(&server, dir.path(), &options).await.unwrap();
    beats(&heartbeat).await;
    let exit = tokio::time::timeout(PATIENCE, session.shutdown())
        .await
        .expect("stopped in time");
    assert_eq!(exit.map(|e| e.killed), Some(true));
    // Let a beat already under way land, then the file must stay still.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let stopped = beats(&heartbeat).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        beats(&heartbeat).await,
        stopped,
        "the grandchild still runs"
    );
}
