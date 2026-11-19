//! The MCP inspector (spec: TUI MCP inspector pane), driven through
//! `update` with keys; rendered with ratatui's `TestBackend`. Jobs run
//! synchronously the way the effect runner runs them; the live-session
//! test runs a real session thread against the in-process fake server.

use std::collections::VecDeque;
use std::path::Path;
use std::time::Duration;

use rustman_core::Workspace;
use rustman_core::model::{McpOperation, Request};
use rustman_mcp::client::info::ResourceCapabilities;
use rustman_mcp::client::{
    ApprovalBroker, Capabilities, ElicitationDecision, ElicitationRequest, InteractiveBroker,
    SamplingDecision, SamplingRequest, ServerTarget, SessionInfo, TransportKind,
};
use rustman_test_support::McpHttpServer;
use serde_json::json;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use super::{Command, Conn, Focus, Job, Lists, Live, Msg, ServerRow, Tab};
use crate::app::{Action, Effect, Overlay, Screen, Session, State, update};
use crate::effects::execute;
use crate::snapshot::{buffer_text, render};
use crate::workbench_tests::event;

fn scratch() -> State {
    let mut state = State::with_screen(Screen::Shell(Session::Scratch));
    update(
        &mut state,
        Action::Resize {
            width: 110,
            height: 32,
        },
    );
    state
}

/// A shell over a workspace at `<tmp>/ws` defining `servers`.
fn workspace(servers: &str) -> (tempfile::TempDir, State) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("rustman.toml"),
        format!("[workspace]\nname = \"mcp\"\nversion = 1\n{servers}"),
    )
    .unwrap();
    std::fs::write(root.join(".gitignore"), ".rustman/\n").unwrap();
    let ws = Workspace::load(&root).unwrap();
    let mut state = scratch();
    state.screen = Screen::Shell(Session::Workspace(Box::new(ws)));
    state.mcp.trust_key = rustman_core::trust::TrustKey::in_dir(tmp.path().join("keys"));
    (tmp, state)
}

fn keys(state: &mut State, keys: &str) -> Vec<Effect> {
    let mut effects = Vec::new();
    for key in keys.split(' ') {
        effects = update(state, Action::Key(event(key)));
    }
    effects
}

fn type_text(state: &mut State, text: &str) {
    for c in text.chars() {
        let key = if c == ' ' {
            "Space".to_owned()
        } else {
            c.to_string()
        };
        update(state, Action::Key(event(&key)));
    }
}

fn msg(state: &mut State, msg: Msg) -> Vec<Effect> {
    update(state, Action::Mcp(Box::new(msg)))
}

fn run(state: &mut State, mut effects: Vec<Effect>) {
    while let Some(effect) = effects.pop() {
        let done = execute(effect);
        effects.extend(update(state, done));
    }
}

fn info(prompts: bool) -> SessionInfo {
    SessionInfo {
        server_name: "fake-files".into(),
        server_version: "1.2.0".into(),
        server_title: None,
        protocol_version: "2025-06-18".into(),
        instructions: None,
        capabilities: Capabilities {
            tools: Some(false),
            resources: Some(ResourceCapabilities {
                subscribe: true,
                list_changed: false,
            }),
            prompts: prompts.then_some(false),
            logging: true,
            completions: false,
        },
        transport: TransportKind::Stdio,
    }
}

fn lists() -> Lists {
    Lists {
        tools: serde_json::from_value(json!([
            {"name": "search", "annotations": {"readOnlyHint": true}, "inputSchema": {
                "type": "object",
                "required": ["q"],
                "properties": {
                    "q": {"type": "string", "description": "What to find"},
                    "mode": {"enum": ["fast", "safe"]}
                }
            }},
            {"name": "delete", "annotations": {"destructiveHint": true}, "inputSchema": {"type": "object"}}
        ]))
        .unwrap(),
        resources: serde_json::from_value(json!([{"uri": "file:///README.md", "name": "README"}]))
            .unwrap(),
        templates: serde_json::from_value(json!([{"uriTemplate": "file:///{path}", "name": "files"}]))
            .unwrap(),
        prompts: Vec::new(),
    }
}

