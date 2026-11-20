//! The workspace plugin trust prompt (add-plugin-system task 4.1), with the
//! real trust store over a temporary workspace, rendered with ratatui's
//! `TestBackend`.

use std::path::Path;
use std::sync::Arc;

use rustman_core::plugins::PluginGate;
use rustman_core::run::{RequestRecord, RequestStatus};
use rustman_core::trust::TrustKey;
use rustman_core::{SecretBackend, Workspace};
use rustman_plugin::fixtures;
use rustman_plugin::pin::sha256_hex;
use rustman_plugin::trust::TrustGate;
use rustman_test_support::TestServer;

use crate::app::{Action, Effect, Overlay, Screen, State, update};
use crate::plugin_trust::Gate;
use crate::snapshot::{buffer_text, render};
use crate::workbench_tests::event;

/// A workspace at `<tmp>/ws` with the `faker` plugin.
fn workspace() -> (tempfile::TempDir, TrustGate) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(root.join("plugins")).unwrap();
    std::fs::write(root.join(".gitignore"), ".rustman/\n").unwrap();
    std::fs::write(
        root.join("rustman.toml"),
        "[workspace]\nname = \"w\"\nversion = 1\n",
    )
    .unwrap();
    let manifest = fixtures::manifest("faker", &["generator"], &["random"]);
    let faker = fixtures::with_manifest(&fixtures::faker(), &manifest);
    declare(&root, "faker", &faker, &["random"]);
    let gate = TrustGate {
        key: TrustKey::in_dir(tmp.path().join("keys")),
    };
    (tmp, gate)
}

