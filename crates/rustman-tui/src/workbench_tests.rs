//! Workbench flows driven through `update` with keys, as the user types
//! them, against a fixture workspace (tasks 4.1–4.4 and 4.6). Effects run
//! synchronously, the way the effect runner would run them.

use std::fs;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;

use crate::app::{Action, Effect, Overlay, State, update};
use crate::config::Overrides;
use crate::effects::execute;
use crate::focus::Focus;
use crate::keymap::chord::{Code, parse_chord};
use crate::snapshot::{buffer_text, render, style_of};
use crate::theme::{ColorMode, Token};
use crate::workbench::sidebar_rows;

const GET_USER: &str = "version: 1\nkind: http\nname: Get user\nmethod: GET\n\
    url: '{{base_url}}/users/{{id}}'\nheaders:\n- name: Authorization\n  value: Bearer {{token}}\n";

/// A workspace with users/get-user, users/create-user and grpc/hello, the
/// environment `local` (with the secret `token`), loaded into a shell.
fn fixture(width: u16, height: u16) -> (tempfile::TempDir, State) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    let write = |rel: &str, text: &str| {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    write(
        "rustman.toml",
        "[workspace]\nname = \"demo\"\nversion = 1\n\n[defaults]\ndefault_environment = \"local\"\n",
    );
    write(
        "environments/local.yaml",
        "variables:\n  base_url: http://localhost:8080\nsecrets: [token]\n",
    );
    write("users/get-user.rman.yaml", GET_USER);
    write(
        "users/create-user.rman.yaml",
        "version: 1\nkind: http\nname: Create user\nmethod: POST\nurl: '{{base_url}}/users'\n",
    );
    write(
        "grpc/hello.rman.yaml",
        "version: 1\nkind: grpc\nname: Hello\nservice: Greeter\nrpc: SayHello\n",
    );
    let config = tmp.path().join("config");
    fs::create_dir(&config).unwrap();
    let (mut state, effects) = crate::initial_state_with(
        &root,
        Overrides::default(),
        Some(&config),
        ColorMode::TrueColor,
    );
    run(&mut state, effects);
    update(&mut state, Action::Resize { width, height });
    (tmp, state)
}

fn root(state: &State) -> PathBuf {
    crate::workbench::workspace(state).unwrap().root.clone()
}

/// Run effects (and the effects their completions return) to the end.
pub(crate) fn run(state: &mut State, mut effects: Vec<Effect>) {
    while let Some(effect) = effects.pop() {
        let done = execute(effect);
        effects.extend(update(state, done));
    }
}

pub(crate) fn event(text: &str) -> KeyEvent {
    let chord = parse_chord(text).unwrap();
    let code = match chord.code {
        Code::Char(c) => KeyCode::Char(c),
        Code::F(n) => KeyCode::F(n),
        Code::Enter => KeyCode::Enter,
        Code::Esc => KeyCode::Esc,
        Code::Tab => KeyCode::Tab,
        Code::BackTab => KeyCode::BackTab,
        Code::Backspace => KeyCode::Backspace,
        Code::Delete => KeyCode::Delete,
        Code::Up => KeyCode::Up,
        Code::Down => KeyCode::Down,
        Code::Left => KeyCode::Left,
        Code::Right => KeyCode::Right,
        Code::Home => KeyCode::Home,
        Code::End => KeyCode::End,
        Code::PageUp => KeyCode::PageUp,
        Code::PageDown => KeyCode::PageDown,
        Code::Insert => KeyCode::Insert,
    };
    let mut modifiers = KeyModifiers::NONE;
    modifiers.set(KeyModifiers::CONTROL, chord.ctrl);
    modifiers.set(KeyModifiers::ALT, chord.alt);
    modifiers.set(KeyModifiers::SHIFT, chord.shift);
    KeyEvent::new(code, modifiers)
}

/// Press space-separated keys and run their effects.
pub(crate) fn press(state: &mut State, keys: &str) {
    for key in keys.split(' ') {
        let effects = update(state, Action::Key(event(key)));
        run(state, effects);
    }
}