/// The pane open on server `files`, connected, with the commands the
/// session thread would receive.
fn connected(state: &mut State, prompts: bool) -> UnboundedReceiver<Command> {
    let (commands, receiver) = unbounded_channel();
    state.mcp.open = true;
    state.mcp.servers = vec![
        ServerRow {
            name: "files".into(),
            transport: "stdio",
            conn: Conn::Connected,
        },
        ServerRow {
            name: "remote".into(),
            transport: "http",
            conn: Conn::Idle,
        },
    ];
    state.mcp.session = Some(Live {
        server: "files".into(),
        info: info(prompts),
        lists: lists(),
        notifications: VecDeque::new(),
        stderr: VecDeque::from(["warning: slow disk".to_owned()]),
        reread: None,
        level: None,
        busy: None,
        commands,
    });
    state.mcp.focus = Focus::List;
    receiver
}

#[test]
fn the_inspector_shows_servers_info_tabs_and_the_tool_form() {
    let mut state = scratch();
    let _commands = connected(&mut state, true);
    keys(&mut state, "Enter");
    assert_eq!(state.mcp.focus, Focus::Form);
    let text = buffer_text(&render(&state, 110, 32));
    assert!(
        text.contains("fake-files 1.2.0 · stdio · 2025-06-18"),
        "{text}"
    );
    assert!(
        text.contains("1 Tools  2 Resources  3 Prompts  4 Notifications  5 Stderr"),
        "{text}"
    );
    assert!(text.contains("search [read-only]"), "{text}");
    // Spec: Enum argument -- a picker with exactly `fast` and `safe`.
    assert!(text.contains("mode  one of  fast | safe"), "{text}");
    insta::assert_snapshot!("mcp_inspector_tool_form", text);
}

#[test]
fn undeclared_prompts_are_disabled_with_the_reason() {
    let mut state = scratch();
    let _commands = connected(&mut state, false);
    keys(&mut state, "3");
    assert_eq!(state.mcp.tab, Tab::Prompts);
    let text = buffer_text(&render(&state, 110, 32));
    assert!(text.contains("server does not declare prompts"), "{text}");
}

#[test]
fn a_form_call_sends_the_arguments_and_shows_the_result() {
    let mut state = scratch();
    let mut commands = connected(&mut state, true);
    keys(&mut state, "Enter");
    type_text(&mut state, "rust");
    keys(&mut state, "Down Right Right Enter");
    let Ok(Command::Perform { op, validate, .. }) = commands.try_recv() else {
        panic!("a call was sent");
    };
    assert!(validate);
    let rustman_mcp::client::McpOp::ToolsCall { name, arguments } = &op else {
        panic!("{op:?}");
    };
    assert_eq!(name, "search");
    assert_eq!(arguments, &json!({"q": "rust", "mode": "safe"}));
    let busy = buffer_text(&render(&state, 110, 32));
    assert!(busy.contains("tools/call search…"), "{busy}");
    msg(
        &mut state,
        Msg::Performed(Box::new(super::Performed {
            title: "tools/call search".into(),
            op,
            result: Err("mcp: protocol error -32602: bad".into()),
        })),
    );
    let text = buffer_text(&render(&state, 110, 32));
    assert!(
        text.contains("tools/call search  protocol error -32602"),
        "{text}"
    );
}

#[test]
fn invalid_arguments_can_be_sent_after_an_explicit_confirm() {
    let mut state = scratch();
    let mut commands = connected(&mut state, true);
    keys(&mut state, "Enter Enter");
    let Ok(Command::Perform { op, .. }) = commands.try_recv() else {
        panic!("a call was sent");
    };
    msg(
        &mut state,
        Msg::Performed(Box::new(super::Performed {
            title: "tools/call search".into(),
            op,
            result: Err("mcp: invalid arguments: /q: required property missing".into()),
        })),
    );
    let text = buffer_text(&render(&state, 160, 32));
    assert!(text.contains("/q: required property missing"), "{text}");
    assert!(text.contains("Ctrl+O sends the arguments anyway"), "{text}");
    keys(&mut state, "Ctrl+O");
    let Ok(Command::Perform { validate, .. }) = commands.try_recv() else {
        panic!("the call was sent again");
    };
    assert!(!validate, "the override skips validation");
}