fn declare(root: &Path, name: &str, wasm: &[u8], grants: &[&str]) {
    std::fs::write(root.join(format!("plugins/{name}.wasm")), wasm).unwrap();
    let grants: Vec<String> = grants.iter().map(|g| format!("\"{g}\"")).collect();
    let table = format!(
        "\n[plugins.{name}]\nsource = \"plugins/{name}.wasm\"\nsha256 = \"{}\"\ngrants = [{}]\n",
        sha256_hex(wasm),
        grants.join(", ")
    );
    let path = root.join("rustman.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(&table);
    std::fs::write(path, text).unwrap();
}

fn hmac() -> Vec<u8> {
    let manifest = fixtures::manifest("hmac", &["auth"], &["secrets:read", "clock", "net"]);
    fixtures::with_manifest(&fixtures::hmac_signer(), &manifest)
}

/// Open the workspace at `root` in a TUI using `gate`.
fn open(root: &Path, gate: &TrustGate) -> State {
    let mut state = State::with_screen(Screen::Loading(root.to_path_buf()));
    state.plugin_gate = Gate(Some(Arc::new(gate.clone())));
    update(
        &mut state,
        Action::Resize {
            width: 100,
            height: 30,
        },
    );
    let ws = Workspace::load(root).unwrap();
    update(&mut state, Action::WorkspaceLoaded(Ok(Box::new(ws))));
    state
}

/// Trust what is declared at `root` now.
fn trust_now(root: &Path, gate: &TrustGate) {
    let prompt = gate.check(root).unwrap().expect("untrusted");
    gate.approve(root, &prompt).unwrap();
}

fn trust_file(root: &Path) -> String {
    std::fs::read_to_string(root.join(".rustman/trust.yaml")).unwrap_or_default()
}

/// "Teammate adds a plugin": a trusted workspace gains `[plugins.hmac]`;
/// the prompt lists it and its grants, and nothing is recorded or run
/// until the user answers.
#[test]
fn teammate_adds_a_plugin() {
    let (tmp, gate) = workspace();
    let root = tmp.path().join("ws");
    trust_now(&root, &gate);
    let trusted = trust_file(&root);
    assert!(gate.check(&root).unwrap().is_none());

    declare(&root, "hmac", &hmac(), &["secrets:read:hmac_key", "clock"]);
    let mut state = open(&root, &gate);
    let Some(Overlay::PluginTrust(prompt)) = &state.overlay else {
        panic!("no trust prompt: {:?}", state.overlay);
    };
    assert!(prompt.changed);
    let text = buffer_text(&render(&state, 100, 30));
    assert!(text.contains("hmac 0.1.0"), "{text}");
    assert!(text.contains("may read the secret hmac_key"), "{text}");
    insta::assert_snapshot!("plugin_trust_teammate_adds_a_plugin", text);
    assert_eq!(
        trust_file(&root),
        trusted,
        "nothing is recorded before the answer"
    );

    // Declining keeps every plugin untrusted.
    update(&mut state, Action::Key(event("n")));
    assert!(state.overlay.is_none());
    assert_eq!(trust_file(&root), trusted);
    assert!(gate.check(&root).unwrap().is_some());

    // Approving records the new set: the next open does not ask.
    let mut state = open(&root, &gate);
    update(&mut state, Action::Key(event("y")));
    assert!(state.overlay.is_none());
    assert!(gate.check(&root).unwrap().is_none());
    assert!(open(&root, &gate).overlay.is_none());
}

/// "Grants changed": a trusted plugin whose grants gain
/// `net:api.example.com` is shown again.
#[test]
fn grants_changed() {
    let (tmp, gate) = workspace();
    let root = tmp.path().join("ws");
    declare(&root, "hmac", &hmac(), &["secrets:read:hmac_key", "clock"]);
    trust_now(&root, &gate);
    assert!(open(&root, &gate).overlay.is_none());

    let settings = std::fs::read_to_string(root.join("rustman.toml")).unwrap();
    let changed = settings.replace(
        "grants = [\"secrets:read:hmac_key\", \"clock\"]",
        "grants = [\"secrets:read:hmac_key\", \"clock\", \"net:api.example.com\"]",
    );
    assert_ne!(changed, settings);
    std::fs::write(root.join("rustman.toml"), changed).unwrap();
    let state = open(&root, &gate);
    assert!(matches!(state.overlay, Some(Overlay::PluginTrust(_))));
    let text = buffer_text(&render(&state, 100, 30));
    assert!(
        text.contains("may send HTTP requests to api.example.com"),
        "{text}"
    );
    insta::assert_snapshot!("plugin_trust_grants_changed", text);
}

/// Raised limits make a new set, and the prompt shows them.
#[test]
fn limits_changed() {
    let (tmp, gate) = workspace();
    let root = tmp.path().join("ws");
    trust_now(&root, &gate);
    let settings = std::fs::read_to_string(root.join("rustman.toml")).unwrap();
    let raised = format!("{settings}limits = {{ cpu_ms = 10000, memory_mib = 1024 }}\n");
    std::fs::write(root.join("rustman.toml"), raised).unwrap();
    let state = open(&root, &gate);
    let Some(Overlay::PluginTrust(prompt)) = &state.overlay else {
        panic!("no trust prompt: {:?}", state.overlay);
    };
    assert!(prompt.changed);
    let text = buffer_text(&render(&state, 100, 30));
    assert!(
        text.contains("limits: execution time 10000 ms per call, memory 1024 MiB"),
        "{text}"
    );
}

/// Without plugin support (no gate), nothing is asked.
#[test]
fn without_a_gate_nothing_is_asked() {
    let (tmp, _gate) = workspace();
    let root = tmp.path().join("ws");
    let mut state = State::with_screen(Screen::Loading(root.clone()));
    let ws = Workspace::load(&root).unwrap();
    update(&mut state, Action::WorkspaceLoaded(Ok(Box::new(ws))));
    assert!(state.overlay.is_none());
}

/// "Plugin kind loads", in the TUI: once the set is trusted, a `kind: soap`
/// request opens in the pane its plugin declares, with each field, its
/// editor and the response views.
#[test]
fn plugin_kind_pane() {
    let (tmp, gate) = workspace();
    let root = tmp.path().join("ws");
    let manifest = fixtures::manifest("soapy", &["kind"], &[]);
    let soapy = fixtures::with_manifest(&fixtures::kind_plugin("soap"), &manifest);
    declare(&root, "soapy", &soapy, &[]);
    std::fs::write(
        root.join("order.rman.yaml"),
        "version: 1\nkind: soap\nname: get-order\nendpoint: '{{base_url}}/soap'\n\
         action: GetOrder\nenvelope: |\n  <Envelope>\n    <Body/>\n  </Envelope>\n",
    )
    .unwrap();
    trust_now(&root, &gate);

    let mut state = State::with_screen(Screen::Loading(root.clone()));
    state.plugin_gate = Gate(Some(Arc::new(gate.clone())));
    update(
        &mut state,
        Action::Resize {
            width: 100,
            height: 30,
        },
    );
    let ws = Workspace::load_with(&root, &crate::files::registry()).unwrap();
    update(&mut state, Action::WorkspaceLoaded(Ok(Box::new(ws))));
    assert!(state.overlay.is_none());
    assert_eq!(state.plugin_kinds.len(), 1);
    crate::workbench::open_path(&mut state, Path::new("order.rman.yaml"));
    let text = buffer_text(&render(&state, 100, 30));
    assert!(text.contains("Kind \"soap\" from plugin soapy"), "{text}");
    assert!(
        text.contains("SOAP action  one of: GetOrder, ListOrders"),
        "{text}"
    );
    assert!(text.contains("Envelope  code (xml)"), "{text}");
    assert!(text.contains("Response views  Raw, JSON tree"), "{text}");
    insta::assert_snapshot!("plugin_kind_pane", text);
}

/// Send `request` from a TUI opened on `root`, with the file secret store,
/// after answering the trust prompt with `answer` (when one is shown).
async fn send_after(root: &Path, gate: &TrustGate, answer: &str, request: &str) -> RequestRecord {
    let mut state = open(root, gate);
    if state.overlay.is_some() {
        update(&mut state, Action::Key(event(answer)));
    }
    state.secret_backend = Some(SecretBackend::File);
    crate::workbench::open_path(&mut state, Path::new(request));
    let effects = crate::send::start(&mut state);
    let [Effect::Send(job)] = effects.as_slice() else {
        panic!("expected a send, got {effects:?}");
    };
    let job = job.clone();
    let done = tokio::task::spawn_blocking(move || crate::send::execute(&job))
        .await
        .unwrap();
    let Action::SendFinished { received, .. } = done else {
        panic!("expected a finished send");
    };
    received.record
}

/// A TUI send uses the trusted plugin set like a headless command (here a
/// value generator); an untrusted set stays unloaded and the send says so.
#[tokio::test(flavor = "multi_thread")]
async fn a_send_uses_the_plugins_only_once_trusted() {
    let server = TestServer::http().await;
    let (tmp, gate) = workspace();
    let root = tmp.path().join("ws");
    std::fs::write(
        root.join("mail.rman.yaml"),
        format!(
            "version: 1\nkind: http\nname: Mail\nmethod: GET\nurl: '{}?e={{{{$faker.email}}}}'\n",
            server.url("/echo")
        ),
    )
    .unwrap();

    let refused = send_after(&root, &gate, "n", "mail.rman.yaml").await;
    assert_eq!(refused.status, RequestStatus::Errored);
    let error = refused.error.unwrap_or_default();
    assert!(error.contains("plugin faker is not loaded"), "{error}");
    assert!(refused.exchange.is_none(), "nothing is sent");

    let sent = send_after(&root, &gate, "y", "mail.rman.yaml").await;
    assert_eq!(sent.error, None);
    assert_eq!(sent.status, RequestStatus::Passed);
    let url = &sent.exchange.expect("sent").request.url;
    assert!(
        url.contains("?e=user-") && url.contains("example.test"),
        "{url}"
    );
}