fn type_text(state: &mut State, text: &str) {
    for c in text.chars() {
        let effects = update(
            state,
            Action::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
        );
        run(state, effects);
    }
}

pub(crate) fn text(state: &State) -> String {
    buffer_text(&render(state, state.area.width, state.area.height))
}

/// Select the sidebar row labelled `label` and focus the sidebar.
fn select(state: &mut State, label: &str) {
    state.shell.focus = Focus::Sidebar;
    let rows = sidebar_rows(state);
    state.shell.selected = rows.iter().position(|r| r.label == label).unwrap();
}

fn open(state: &mut State, label: &str) {
    select(state, label);
    press(state, "Enter");
}

fn labels(state: &State) -> Vec<String> {
    sidebar_rows(state).into_iter().map(|r| r.label).collect()
}

// ---- 4.1 collection tree -------------------------------------------------------

#[test]
fn the_tree_shows_folders_badges_and_opens_requests() {
    // Scenario "Open a request".
    let (_tmp, mut state) = fixture(160, 45);
    let screen = text(&state);
    for row in [
        "▾ grpc",
        "GRPC   hello",
        "▾ users",
        "POST   create-user",
        "GET    get-user",
    ] {
        assert!(screen.contains(row), "{row}:\n{screen}");
    }
    open(&mut state, "get-user");
    assert_eq!(state.tabs.list.len(), 1);
    assert_eq!(state.shell.focus, Focus::Url);
    open(&mut state, "get-user");
    assert_eq!(state.tabs.list.len(), 1, "the existing tab is focused");
    open(&mut state, "create-user");
    open(&mut state, "get-user");
    assert_eq!((state.tabs.list.len(), state.tabs.active), (2, 0));
    select(&mut state, "users");
    press(&mut state, "Enter");
    assert_eq!(
        labels(&state),
        ["grpc", "hello", "users"],
        "Enter folds a folder"
    );
    press(&mut state, "Right");
    assert_eq!(labels(&state).len(), 5);
    press(&mut state, "Left");
    assert_eq!(labels(&state).len(), 3);
}

#[test]
fn slash_filters_the_tree_with_highlighted_matches() {
    // Scenario "Filter the tree".
    let (_tmp, mut state) = fixture(160, 45);
    state.shell.focus = Focus::Sidebar;
    press(&mut state, "/ g u s r");
    assert_eq!(labels(&state), ["users/get-user"]);
    let buffer = render(&state, 160, 45);
    let screen = buffer_text(&buffer);
    assert!(screen.contains("/gusr_  1 found"), "{screen}");
    let hit = style_of(&buffer, "get-user").unwrap();
    assert!(hit.add_modifier.contains(Modifier::UNDERLINED));
    press(&mut state, "Enter");
    assert_eq!(state.tabs.active().unwrap().title(), "get-user");
    state.shell.focus = Focus::Sidebar;
    press(&mut state, "Esc");
    assert_eq!(labels(&state).len(), 5, "Esc clears the filter");
}

#[test]
fn n_creates_a_request_in_the_selected_folder_and_opens_it() {
    let (_tmp, mut state) = fixture(160, 45);
    select(&mut state, "get-user");
    press(&mut state, "n");
    assert!(matches!(state.overlay, Some(Overlay::Input(_))));
    assert!(text(&state).contains("Name of the new request in users"));
    type_text(&mut state, "list-users");
    press(&mut state, "Enter");
    let file = root(&state).join("users/list-users.rman.yaml");
    assert!(file.exists());
    assert!(labels(&state).contains(&"list-users".to_owned()));
    assert_eq!(state.tabs.active().unwrap().title(), "list-users");
    press(&mut state, "Ctrl+B");
    select(&mut state, "list-users");
    press(&mut state, "n");
    type_text(&mut state, "../escape");
    press(&mut state, "Enter");
    assert!(
        text(&state).contains("Type a name without"),
        "bad names are refused"
    );
    press(&mut state, "Esc");
    assert!(state.overlay.is_none());
}

