//! "Copy as…" driven through `update` with keys (runner-parity tasks
//! 5.1-5.3). Effects run synchronously, the way the effect runner would.

use std::path::Path;

use rustman_core::{Secret, SecretBackend, SecretStore};
use rustman_http::{Lang, Snippet};

use crate::app::{Overlay, State, update};
use crate::copy_as::{OSC52_LIMIT, SELECT_HINT, WARNING};
use crate::env_tests::{load, workspace};
use crate::workbench_tests::{press, text};

/// A secret value that must reach a snippet only while revealed.
const CANARY: &str = "canary-c0py-as-7e1f";

const TOML: &str = "[workspace]\nname = \"demo\"\nversion = 1\n\n[defaults]\n\
    default_environment = \"dev\"\n";

/// A workspace with `me` (a bearer secret), `events` (kind `kafka`) and
/// the environment `dev`, whose secret `token` is stored.
pub(crate) fn fixture() -> (tempfile::TempDir, State) {
    let tmp = workspace(&[
        ("rustman.toml", TOML),
        (
            "environments/dev.yaml",
            "variables:\n  base_url: http://localhost:8080\nsecrets: [token]\n",
        ),
        (
            "me.rman.yaml",
            "version: 1\nkind: http\nname: Me\nmethod: GET\nurl: '{{base_url}}/me'\n\
             headers:\n- name: Authorization\n  value: Bearer {{token}}\n",
        ),
        (
            "events.rman.yaml",
            "version: 1\nkind: kafka\nname: Events\ntopic: orders\n",
        ),
    ]);
    SecretStore::open(&tmp.path().join("ws"), SecretBackend::File)
        .set("dev", &Secret::new("token", CANARY.to_owned()))
        .unwrap();
    let mut state = load(&tmp, 100, 30);
    state.osc52 = true;
    crate::workbench::open_path(&mut state, Path::new("me.rman.yaml"));
    (tmp, state)
}

pub(crate) fn type_text(state: &mut State, text: &str) {
    for c in text.chars() {
        let key = if c == ' ' {
            "Space".to_owned()
        } else {
            c.to_string()
        };
        press(state, &key);
    }
}

/// Let the visible toasts expire, so they do not cover the overlay.
fn expire_toasts(state: &mut State) {
    let later = state.now + std::time::Duration::from_secs(60);
    update(state, crate::app::Action::Tick(later));
}

pub(crate) fn toasts(state: &State) -> Vec<String> {
    state
        .notifications
        .log()
        .map(|e| e.message.clone())
        .collect()
}

fn picker_langs(state: &State) -> Vec<String> {
    let Some(Overlay::CopyAs(picker)) = &state.overlay else {
        panic!("expected the picker, got {:?}", state.overlay);
    };
    picker.visible().map(|e| e.label.clone()).collect()
}

// ---- 5.1 the picker ----------------------------------------------------------

#[test]
fn copy_as_python_copies_the_redacted_snippet() {
    // Scenario "Copy as Python".
    let (_tmp, mut state) = fixture();
    press(&mut state, "Alt+c");
    type_text(&mut state, "py req");
    assert_eq!(picker_langs(&state)[0], "python-requests");
    press(&mut state, "Enter");
    assert!(state.overlay.is_none());
    let copied = state.clipboard.clone().expect("copied through OSC 52");
    assert!(copied.contains("import requests"), "{copied}");
    assert!(copied.contains("«redacted:"), "{copied}");
    assert!(!copied.contains(CANARY), "{copied}");
    assert!(toasts(&state).contains(&"Copied python-requests snippet".to_owned()));
}

#[test]
fn alt_c_opens_the_picker_from_the_editor_too() {
    let (_tmp, mut state) = fixture();
    for focus in [crate::focus::Focus::Url, crate::focus::Focus::EditorBody] {
        state.shell.focus = focus;
        press(&mut state, "Alt+c");
        assert!(
            matches!(state.overlay, Some(Overlay::CopyAs(_))),
            "{focus:?}"
        );
        press(&mut state, "Esc");
    }
    assert!(!state.tabs.active().unwrap().dirty, "nothing was typed");
}

#[test]
fn the_picker_lists_the_most_recently_used_language_first() {
    let (_tmp, mut state) = fixture();
    press(&mut state, "Alt+c");
    assert_eq!(picker_langs(&state)[..2], ["curl", "httpie"]);
    type_text(&mut state, "go");
    press(&mut state, "Enter");
    press(&mut state, "Alt+c");
    assert_eq!(picker_langs(&state)[..3], ["go", "curl", "httpie"]);
    insta::assert_snapshot!("copy_as_picker", text(&state));
    press(&mut state, "Esc");
    assert!(state.overlay.is_none());
}

#[test]
fn each_language_is_a_palette_entry() {
    let (_tmp, mut state) = fixture();
    press(&mut state, "Ctrl+p");
    type_text(&mut state, "copy as curl");
    press(&mut state, "Enter");
    let copied = state.clipboard.clone().expect("copied");
    assert!(copied.starts_with("curl "), "{copied}");
    assert!(!copied.contains(CANARY), "{copied}");
    assert!(toasts(&state).contains(&"Copied curl snippet".to_owned()));
    // The palette entry counts as a use of the language.
    press(&mut state, "Alt+c");
    assert_eq!(picker_langs(&state)[0], "curl");
}

#[test]
fn an_unsupported_kind_toasts_and_opens_no_picker() {
    // Scenario "Unsupported kind".
    let (_tmp, mut state) = fixture();
    crate::workbench::open_path(&mut state, Path::new("events.rman.yaml"));
    press(&mut state, "Alt+c");
    assert!(state.overlay.is_none());
    assert!(toasts(&state).contains(&"No snippet export for kind 'kafka'".to_owned()));
    assert!(state.clipboard.is_none());
}