#[test]
fn the_user_cancels_a_call_in_progress() {
    let mut state = scratch();
    let mut commands = connected(&mut state, true);
    keys(&mut state, "2 Enter");
    let Ok(Command::Perform { cancel, .. }) = commands.try_recv() else {
        panic!("a read was sent");
    };
    msg(
        &mut state,
        Msg::Progress(rustman_mcp::client::Progress {
            progress: 40.0,
            total: Some(100.0),
            message: None,
        }),
    );
    let text = buffer_text(&render(&state, 110, 32));
    assert!(
        text.contains("resources/read file:///README.md… 40%"),
        "{text}"
    );
    keys(&mut state, "Ctrl+C");
    assert!(cancel.is_cancelled());
}

fn sampling() -> SamplingRequest {
    SamplingRequest {
        server: "files".into(),
        label: "1 message".into(),
        params: serde_json::from_value(json!({
            "messages": [{"role": "user", "content": {"type": "text", "text": "Summarize the file"}}],
            "systemPrompt": "Be brief.",
            "maxTokens": 100
        }))
        .unwrap(),
    }
}

/// A sampling request queued by the session's broker, and its answer.
fn ask_sampling(state: &mut State) -> impl Future<Output = SamplingDecision> + use<> {
    let (broker, mut approvals) = InteractiveBroker::new();
    let answer = broker.sampling(sampling());
    let approval = approvals.try_recv().unwrap();
    msg(state, Msg::Approval(approval));
    answer
}

#[tokio::test]
async fn a_pending_approval_is_a_badge_until_it_is_handled() {
    let mut state = scratch();
    let _commands = connected(&mut state, true);
    state.mcp.open = false;
    let answer = ask_sampling(&mut state);
    let text = buffer_text(&render(&state, 110, 32));
    assert!(text.contains(" MCP APPROVAL (1) "), "{text}");
    insta::assert_snapshot!("mcp_pending_approval_badge", text);
    keys(&mut state, "Alt+I");
    let dialog = buffer_text(&render(&state, 110, 32));
    assert!(dialog.contains("files asks to sample an LLM"), "{dialog}");
    assert!(dialog.contains("user: Summarize the file"), "{dialog}");
    assert!(dialog.contains("System prompt: Be brief."), "{dialog}");
    assert!(dialog.contains("Max tokens: 100"), "{dialog}");
    insta::assert_snapshot!("mcp_sampling_dialog", dialog);
    type_text(&mut state, "A short summary");
    assert!(
        state.status.mcp.is_some(),
        "nothing is answered while typing"
    );
    keys(&mut state, "Ctrl+S");
    assert_eq!(answer.await, SamplingDecision::typed("A short summary"));
    assert!(state.status.mcp.is_none());
    assert!(!buffer_text(&render(&state, 110, 32)).contains("MCP APPROVAL"));
}

#[tokio::test]
async fn a_rejected_sampling_request_returns_the_rejection() {
    let mut state = scratch();
    let _commands = connected(&mut state, true);
    let answer = ask_sampling(&mut state);
    keys(&mut state, "Esc");
    assert_eq!(answer.await, SamplingDecision::Reject);
}

fn ask_email(state: &mut State) -> impl Future<Output = ElicitationDecision> + use<> {
    let (broker, mut approvals) = InteractiveBroker::new();
    let answer = broker.elicitation(ElicitationRequest {
        server: "files".into(),
        message: "Your email?".into(),
        schema: Some(json!({
            "type": "object",
            "required": ["email"],
            "properties": {"email": {"type": "string", "format": "email"}}
        })),
        url: None,
    });
    msg(state, Msg::Approval(approvals.try_recv().unwrap()));
    answer
}

#[tokio::test]
async fn an_elicitation_is_accepted_only_with_valid_content() {
    let mut state = scratch();
    let _commands = connected(&mut state, true);
    let answer = ask_email(&mut state);
    type_text(&mut state, "not-an-email");
    keys(&mut state, "Ctrl+S");
    let text = buffer_text(&render(&state, 110, 32));
    assert!(text.contains("Your email?"), "{text}");
    assert!(text.contains("/email"), "{text}");
    assert_eq!(state.mcp.approvals.len(), 1, "still waiting");
    for _ in 0.."not-an-email".len() {
        keys(&mut state, "Backspace");
    }
    type_text(&mut state, "ada@example.com");
    keys(&mut state, "Ctrl+S");
    let ElicitationDecision::Accept(accepted) = answer.await else {
        panic!("accepted");
    };
    assert_eq!(accepted.content(), &json!({"email": "ada@example.com"}));
}