#[test]
fn r_renames_and_open_tabs_follow() {
    let (_tmp, mut state) = fixture(160, 45);
    open(&mut state, "create-user");
    select(&mut state, "create-user");
    press(&mut state, "r");
    for _ in 0.."create-user".len() {
        press(&mut state, "Backspace");
    }
    type_text(&mut state, "add-user");
    press(&mut state, "Enter");
    assert!(root(&state).join("users/add-user.rman.yaml").exists());
    assert!(!root(&state).join("users/create-user.rman.yaml").exists());
    assert_eq!(state.tabs.active().unwrap().title(), "add-user");
    assert!(labels(&state).contains(&"add-user".to_owned()));
}

#[test]
fn d_deletes_only_after_confirmation() {
    // Scenario "Delete requires confirmation".
    let (_tmp, mut state) = fixture(160, 45);
    open(&mut state, "get-user");
    let file = root(&state).join("users/get-user.rman.yaml");
    select(&mut state, "get-user");
    press(&mut state, "d");
    assert!(text(&state).contains("Delete users/get-user.rman.yaml?"));
    press(&mut state, "Enter");
    assert!(file.exists(), "Cancel is preselected");
    press(&mut state, "d");
    press(&mut state, "Left Enter");
    assert!(!file.exists());
    assert!(!labels(&state).contains(&"get-user".to_owned()));
    assert!(state.tabs.list.is_empty(), "its tab is closed");
}

// ---- 4.2 tabs ------------------------------------------------------------------

#[test]
fn tabs_open_close_and_switch_by_keys() {
    let (_tmp, mut state) = fixture(160, 45);
    open(&mut state, "get-user");
    press(&mut state, "Ctrl+T");
    open(&mut state, "create-user");
    let titles: Vec<String> = state.tabs.list.iter().map(|t| t.title()).collect();
    assert_eq!(titles, ["get-user", "untitled", "create-user"]);
    press(&mut state, "Alt+1");
    assert_eq!(state.tabs.active, 0);
    press(&mut state, "Ctrl+PageUp");
    assert_eq!(state.tabs.active, 2, "wraps around");
    press(&mut state, "Ctrl+PageDown Ctrl+PageDown");
    assert_eq!(state.tabs.active, 1);
    press(&mut state, "Alt+9");
    assert_eq!(state.tabs.active, 1, "no ninth tab");
    press(&mut state, "Ctrl+W");
    assert_eq!(
        state.tabs.list.len(),
        2,
        "a blank new tab closes without asking"
    );
    assert!(text(&state).contains(" get-user  create-user "));
}

#[test]
fn a_tab_in_flight_keeps_going_while_another_is_edited() {
    // Scenario "Independent tabs".
    let (_tmp, mut state) = fixture(160, 45);
    open(&mut state, "get-user");
    let a = state.tabs.active().unwrap().id;
    let (seq, _token) = state.tabs.get_mut(a).unwrap().start(state.now);
    open(&mut state, "create-user");
    press(&mut state, "End");
    type_text(&mut state, "/42");
    let b = state.tabs.active().unwrap();
    assert_eq!(b.editor.url().unwrap().text(), "{{base_url}}/users/42");
    assert!(state.tabs.get(a).unwrap().in_flight.is_some());
    let mut record = rustman_core::run::RequestRecord::new(
        "users/get-user".into(),
        "Get user".into(),
        "users/get-user.rman.yaml".into(),
        1,
    );
    record.duration_ms = 12.0;
    update(
        &mut state,
        Action::SendFinished {
            tab: a,
            seq,
            received: Box::new(crate::response::Received::without_body(record)),
        },
    );
    let a_tab = state.tabs.get(a).unwrap();
    assert!(a_tab.in_flight.is_none());
    assert_eq!(a_tab.exchange.as_ref().unwrap().record.id, "users/get-user");
    let b = state.tabs.active().unwrap();
    assert!(b.exchange.is_none(), "B is untouched");
    assert_eq!(b.editor.url().unwrap().text(), "{{base_url}}/users/42");
}

// ---- 4.3 dirty tracking, saving, prompts and changes on disk ---------------------

