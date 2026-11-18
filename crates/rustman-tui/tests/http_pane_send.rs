//! Task 4.5 end to end: a header toggled off in the HTTP pane is not sent
//! (scenario "Disabled header not sent"). The edited request goes through
//! the engine's runner to the in-process test server, which echoes the
//! headers it received.

use std::fs;
use std::path::Path;

use rustman_core::collection::{Collection, Entry};
use rustman_core::secrets::{SecretBackend, SecretStore};
use rustman_core::{Workspace, model::Request};
use rustman_http::runner::{RunContext, RunOptions, Runner};
use rustman_test_support::TestServer;
use rustman_tui::focus::Focus;
use rustman_tui::http_pane::{HttpEditor, HttpTab};
use rustman_tui::keymap::chord::parse_chord;
use rustman_tui::pane::PaneEditor;

/// Replace the request at `path` in the loaded tree (an unsaved edit).
fn replace(collection: &mut Collection, path: &Path, request: &Request) -> bool {
    collection.entries.iter_mut().any(|entry| match entry {
        Entry::Request(r) if r.path == path => {
            r.request = request.clone();
            true
        }
        Entry::Request(_) => false,
        Entry::Collection(child) => replace(child, path, request),
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disabled_header_is_not_sent() {
    let server = TestServer::http().await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::write(
        root.join("rustman.toml"),
        "[workspace]\nname = \"t\"\nversion = 1\n",
    )
    .unwrap();
    let text = format!(
        "version: 1\nkind: http\nname: Echo\nmethod: GET\nurl: {}\nheaders:\n\
         - name: X-Debug\n  value: '1'\n- name: X-Keep\n  value: yes\n",
        server.url("/echo")
    );
    fs::write(root.join("echo.rman.yaml"), text).unwrap();
    let mut ws = Workspace::load(root).unwrap();

    let Request::Http(http) = &ws.requests()[0].entry.request else {
        panic!("http request");
    };
    let mut editor = HttpEditor::new(http);
    editor.tab = HttpTab::Headers;
    editor.key(Focus::EditorBody, parse_chord("Space").unwrap(), None);
    let edited = editor.request().unwrap();
    assert!(replace(&mut ws.tree, Path::new("echo.rman.yaml"), &edited));

    let store = SecretStore::open(root, SecretBackend::File);
    let ctx = RunContext {
        root,
        settings: &ws.settings,
        environment: None,
        secrets: &store,
        in_workspace: false,
    };
    let runner = Runner::new(ctx, RunOptions::default()).unwrap();
    let requests = ws.requests();
    let sent = runner.send(&requests[0], 1).await;
    let response = sent.record.exchange.expect("a response").response;
    let echoed = response.body.expect("an inline body").to_ascii_lowercase();
    assert!(echoed.contains("x-keep"), "{echoed}");
    assert!(
        !echoed.contains("x-debug"),
        "the disabled header was sent: {echoed}"
    );
}