#[tokio::test]
async fn an_elicitation_can_be_declined_or_cancelled() {
    let mut state = scratch();
    let _commands = connected(&mut state, true);
    let declined = ask_email(&mut state);
    keys(&mut state, "Ctrl+D");
    assert_eq!(declined.await, ElicitationDecision::Decline);
    let cancelled = ask_email(&mut state);
    keys(&mut state, "Esc");
    assert_eq!(cancelled.await, ElicitationDecision::Cancel);
}

#[test]
fn a_new_stdio_server_shows_its_command_line_before_anything_runs() {
    let (_tmp, mut state) = workspace(
        "[mcp_servers.files]\ncommand = \"npx\"\nargs = [\"-y\", \"@modelcontextprotocol/server-filesystem\", \".\"]\n",
    );
    let effects = keys(&mut state, "Alt+I");
    run(&mut state, effects);
    assert_eq!(state.mcp.servers.len(), 1);
    assert_eq!(state.mcp.servers[0].transport, "stdio");
    let effects = keys(&mut state, "Enter");
    let [Effect::Mcp(job)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert!(matches!(**job, Job::Prepare { .. }));
    let done = execute(effects[0].clone());
    let effects = update(&mut state, done);
    assert!(effects.is_empty(), "nothing is spawned yet: {effects:?}");
    let text = buffer_text(&render(&state, 110, 32));
    assert!(
        text.contains("npx -y @modelcontextprotocol/server-filesystem ."),
        "{text}"
    );
    assert!(text.contains("y run it and remember"), "{text}");
    let effects = keys(&mut state, "n");
    assert!(effects.is_empty());
    assert!(state.mcp.trust.is_none());
    assert_eq!(state.mcp.servers[0].conn, Conn::Idle);
}

/// The trust dialog shows each variable the command gets as `KEY=value`,
/// the value of one named like a secret redacted.
#[test]
fn the_trust_dialog_shows_env_values_with_secrets_redacted() {
    let (_tmp, mut state) = workspace(
        "[mcp_servers.files]\ncommand = \"npx\"\nargs = [\"server\"]\n\
         env = { MODE = \"fast\", API_TOKEN = \"literal-token-value\" }\n",
    );
    let effects = keys(&mut state, "Alt+I");
    run(&mut state, effects);
    let effects = keys(&mut state, "Enter");
    let done = execute(effects[0].clone());
    update(&mut state, done);
    // The working directory is a temporary folder: shown as a fixed one.
    state.mcp.trust.as_mut().unwrap().pending.cwd = "/work/files".into();
    let text = buffer_text(&render(&state, 110, 32));
    assert!(text.contains("  MODE=fast"), "{text}");
    assert!(text.contains("  API_TOKEN=«redacted:API_TOKEN»"), "{text}");
    assert!(!text.contains("literal-token-value"), "{text}");
    insta::assert_snapshot!("mcp_trust_dialog", text);
}

#[test]
fn approving_a_command_records_it_and_connects() {
    let (_tmp, mut state) = workspace("[mcp_servers.files]\ncommand = \"npx\"\n");
    let effects = keys(&mut state, "Alt+I");
    run(&mut state, effects);
    let effects = keys(&mut state, "Enter");
    let done = execute(effects[0].clone());
    update(&mut state, done);
    let effects = keys(&mut state, "y");
    let [Effect::Mcp(job)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert!(matches!(**job, Job::Trust { .. }));
    let done = execute(effects[0].clone());
    let effects = update(&mut state, done);
    let [Effect::Mcp(job)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert!(matches!(**job, Job::Connect { .. }));
    let root = crate::workbench::workspace(&state).unwrap().root.clone();
    let trust = std::fs::read_to_string(root.join(".rustman/trust.yaml")).unwrap();
    assert!(trust.contains("mcp/files"), "{trust}");
}

#[test]
fn the_last_call_is_saved_as_a_kind_mcp_request() {
    let (_tmp, mut state) = workspace("");
    let mut commands = connected(&mut state, true);
    keys(&mut state, "Enter");
    type_text(&mut state, "rust");
    keys(&mut state, "Enter");
    let Ok(Command::Perform { op, .. }) = commands.try_recv() else {
        panic!("a call was sent");
    };
    let done = rustman_mcp::client::OpResult {
        result: json!({"content": [{"type": "text", "text": "found"}], "isError": false}),
        outcome: "ok",
        is_error: false,
        output_violations: None,
        overridden: Vec::new(),
        capture_body: "found".into(),
        text: "found\n".into(),
        notifications: Vec::new(),
        elapsed: Duration::from_millis(5),
    };
    msg(
        &mut state,
        Msg::Performed(Box::new(super::Performed {
            title: "tools/call search".into(),
            op,
            result: Ok(Box::new(done)),
        })),
    );
    keys(&mut state, "Ctrl+S");
    let Some(Overlay::Input(input)) = &state.overlay else {
        panic!("a file name prompt");
    };
    assert_eq!(input.text.text(), "search.rman.yaml");
    let effects = keys(&mut state, "Enter");
    run(&mut state, effects);
    let root = crate::workbench::workspace(&state).unwrap().root.clone();
    let text = std::fs::read_to_string(root.join("search.rman.yaml")).unwrap();
    let loaded = rustman_core::model::KindRegistry::default()
        .load(Path::new("search.rman.yaml"), &text)
        .unwrap();
    let Request::Mcp(request) = loaded else {
        panic!("{text}");
    };
    assert_eq!(request.server, "files");
    assert_eq!(request.operation, McpOperation::ToolsCall);
    assert_eq!(request.params()["arguments"], json!({"q": "rust"}));
}

#[test]
fn notifications_and_stderr_have_their_tabs() {
    let mut state = scratch();
    let _commands = connected(&mut state, true);
    let note = rustman_mcp::client::Notification {
        at: std::time::UNIX_EPOCH + Duration::from_secs(45_296),
        kind: rustman_mcp::client::NotificationKind::ResourceUpdated {
            uri: "file:///README.md".into(),
            subscribed: true,
        },
    };
    msg(&mut state, Msg::Notification(Box::new(note)));
    keys(&mut state, "4");
    let text = buffer_text(&render(&state, 110, 32));
    assert!(
        text.contains("12:34:56 resource updated file:///README.md  (r: read again)"),
        "{text}"
    );
    keys(&mut state, "5");
    let text = buffer_text(&render(&state, 110, 32));
    assert!(text.contains("warning: slow disk"), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_session_lists_and_calls_tools() {
    let server = McpHttpServer::streamable().await;
    let dir = tempfile::tempdir().unwrap();
    let target = ServerTarget::Url(server.url());
    let (resolved, approved) = target
        .resolve(
            dir.path(),
            false,
            &rustman_core::vars::Resolver::new(),
            false,
        )
        .unwrap();
    let (actions, mut received) = unbounded_channel();
    super::worker::spawn_session(resolved, approved, None, actions);
    let mut state = scratch();
    state.mcp.open = true;
    let name = server_name(&server.url());
    state.mcp.servers = vec![ServerRow {
        name: name.clone(),
        transport: "http",
        conn: Conn::Connecting,
    }];
    let connected = next(&mut received).await;
    update(&mut state, connected);
    assert_eq!(state.mcp.servers[0].conn, Conn::Connected);
    let live = state.mcp.session.as_ref().unwrap();
    assert_eq!(live.info.transport, TransportKind::StreamableHttp);
    assert_eq!(live.lists.tools[0].name, "echo");
    keys(&mut state, "Enter");
    keys(&mut state, "Ctrl+J");
    state.mcp.form.as_mut().unwrap().raw = Some(r#"{"text":"hi there"}"#.into());
    keys(&mut state, "Enter");
    loop {
        let action = next(&mut received).await;
        let performed = matches!(&action, Action::Mcp(m) if matches!(**m, Msg::Performed(_)));
        update(&mut state, action);
        if performed {
            break;
        }
    }
    let result = state.mcp.result.as_ref().unwrap();
    assert_eq!(result.label, "ok");
    assert_eq!(result.lines, ["hi there"]);
    super::shut_down(&mut state);
}

/// The next action of the session thread (generous: CI machines are slow).
async fn next(received: &mut UnboundedReceiver<Action>) -> Action {
    tokio::time::timeout(Duration::from_secs(30), received.recv())
        .await
        .expect("an action in time")
        .expect("the session thread is alive")
}

fn server_name(url: &str) -> String {
    ServerTarget::Url(url.to_owned()).ad_hoc().unwrap().name
}

/// A browser for the OAuth test: follows the authorization URL to its
/// redirect, then delivers it to the loopback listener.
fn follow_login(url: &str) {
    let url = url.to_owned();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let (status, location) = rustman_test_support::mcp_oauth::browse(&url).await;
            assert_eq!(status, 302);
            rustman_test_support::mcp_oauth::browse(&location.expect("a redirect")).await;
        });
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connecting_to_an_oauth_server_logs_in_through_the_browser() {
    let server = McpHttpServer::oauth(true).await;
    let dir = tempfile::tempdir().unwrap();
    let target = ServerTarget::Url(server.url());
    let (resolved, approved) = target
        .resolve(
            dir.path(),
            false,
            &rustman_core::vars::Resolver::new(),
            false,
        )
        .unwrap();
    let (actions, mut received) = unbounded_channel();
    let oauth = super::worker::oauth_options(
        Some(dir.path()),
        Some(rustman_core::SecretBackend::File),
        &actions,
        follow_login,
    );
    assert!(oauth.is_some(), "a workspace has a token store");
    super::worker::spawn_session(resolved, approved, oauth, actions);
    let mut state = scratch();
    state.mcp.open = true;
    let name = server_name(&server.url());
    state.mcp.servers = vec![ServerRow {
        name,
        transport: "http",
        conn: Conn::Connecting,
    }];
    let login = next(&mut received).await;
    let Action::Mcp(msg) = &login else {
        panic!("{login:?}");
    };
    let Msg::Login(url) = &**msg else {
        panic!("expected the login URL first, got {msg:?}");
    };
    assert!(url.contains("code_challenge_method=S256"), "{url}");
    update(&mut state, login);
    let connected = next(&mut received).await;
    update(&mut state, connected);
    assert_eq!(state.mcp.servers[0].conn, Conn::Connected);
    assert_eq!(server.grants(), ["authorization_code"]);
    super::shut_down(&mut state);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_saved_mcp_request_is_sent_from_the_workbench() {
    let server = McpHttpServer::streamable().await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("rustman.toml"),
        format!(
            "[workspace]\nname = \"mcp\"\nversion = 1\n\n[mcp_servers.remote]\nurl = \"{}\"\n",
            server.url()
        ),
    )
    .unwrap();
    std::fs::write(root.join(".gitignore"), ".rustman/\n").unwrap();
    std::fs::write(
        root.join("echo.rman.yaml"),
        "version: 1\nkind: mcp\nname: Echo\nserver: remote\noperation: tools/call\n\
         params:\n  name: echo\n  arguments:\n    text: hi there\n\
         assertions:\n  - content_text_contains: hi there\n",
    )
    .unwrap();
    let ws = Workspace::load(&root).unwrap();
    let mut state = scratch();
    state.screen = Screen::Shell(Session::Workspace(Box::new(ws)));
    state.secret_backend = Some(rustman_core::SecretBackend::File);
    crate::workbench::open_path(&mut state, Path::new("echo.rman.yaml"));
    let effects = crate::send::start(&mut state);
    let [Effect::Send(job)] = effects.as_slice() else {
        panic!("expected a send, got {effects:?}");
    };
    let job = job.clone();
    let done = tokio::task::spawn_blocking(move || crate::send::execute(&job))
        .await
        .unwrap();
    update(&mut state, done);
    let tab = state.tabs.active().unwrap();
    let record = &tab.exchange.as_ref().expect("a result").record;
    assert_eq!(
        record.status,
        rustman_core::run::RequestStatus::Passed,
        "{record:?}"
    );
    assert!(record.mcp.is_some(), "{record:?}");
}

#[test]
fn escape_sequences_in_server_text_never_reach_the_terminal() {
    let mut state = scratch();
    let _commands = connected(&mut state, true);
    msg(
        &mut state,
        Msg::Stderr(vec!["\u{1b}[2J\u{1b}]0;owned\u{7}after".into()]),
    );
    keys(&mut state, "5");
    let buffer = render(&state, 110, 32);
    assert!(
        buffer
            .content()
            .iter()
            .all(|cell| !cell.symbol().contains(char::is_control)),
        "a control character was drawn"
    );
    assert!(buffer_text(&buffer).contains("[2J]0;ownedafter"));
}