#[test]
fn editing_marks_the_tab_and_sidebar_dirty_and_ctrl_s_saves() {
    // Scenario "Dirty marker".
    let (_tmp, mut state) = fixture(160, 45);
    open(&mut state, "create-user");
    press(&mut state, "End");
    type_text(&mut state, "/v2");
    let screen = text(&state);
    assert!(screen.contains(" create-user ● "), "{screen}");
    assert!(
        screen.contains("create-user ●"),
        "sidebar marker:\n{screen}"
    );
    press(&mut state, "Ctrl+S");
    assert!(!state.tabs.active().unwrap().dirty);
    assert!(!text(&state).contains('●'));
    let saved = fs::read_to_string(root(&state).join("users/create-user.rman.yaml")).unwrap();
    assert!(saved.contains("url: '{{base_url}}/users/v2'"), "{saved}");
}

#[test]
fn a_new_tab_is_saved_under_a_name() {
    let (_tmp, mut state) = fixture(160, 45);
    press(&mut state, "Ctrl+T");
    type_text(&mut state, "http://x/");
    press(&mut state, "Ctrl+S");
    assert!(matches!(state.overlay, Some(Overlay::Input(_))));
    type_text(&mut state, "scratch/ping");
    press(&mut state, "Enter");
    assert!(root(&state).join("scratch/ping.rman.yaml").exists());
    assert_eq!(state.tabs.active().unwrap().title(), "ping");
    assert!(!state.tabs.any_dirty());
}

#[test]
fn closing_a_dirty_tab_asks_save_discard_or_cancel() {
    let (_tmp, mut state) = fixture(160, 45);
    open(&mut state, "create-user");
    type_text(&mut state, "x");
    press(&mut state, "Ctrl+W");
    assert!(text(&state).contains("create-user has unsaved changes."));
    press(&mut state, "Esc");
    assert_eq!(state.tabs.list.len(), 1, "Cancel keeps the tab");
    press(&mut state, "Ctrl+W d");
    assert!(state.tabs.list.is_empty(), "Discard closes it");
    let file = root(&state).join("users/create-user.rman.yaml");
    assert!(!fs::read_to_string(&file).unwrap().contains('x'));
    open(&mut state, "create-user");
    press(&mut state, "End");
    type_text(&mut state, "/z");
    press(&mut state, "Ctrl+W s");
    assert!(state.tabs.list.is_empty(), "Save closes it once saved");
    assert!(fs::read_to_string(&file).unwrap().contains("/users/z"));
}

#[test]
fn quitting_with_unsaved_changes_asks_first() {
    // Scenario "Quit with unsaved changes".
    let (_tmp, mut state) = fixture(160, 45);
    open(&mut state, "create-user");
    press(&mut state, "End");
    type_text(&mut state, "/q");
    press(&mut state, "Ctrl+Q");
    assert!(!state.quit);
    let screen = text(&state);
    assert!(
        screen.contains("1 tab has unsaved changes.") && screen.contains("Discard"),
        "{screen}"
    );
    press(&mut state, "c");
    assert!(!state.quit && state.overlay.is_none(), "Cancel");
    press(&mut state, "Ctrl+Q Enter");
    assert!(state.quit, "Save, then quit");
    let file = root(&state).join("users/create-user.rman.yaml");
    assert!(fs::read_to_string(file).unwrap().contains("/users/q"));
}

#[test]
fn a_clean_tab_follows_changes_on_disk() {
    let (_tmp, mut state) = fixture(160, 45);
    open(&mut state, "create-user");
    let rel = PathBuf::from("users/create-user.rman.yaml");
    let changed = "version: 1\nkind: http\nname: Create user\nmethod: PUT\nurl: http://other/\n";
    fs::write(root(&state).join(&rel), changed).unwrap();
    let effects = update(&mut state, Action::FilesChanged(vec![rel.clone()]));
    run(&mut state, effects);
    let tab = state.tabs.active().unwrap();
    assert_eq!(tab.editor.method(), Some("PUT"));
    assert!(!tab.dirty);
    let toasts: Vec<_> = state
        .notifications
        .log()
        .map(|e| e.message.clone())
        .collect();
    assert!(
        toasts.contains(&"Reloaded create-user from disk".to_owned()),
        "{toasts:?}"
    );
    // The echo of our own save changes nothing.
    let effects = update(&mut state, Action::FilesChanged(vec![rel]));
    run(&mut state, effects);
    assert_eq!(state.notifications.log().count(), toasts.len());
}