#[test]
fn without_a_request_there_is_nothing_to_copy() {
    let tmp = workspace(&[("rustman.toml", TOML)]);
    let mut state = load(&tmp, 100, 30);
    press(&mut state, "Alt+c");
    assert!(state.overlay.is_none());
    assert!(toasts(&state).contains(&"Open a request to copy it as code".to_owned()));
}

// ---- 5.2 OSC 52 and the read-only pane ----------------------------------------

#[test]
fn without_osc52_the_snippet_opens_in_a_read_only_pane() {
    // Scenario "Unsupported terminal".
    let (_tmp, mut state) = fixture();
    state.osc52 = false;
    press(&mut state, "Alt+c");
    press(&mut state, "Enter");
    assert!(state.clipboard.is_none(), "nothing written to the terminal");
    assert!(matches!(state.overlay, Some(Overlay::Snippet(_))));
    let screen = text(&state);
    assert!(screen.contains(SELECT_HINT), "{screen}");
    assert!(screen.contains("curl snippet"), "{screen}");
    assert!(screen.contains("http://localhost:8080/me"), "{screen}");
    assert!(!screen.contains(CANARY), "{screen}");
    press(&mut state, "Esc");
    assert!(state.overlay.is_none());
}

#[test]
fn a_snippet_over_the_limit_opens_in_the_pane() {
    // The limit is on the sequence sent, which base64 makes about 4/3 of
    // the snippet: 76 794 bytes fit, one more does not.
    let fits = "x".repeat(76_794);
    let over = "x".repeat(76_795);
    assert!(crate::clipboard::osc52(&fits).len() <= OSC52_LIMIT);
    assert!(crate::clipboard::osc52(&over).len() > OSC52_LIMIT);
    let (_tmp, mut state) = fixture();
    let big = Snippet {
        code: over,
        warnings: vec!["snippet: a warning".into()],
    };
    crate::copy_as::on_made(&mut state, Lang::Curl, Ok(big));
    assert!(state.clipboard.is_none());
    assert!(matches!(state.overlay, Some(Overlay::Snippet(_))));
    assert!(toasts(&state).contains(&"snippet: a warning".to_owned()));
    let raw_limit = Snippet {
        code: "x".repeat(OSC52_LIMIT),
        warnings: Vec::new(),
    };
    state.overlay = None;
    crate::copy_as::on_made(&mut state, Lang::Curl, Ok(raw_limit));
    assert!(
        state.clipboard.is_none(),
        "100 KB raw is too long once encoded"
    );
    assert!(matches!(state.overlay, Some(Overlay::Snippet(_))));
    state.overlay = None;
    let fitting = Snippet {
        code: fits,
        warnings: Vec::new(),
    };
    crate::copy_as::on_made(&mut state, Lang::Curl, Ok(fitting));
    assert_eq!(state.clipboard.as_ref().map(String::len), Some(76_794));
}

#[test]
fn the_copied_text_reaches_the_terminal_as_osc52() {
    let (_tmp, mut state) = fixture();
    press(&mut state, "Alt+c");
    press(&mut state, "Enter");
    let copied = state.clipboard.clone().unwrap();
    let sequence = crate::clipboard::osc52(&copied);
    assert!(sequence.starts_with("\x1b]52;c;") && sequence.ends_with('\x07'));
}

// ---- 5.3 revealed secrets -------------------------------------------------------

#[test]
fn copying_while_revealed_takes_a_second_enter() {
    // Scenario "Copy while revealed".
    let (_tmp, mut state) = fixture();
    press(&mut state, "Alt+r");
    expire_toasts(&mut state);
    press(&mut state, "Alt+c");
    let screen = text(&state);
    assert!(screen.contains(WARNING), "{screen}");
    insta::assert_snapshot!("copy_as_revealed", screen);
    let effects = update(
        &mut state,
        crate::app::Action::Key(crate::workbench_tests::event("Enter")),
    );
    assert!(effects.is_empty(), "nothing runs after one Enter");
    assert!(state.clipboard.is_none(), "nothing copied after one Enter");
    assert!(text(&state).contains("Enter again to copy"));
    press(&mut state, "Enter");
    let copied = state
        .clipboard
        .clone()
        .expect("copied after the second Enter");
    assert!(copied.contains(CANARY), "revealed secrets are included");
}

#[test]
fn moving_the_selection_disarms_the_copy() {
    let (_tmp, mut state) = fixture();
    press(&mut state, "Alt+r");
    press(&mut state, "Alt+c");
    press(&mut state, "Enter");
    press(&mut state, "Down");
    press(&mut state, "Enter");
    assert!(
        state.clipboard.is_none(),
        "the move asked for confirmation again"
    );
    press(&mut state, "Enter");
    let copied = state.clipboard.clone().unwrap();
    assert!(copied.starts_with("http"), "httpie: {copied}");
}

#[test]
fn a_palette_entry_while_revealed_asks_for_confirmation() {
    let (_tmp, mut state) = fixture();
    press(&mut state, "Alt+r");
    press(&mut state, "Ctrl+p");
    type_text(&mut state, "copy as wget");
    press(&mut state, "Enter");
    assert!(state.clipboard.is_none());
    assert_eq!(picker_langs(&state)[0], "wget");
    press(&mut state, "Enter Enter");
    assert!(state.clipboard.clone().unwrap().contains("wget"));
}