#[test]
fn a_dirty_tab_warns_of_a_conflict_on_disk() {
    // Scenario "External edit conflict".
    let (_tmp, mut state) = fixture(160, 45);
    open(&mut state, "get-user");
    press(&mut state, "End");
    type_text(&mut state, "?mine=1");
    let rel = PathBuf::from("users/get-user.rman.yaml");
    fs::write(root(&state).join(&rel), GET_USER.replace("GET", "DELETE")).unwrap();
    let effects = update(&mut state, Action::FilesChanged(vec![rel.clone()]));
    run(&mut state, effects);
    let screen = text(&state);
    assert!(
        screen.contains("Keep mine") && screen.contains("Load from disk"),
        "{screen}"
    );
    press(&mut state, "Enter");
    let tab = state.tabs.active().unwrap();
    assert!(tab.dirty && tab.editor.url().unwrap().text().ends_with("?mine=1"));
    let effects = update(&mut state, Action::FilesChanged(vec![rel]));
    run(&mut state, effects);
    press(&mut state, "l");
    let tab = state.tabs.active().unwrap();
    assert_eq!(tab.editor.method(), Some("DELETE"));
    assert!(!tab.dirty);
}

#[test]
fn a_conflict_waits_for_an_open_dialog_to_close() {
    let (_tmp, mut state) = fixture(160, 45);
    open(&mut state, "get-user");
    press(&mut state, "End");
    type_text(&mut state, "?mine=1");
    open(&mut state, "create-user");
    type_text(&mut state, "x");
    press(&mut state, "Ctrl+W");
    assert!(text(&state).contains("create-user has unsaved changes."));
    let rel = PathBuf::from("users/get-user.rman.yaml");
    fs::write(root(&state).join(&rel), GET_USER.replace("GET", "DELETE")).unwrap();
    let effects = update(&mut state, Action::FilesChanged(vec![rel]));
    run(&mut state, effects);
    let screen = text(&state);
    assert!(
        screen.contains("create-user has unsaved changes.") && !screen.contains("Keep mine"),
        "the open dialog is kept:\n{screen}"
    );
    press(&mut state, "Esc");
    assert_eq!(state.tabs.list.len(), 2, "Cancel keeps the tab");
    let screen = text(&state);
    assert!(
        screen.contains("Keep mine") && screen.contains("Load from disk"),
        "the conflict is asked next:\n{screen}"
    );
    press(&mut state, "Esc");
    assert!(state.overlay.is_none(), "asked once");
}

#[test]
fn a_failed_save_is_an_error_toast_and_keeps_the_tab_dirty() {
    let (_tmp, mut state) = fixture(160, 45);
    open(&mut state, "create-user");
    type_text(&mut state, "x");
    let id = state.tabs.active().unwrap().id;
    let path = Path::new("users/create-user.rman.yaml").to_path_buf();
    let result = Err("Permission denied (os error 13)".to_owned());
    let effects = update(
        &mut state,
        Action::Saved {
            tab: id,
            path,
            result,
        },
    );
    run(&mut state, effects);
    assert!(state.tabs.active().unwrap().dirty);
    let toast = state.notifications.visible().next().unwrap();
    assert!(
        toast.message.ends_with("Permission denied (os error 13)"),
        "{}",
        toast.message
    );
}

// ---- 4.4 panes ------------------------------------------------------------------

#[test]
fn an_unknown_kind_opens_read_only_with_a_notice() {
    // Scenario "Unknown kind".
    let (_tmp, mut state) = fixture(160, 45);
    open(&mut state, "hello");
    state.shell.focus = Focus::EditorBody;
    type_text(&mut state, "zz");
    let screen = text(&state);
    assert!(screen.contains("No editor for kind \"grpc\""), "{screen}");
    assert!(screen.contains("service: Greeter"), "{screen}");
    assert!(!state.tabs.active().unwrap().dirty, "nothing can be typed");
    insta::assert_snapshot!("read_only_grpc", screen);
}

// ---- 4.6 variable highlighting and the tooltip -------------------------------------

#[test]
fn variables_are_coloured_by_resolution() {
    // Scenario "Unresolved variable".
    let (_tmp, mut state) = fixture(160, 45);
    open(&mut state, "get-user");
    state.shell.focus = Focus::Method;
    let buffer = render(&state, 160, 45);
    let theme = &state.ui.theme;
    assert_eq!(
        style_of(&buffer, "{{base_url}}").unwrap().fg,
        theme.color(Token::Accent)
    );
    assert_eq!(
        style_of(&buffer, "{{id}}").unwrap().fg,
        theme.color(Token::Error)
    );
    insta::assert_snapshot!("variables_resolved_and_unresolved", buffer_text(&buffer));
}

#[test]
fn the_tooltip_shows_value_and_layer() {
    let (_tmp, mut state) = fixture(160, 45);
    open(&mut state, "get-user");
    press(&mut state, "Home");
    let screen = text(&state);
    assert!(
        screen.contains("{{base_url}} = http://localhost:8080 · environment"),
        "{screen}"
    );
    press(&mut state, "End Left Left");
    assert!(text(&state).contains("{{id}} is not defined in any layer"));
    insta::assert_snapshot!("tooltip_unresolved", text(&state));
}

#[test]
fn the_tooltip_masks_secrets() {
    // Scenario "Inspect a secret".
    let (_tmp, mut state) = fixture(160, 45);
    open(&mut state, "get-user");
    press(&mut state, "Tab Right Tab");
    assert_eq!(state.shell.focus, Focus::EditorBody);
    let screen = text(&state);
    assert!(
        screen.contains("{{token}} = •••••••• · secret (environment)"),
        "{screen}"
    );
    insta::assert_snapshot!("tooltip_secret", screen);
}

#[test]
fn a_stored_secret_never_reaches_the_screen() {
    // Canary for the variable tooltip and highlighting: the value of a
    // secret that is set in the store is never rendered.
    let (_tmp, mut state) = fixture(160, 45);
    let store = rustman_core::SecretStore::open(&root(&state), rustman_core::SecretBackend::File);
    let canary = "canary-7f3a-secret";
    store
        .set(
            "local",
            &rustman_core::Secret::new("token", canary.to_owned()),
        )
        .unwrap();
    assert_eq!(
        store.get("local", "token").unwrap().unwrap().expose(),
        canary,
        "the secret is really stored"
    );
    open(&mut state, "get-user");
    press(&mut state, "Tab Right Tab");
    let screen = text(&state);
    assert!(screen.contains("{{token}} = ••••••••"), "{screen}");
    assert!(!screen.contains(canary), "{screen}");
    press(&mut state, "Enter Tab");
    let editing = text(&state);
    assert!(editing.contains("Bearer {{token}}"), "{editing}");
    assert!(!editing.contains(canary), "{editing}");
}

#[test]
fn an_editor_fallback_loads_the_edit_and_toasts_why() {
    let mut state = State::with_screen(crate::app::Screen::Shell(crate::app::Session::Scratch));
    crate::testing::open_raw_body(&mut state);
    let tab = state.tabs.active().unwrap().id;
    let edited = crate::external::Edited {
        text: "{\"edited\":true}".into(),
        fallback: Some("cannot run nope (not found); opened vi".into()),
    };
    let effects = update(
        &mut state,
        Action::ExternalEdited {
            tab,
            result: Ok(edited),
        },
    );
    assert!(effects.is_empty());
    assert_eq!(crate::testing::body_text(&state), "{\"edited\":true}");
    let toasts: Vec<_> = state
        .notifications
        .log()
        .map(|e| e.message.clone())
        .collect();
    assert_eq!(
        toasts,
        ["External editor: cannot run nope (not found); opened vi"]
    );
}
