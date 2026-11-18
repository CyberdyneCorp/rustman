//! Keys in the shell: overlays first (palette, help), then the focused text
//! area, then the keymap; and running the action a key, the palette or the
//! help overlay names.

use std::path::Path;

use crate::app::{Effect, Overlay, Screen, Session, State};
use crate::focus::{Focus, Pane};
use crate::keymap::{ActionId, Chord, Context, Resolution, chord::Code};
use crate::layout::{Breakpoint, regions};
use crate::palette::{self, Palette, Scope, Sources, Target};
use crate::shell::{self, Command};
use crate::status::VimMode;
use crate::toast::Severity;
use crate::tree::{self, Filter};
use crate::{graphql, reveal, send, viewer_keys, viz, workbench};

/// Handle a key while the shell is showing.
pub fn shell_key(state: &mut State, chord: Chord) -> Vec<Effect> {
    let effects = match state.overlay {
        Some(Overlay::Palette(_)) => palette_key(state, chord),
        Some(Overlay::Help { .. }) => help_key(state, chord),
        Some(Overlay::Confirm(_)) => confirm_key(state, chord),
        Some(Overlay::Input(_)) => workbench::input_key(state, chord),
        Some(Overlay::Exchanges(_) | Overlay::Diff(_)) => viewer_keys::overlay_key(state, chord),
        _ => match crate::webhook::key(state, chord).or_else(|| viewer_keys::box_key(state, chord))
        {
            Some(effects) => effects,
            None => {
                let typed = state.pending_keys.is_empty() && text_key(state, chord);
                if typed {
                    Vec::new()
                } else {
                    keymap_key(state, chord)
                }
            }
        },
    };
    sync_vim_status(state);
    effects
}

/// The status bar shows the body's vim mode while it is focused.
fn sync_vim_status(state: &mut State) {
    state.status.vim = state.vim.as_ref().map(|vim| {
        if state.shell.focus == Focus::EditorBody {
            vim.mode
        } else {
            VimMode::Normal
        }
    });
}

/// Keys for the focused text input; false when the keymap should have it.
fn text_key(state: &mut State, chord: Chord) -> bool {
    match state.shell.focus {
        Focus::Sidebar => filter_key(state, chord),
        Focus::Response => false,
        focus => editor_key(state, focus, chord),
    }
}

/// Keys for the open request's editor.
fn editor_key(state: &mut State, focus: Focus, chord: Chord) -> bool {
    let Some(tab) = state.tabs.active_mut() else {
        // No request open: characters typed in the URL field do nothing
        // rather than run shortcuts.
        return focus == Focus::Url && chord.typed_char().is_some();
    };
    let handled = tab.editor.key(focus, chord, state.vim.as_mut());
    if handled {
        tab.refresh();
    }
    handled
}

/// Keys for the sidebar filter: typing edits the query while it is open,
/// `Esc` clears it.
fn filter_key(state: &mut State, chord: Chord) -> bool {
    let Some(filter) = state.shell.tree.filter.as_mut() else {
        return false;
    };
    match chord.code {
        Code::Esc => state.shell.tree.filter = None,
        _ if !filter.editing => return false,
        Code::Backspace => {
            filter.query.pop();
        }
        _ => match chord.typed_char() {
            Some(c) => filter.query.push(c),
            None => return false,
        },
    }
    state.shell.selected = 0;
    state.shell.scroll[Pane::Sidebar.index()] = 0;
    true
}

/// `/` in the sidebar: open the filter.
fn start_filter(state: &mut State) {
    state.shell.focus = Focus::Sidebar;
    let query = state
        .shell
        .tree
        .filter
        .take()
        .map(|f| f.query)
        .unwrap_or_default();
    state.shell.tree.filter = Some(Filter {
        query,
        editing: true,
    });
}

fn confirm_key(state: &mut State, chord: Chord) -> Vec<Effect> {
    let Some(Overlay::Confirm(confirm)) = &mut state.overlay else {
        return Vec::new();
    };
    match confirm.key(chord) {
        Some(decision) => {
            state.overlay = None;
            workbench::decide(state, decision)
        }
        None => Vec::new(),
    }
}

fn keymap_key(state: &mut State, chord: Chord) -> Vec<Effect> {
    let context = Context::of(state.shell.focus);
    state.pending_keys.push(chord);
    match state.keymap.resolve(context, &state.pending_keys) {
        Resolution::Action(action) => {
            state.pending_keys.clear();
            run(state, action)
        }
        Resolution::Pending => Vec::new(),
        Resolution::Unbound => {
            let retry = state.pending_keys.len() > 1;
            state.pending_keys.clear();
            // `g` then `j`: the `g` is dropped and `j` stands on its own.
            if retry {
                keymap_key(state, chord)
            } else {
                Vec::new()
            }
        }
    }
}

fn palette_key(state: &mut State, chord: Chord) -> Vec<Effect> {
    let Some(Overlay::Palette(palette)) = &mut state.overlay else {
        return Vec::new();
    };
    match (chord.code, chord.ctrl) {
        (Code::Esc, _) => state.overlay = None,
        (Code::Enter, _) => return choose(state),
        (Code::Up, _) | (Code::Char('p' | 'k'), true) => palette.move_by(-1),
        (Code::Down, _) | (Code::Char('n' | 'j'), true) => palette.move_by(1),
        (Code::Backspace, _) => {
            let mut query = palette.query.clone();
            query.pop();
            palette.set_query(&query, &state.recent);
        }
        _ => match chord.typed_char() {
            Some(c) => {
                let query = format!("{}{c}", palette.query);
                palette.set_query(&query, &state.recent);
            }
            None => return quit_only(state, chord),
        },
    }
    Vec::new()
}

/// Inside a modal, other keys do nothing, except quitting.
fn quit_only(state: &mut State, chord: Chord) -> Vec<Effect> {
    let quit = state.keymap.resolve(Context::Global, &[chord]);
    if quit == Resolution::Action(ActionId::Quit) {
        state.quit = true;
    }
    Vec::new()
}

/// Run the selected palette entry and remember it as recently used.
fn choose(state: &mut State) -> Vec<Effect> {
    let Some(Overlay::Palette(palette)) = state.overlay.take() else {
        return Vec::new();
    };
    let Some(entry) = palette.selected() else {
        return Vec::new();
    };
    let target = entry.target.clone();
    palette::touch(&mut state.recent, target.clone());
    match target {
        Target::Action(action) => run(state, action),
        Target::Request(path) => {
            workbench::reveal(state, Path::new(&path));
            Vec::new()
        }
        Target::Environment(name) => {
            state.notify(Severity::Info, format!("Active environment: {name}"));
            state.status.environment = Some(name);
            reveal::environment_changed(state)
        }
    }
}

fn help_key(state: &mut State, chord: Chord) -> Vec<Effect> {
    let Some(Overlay::Help { scroll }) = &mut state.overlay else {
        return Vec::new();
    };
    let closes = matches!(chord.code, Code::Esc | Code::F(1) | Code::Char('?'));
    match chord.code {
        _ if closes => state.overlay = None,
        Code::Up | Code::Char('k') => *scroll = scroll.saturating_sub(1),
        Code::Down | Code::Char('j') => *scroll = scroll.saturating_add(1),
        Code::PageUp => *scroll = scroll.saturating_sub(10),
        Code::PageDown => *scroll = scroll.saturating_add(10),
        _ => return quit_only(state, chord),
    }
    Vec::new()
}

/// Run `action`.
pub fn run(state: &mut State, action: ActionId) -> Vec<Effect> {
    // `Esc` cancels the request in flight before it dismisses anything
    // (spec: Cancel in-flight request).
    if action == ActionId::Escape && state.overlay.is_none() && send::cancel(state) {
        return Vec::new();
    }
    if let Some(effects) = request_action(state, action) {
        return effects;
    }
    if let Some(command) = shell_command(action) {
        shell::apply(state, command);
        return Vec::new();
    }
    if let Some(effects) = workbench_action(state, action) {
        return effects;
    }
    match action {
        ActionId::Quit => workbench::quit(state),
        ActionId::PaletteOpen => open_palette(state, Scope::Everything),
        ActionId::EnvSwitch => open_palette(state, Scope::Environments),
        ActionId::HelpOpen => state.overlay = Some(Overlay::Help { scroll: 0 }),
        ActionId::SecretsReveal => return reveal::toggle(state),
        ActionId::VizOpen => return viz::start(state),
        ActionId::WebhookOpen => crate::webhook::toggle_pane(state),
        ActionId::ReportHtml => return crate::response::html::start(state),
        ActionId::GraphqlRefreshSchema => return graphql::refresh(state),
        ActionId::GraphqlSubscriptionStart => return graphql::start_subscription(state),
        ActionId::GraphqlSubscriptionStop => {
            graphql::stop_subscription(state);
        }
        ActionId::EditorComplete => graphql::complete(state),
        ActionId::PaneLeft => pane_step(state, false),
        ActionId::PaneRight => pane_step(state, true),
        ActionId::ListUp
        | ActionId::ListDown
        | ActionId::ListTop
        | ActionId::ListBottom
        | ActionId::ListPageUp
        | ActionId::ListPageDown => list_move(state, action),
        _ => {
            let message = format!("{} is not available yet", action.title());
            state.notify(Severity::Info, message);
        }
    }
    Vec::new()
}

/// Sending, cancelling and the response viewer's actions.
fn request_action(state: &mut State, action: ActionId) -> Option<Vec<Effect>> {
    match action {
        ActionId::RequestSend => Some(send::start(state)),
        ActionId::RequestCancel => {
            if !send::cancel(state) {
                idle_cancel(state);
            }
            Some(Vec::new())
        }
        ActionId::ListUp
        | ActionId::ListDown
        | ActionId::ListTop
        | ActionId::ListBottom
        | ActionId::ListPageUp
        | ActionId::ListPageDown
            if state.shell.focus.pane() == Pane::Response =>
        {
            Some(viewer_keys::list_move(state, action))
        }
        _ => viewer_keys::run(state, action),
    }
}

/// The request workbench's actions: tabs, saving, the sidebar's file
/// commands, editor modes and the external editor.
fn workbench_action(state: &mut State, action: ActionId) -> Option<Vec<Effect>> {
    let effects = match action {
        ActionId::RequestSave => workbench::save(state),
        ActionId::SidebarOpen => workbench::open_selected(state),
        ActionId::EditorExternal => workbench::external(state),
        ActionId::TabNew => {
            workbench::new_tab(state);
            Vec::new()
        }
        ActionId::TabClose => {
            workbench::close_tab(state);
            Vec::new()
        }
        _ => {
            workbench_command(state, action)?;
            Vec::new()
        }
    };
    Some(effects)
}

/// The workbench actions that need no effect; `None` for other actions.
fn workbench_command(state: &mut State, action: ActionId) -> Option<()> {
    match action {
        ActionId::TabNext => state.tabs.step(true),
        ActionId::TabPrev => state.tabs.step(false),
        ActionId::SidebarFilter => start_filter(state),
        ActionId::SidebarNew => workbench::prompt_new(state),
        ActionId::SidebarRename => workbench::prompt_rename(state),
        ActionId::SidebarDelete => workbench::prompt_delete(state),
        ActionId::TreeCollapse => workbench::fold_selected(state, true),
        ActionId::TreeExpand => workbench::fold_selected(state, false),
        ActionId::EditorMode => workbench::cycle_mode(state),
        _ => state.tabs.select(action.tab_index()?),
    }
    Some(())
}

fn shell_command(action: ActionId) -> Option<Command> {
    Some(match action {
        ActionId::SidebarToggle => Command::ToggleSidebar,
        ActionId::SidebarGrow => Command::GrowSidebar,
        ActionId::SidebarShrink => Command::ShrinkSidebar,
        ActionId::FocusNext => Command::FocusNext,
        ActionId::FocusPrev => Command::FocusPrev,
        ActionId::Escape => Command::Dismiss,
        ActionId::NotificationsShow => Command::ShowNotifications,
        _ => return None,
    })
}

fn open_palette(state: &mut State, scope: Scope) {
    let requests = workbench::workspace(state).map_or_else(Vec::new, tree::request_paths);
    let environments: Vec<String> = match &state.screen {
        Screen::Shell(Session::Workspace(ws)) => {
            ws.environments.iter().map(|e| e.name.clone()).collect()
        }
        _ => Vec::new(),
    };
    let sources = Sources {
        keymap: &state.keymap,
        requests: &requests,
        environments: &environments,
        active_environment: state.status.environment.as_deref(),
        dot: state.ui.glyphs.dot,
    };
    let entries = palette::entries(scope, sources);
    let mut palette = Palette::new(scope, entries, &state.recent);
    if scope == Scope::Environments
        && let Some(ws) = workbench::workspace(state)
    {
        let variables = ws
            .environments
            .iter()
            .map(|env| (env.name.clone(), palette::env_variables(ws, env)))
            .collect();
        palette = palette.with_variables(variables);
    }
    state.overlay = Some(Overlay::Palette(Box::new(palette)));
}

/// `Ctrl+C` with nothing in flight never quits (spec: Cancel in-flight
/// request); it says how to.
fn idle_cancel(state: &mut State) {
    if state.status.in_flight > 0 {
        return;
    }
    let quit = state.keymap.key(ActionId::Quit);
    let quit = quit.as_deref().unwrap_or("Ctrl+Q");
    state.notify(Severity::Info, format!("Press {quit} to quit"));
}

/// `h`/`l` in vim mode: focus the pane to the left or right.
fn pane_step(state: &mut State, right: bool) {
    let bp = Breakpoint::of(state.area.width, state.area.height);
    let sidebar = state.shell.sidebar_shown(bp);
    let next = match (state.shell.focus.pane(), right) {
        (Pane::Sidebar, true) => Some(Pane::Editor),
        (Pane::Editor, true) => Some(Pane::Response),
        (Pane::Response, false) => Some(Pane::Editor),
        (Pane::Editor, false) if sidebar => Some(Pane::Sidebar),
        _ => None,
    };
    if let Some(pane) = next {
        state.shell.focus = pane.entry();
    }
}

/// Rows of `pane` that show content (inside the borders).
fn page_size(state: &State, pane: Pane) -> usize {
    regions(state.area, &state.shell)
        .pane(pane)
        .map_or(1, |r| usize::from(r.height.saturating_sub(2)).max(1))
}

fn list_move(state: &mut State, action: ActionId) {
    let pane = state.shell.focus.pane();
    let page = page_size(state, pane);
    let len = match pane {
        Pane::Sidebar => workbench::sidebar_rows(state).len(),
        _ => usize::from(u16::MAX),
    };
    let current = match pane {
        Pane::Sidebar => state.shell.selected,
        _ => usize::from(state.shell.scroll[pane.index()]),
    };
    let last = len.saturating_sub(1);
    let next = match action {
        ActionId::ListUp => current.saturating_sub(1),
        ActionId::ListDown => (current + 1).min(last),
        ActionId::ListTop => 0,
        ActionId::ListBottom => last,
        ActionId::ListPageUp => current.saturating_sub(page),
        _ => (current + page).min(last),
    };
    match pane {
        Pane::Sidebar => select(state, next),
        _ => state.shell.scroll[pane.index()] = u16::try_from(next).unwrap_or(u16::MAX),
    }
}

/// Select sidebar row `index` and scroll it into view.
fn select(state: &mut State, index: usize) {
    let rows = page_size(state, Pane::Sidebar);
    state.shell.selected = index;
    let scroll = usize::from(state.shell.scroll[Pane::Sidebar.index()]);
    let scroll = scroll.min(index).max((index + 1).saturating_sub(rows));
    state.shell.scroll[Pane::Sidebar.index()] = u16::try_from(scroll).unwrap_or(u16::MAX);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Action, update};
    use crate::config::Overrides;
    use crate::keymap::{Keymap, UserLayer};
    use crate::palette::Scope;
    use crate::snapshot::{buffer_text, render};
    use crate::theme::ColorMode;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::Path;

    /// A workspace with the environments `local` and `staging`, loaded into
    /// a shell of `width` x `height`, with the `keybindings.toml` text `keys`.
    fn shell(width: u16, height: u16, keys: &str, vim: bool) -> (tempfile::TempDir, State) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("demo");
        rustman_core::init::init(&dir, "demo", false).unwrap();
        std::fs::write(dir.join("environments/staging.yaml"), "variables: {}\n").unwrap();
        let config = tmp.path().join("config");
        std::fs::create_dir(&config).unwrap();
        std::fs::write(config.join("keybindings.toml"), keys).unwrap();
        let overrides = Overrides {
            vim,
            ..Overrides::default()
        };
        let (mut state, effects) =
            crate::initial_state_with(&dir, overrides, Some(&config), ColorMode::NoColor);
        for effect in effects {
            let done = crate::effects::execute(effect);
            update(&mut state, done);
        }
        update(&mut state, Action::Resize { width, height });
        (tmp, state)
    }

    /// Press space-separated keys (`Ctrl+P s w`), as the terminal sends them.
    fn press(state: &mut State, keys: &str) {
        for word in keys.split(' ') {
            let chord = crate::keymap::chord::parse_chord(word).unwrap();
            update(state, Action::Key(event(chord)));
        }
    }

    fn event(chord: Chord) -> KeyEvent {
        let code = match chord.code {
            Code::Char(c) => KeyCode::Char(c),
            Code::F(n) => KeyCode::F(n),
            Code::Enter => KeyCode::Enter,
            Code::Esc => KeyCode::Esc,
            Code::Tab => KeyCode::Tab,
            Code::BackTab => KeyCode::BackTab,
            Code::Backspace => KeyCode::Backspace,
            Code::Up => KeyCode::Up,
            Code::Down => KeyCode::Down,
            other => panic!("no event for {other:?}"),
        };
        let mut modifiers = KeyModifiers::NONE;
        modifiers.set(KeyModifiers::CONTROL, chord.ctrl);
        modifiers.set(KeyModifiers::ALT, chord.alt);
        KeyEvent::new(code, modifiers)
    }

    fn toasts(state: &State) -> Vec<String> {
        state
            .notifications
            .log()
            .map(|e| e.message.clone())
            .collect()
    }

    fn text(state: &State) -> String {
        buffer_text(&render(state, state.area.width, state.area.height))
    }

    fn focus_body(state: &mut State) {
        state.shell.focus = Focus::EditorBody;
    }

    #[test]
    fn keymap_conflicts_are_toasted_and_quit_keeps_a_key() {
        let (_tmp, mut state) = shell(160, 45, "[global]\n\"Ctrl+Q\" = \"none\"", false);
        let log: Vec<&str> = state
            .notifications
            .log()
            .map(|entry| entry.message.as_str())
            .collect();
        assert!(
            log.iter().any(|m| m.contains("app.quit has no key")),
            "{log:?}"
        );
        press(&mut state, "Ctrl+Q");
        assert!(state.quit);
    }

    #[test]
    fn a_rebound_key_runs_its_action_and_bad_entries_are_toasted() {
        // Scenarios "Rebind send" and "Invalid entry".
        let keys = "[global]\n\"F5\" = \"request.send\"\n\"F6\" = \"request.sned\"\n";
        let (_tmp, mut state) = shell(120, 36, keys, false);
        let warnings = toasts(&state);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("unknown action `request.sned`"),
            "{warnings:?}"
        );
        focus_body(&mut state);
        let resolved = state.keymap.resolve(Context::Editor, &[event_chord("F5")]);
        assert_eq!(resolved, Resolution::Action(ActionId::RequestSend));
        press(&mut state, "F5");
        assert!(
            toasts(&state)
                .iter()
                .any(|t| t == "Open a request to send it")
        );
        press(&mut state, "Ctrl+T");
        let f5 = KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE);
        let effects = update(&mut state, Action::Key(f5));
        assert!(
            matches!(effects.as_slice(), [Effect::Send(_)]),
            "F5 sends the current request: {effects:?}"
        );
        assert_eq!(state.status.in_flight, 1);
        press(&mut state, "Ctrl+B");
        assert!(state.shell.sidebar_hidden, "other bindings still work");
    }

    fn event_chord(text: &str) -> Chord {
        crate::keymap::chord::parse_chord(text).unwrap()
    }

    #[test]
    fn swenv_in_the_palette_opens_the_environment_switcher() {
        // Scenario "Run an action from the palette".
        let (_tmp, mut state) = shell(120, 36, "", false);
        press(&mut state, "Ctrl+P s w e n v");
        let Some(Overlay::Palette(palette)) = &state.overlay else {
            panic!("palette not open");
        };
        assert_eq!(palette.selected().unwrap().label, "Switch environment");
        press(&mut state, "Enter");
        let Some(Overlay::Palette(switcher)) = &state.overlay else {
            panic!("switcher not open");
        };
        assert_eq!(switcher.scope, Scope::Environments);
        insta::assert_snapshot!("environment_switcher", text(&state));
        press(&mut state, "Down Enter");
        assert!(state.overlay.is_none());
        assert_eq!(state.status.environment.as_deref(), Some("staging"));
        assert!(text(&state).contains("env: staging"));
    }

    #[test]
    fn palette_snapshot_shows_bindings() {
        // Scenario "Palette shows bindings".
        let (_tmp, mut state) = shell(120, 36, "", false);
        press(&mut state, "Ctrl+P s e n d");
        let text = text(&state);
        assert!(
            text.contains("Send request") && text.contains("Ctrl+Enter, Ctrl+R"),
            "{text}"
        );
        insta::assert_snapshot!("palette_send", text);
    }

    #[test]
    fn palette_editing_and_recency() {
        let (_tmp, mut state) = shell(120, 36, "", false);
        press(
            &mut state,
            "Ctrl+P h e l p Backspace Backspace Backspace Backspace",
        );
        let Some(Overlay::Palette(palette)) = &state.overlay else {
            panic!("palette not open");
        };
        assert_eq!(palette.query, "");
        press(&mut state, "Esc");
        assert!(state.overlay.is_none());
        press(&mut state, "Ctrl+P t o g g l e s i d e Enter");
        assert!(state.shell.sidebar_hidden);
        press(&mut state, "Ctrl+P");
        let Some(Overlay::Palette(palette)) = &state.overlay else {
            panic!("palette not open");
        };
        assert_eq!(
            palette.selected().unwrap().label,
            "Toggle sidebar",
            "recent first"
        );
        press(&mut state, "Ctrl+Q");
        assert!(state.quit, "Ctrl+Q quits from the palette");
    }

    #[test]
    fn choosing_a_request_opens_and_selects_it() {
        let (_tmp, mut state) = shell(100, 30, "", false);
        press(&mut state, "Ctrl+P p i n g");
        press(&mut state, "Enter");
        assert_eq!(state.shell.focus, Focus::Url);
        assert_eq!(state.tabs.active().unwrap().title(), "ping");
        let rows = workbench::sidebar_rows(&state);
        assert_eq!(rows[state.shell.selected].label, "ping");
        assert_eq!(
            state.recent,
            [Target::Request("examples/ping.rman.yaml".into())]
        );
    }

    #[test]
    fn help_reflects_a_user_rebinding() {
        // Scenario "Help reflects overrides".
        let keys = "[global]\n\"F5\" = \"request.send\"\n";
        let (_tmp, mut state) = shell(120, 36, keys, false);
        press(&mut state, "?");
        assert!(matches!(state.overlay, Some(Overlay::Help { .. })));
        let text = text(&state);
        assert!(
            text.contains("F5, Ctrl+Enter, Ctrl+R") && text.contains("Send request"),
            "{text}"
        );
        insta::assert_snapshot!("help_rebound_send", text);
        press(&mut state, "Down Down Up");
        assert_eq!(state.overlay, Some(Overlay::Help { scroll: 1 }));
        press(&mut state, "?");
        assert!(state.overlay.is_none());
        press(&mut state, "F1 Esc");
        assert!(state.overlay.is_none());
    }

    #[test]
    fn question_mark_types_in_a_text_input() {
        let (_tmp, mut state) = shell(120, 36, "", false);
        crate::testing::open_raw_body(&mut state);
        press(&mut state, "a ? Enter b");
        assert!(state.overlay.is_none());
        assert_eq!(crate::testing::body_text(&state), "a?\nb");
        press(&mut state, "F1");
        assert!(
            matches!(state.overlay, Some(Overlay::Help { .. })),
            "F1 still opens help"
        );
        press(&mut state, "Esc");
        state.shell.focus = Focus::Url;
        press(&mut state, "?");
        assert!(state.overlay.is_none());
    }

    #[test]
    fn vim_modal_editing_in_the_body() {
        // Scenario "Modal editing in the body".
        let (_tmp, mut state) = shell(120, 36, "", true);
        crate::testing::open_raw_body(&mut state);
        assert_eq!(state.status.vim, Some(VimMode::Normal));
        press(&mut state, "i");
        assert_eq!(state.status.vim, Some(VimMode::Insert));
        assert!(text(&state).contains("INSERT"));
        press(&mut state, "a b c Esc");
        assert_eq!(crate::testing::body_text(&state), "abc");
        assert_eq!(state.status.vim, Some(VimMode::Normal));
        let screen = text(&state);
        assert!(
            screen.contains("NORMAL") && screen.contains(" abc"),
            "{screen}"
        );
        press(&mut state, "v");
        assert!(text(&state).contains("VISUAL"));
        press(&mut state, "Esc Tab");
        assert_eq!(
            state.shell.focus,
            Focus::Response,
            "Tab still moves focus in normal mode"
        );
        press(&mut state, "Shift+Tab ?");
        assert!(
            matches!(state.overlay, Some(Overlay::Help { .. })),
            "? is not text in normal mode"
        );
        let help = crate::help::lines(&state.keymap, Context::Editor, true, &state.ui);
        assert!(
            help.iter()
                .any(|l| l.to_string().contains("Vim text editing"))
        );
    }

    #[test]
    fn vim_navigation_in_the_sidebar() {
        let (_tmp, mut state) = shell(160, 45, "", true);
        let items = workbench::sidebar_rows(&state);
        assert!(!items.is_empty());
        state.shell.focus = Focus::Sidebar;
        press(&mut state, "j");
        assert_eq!(state.shell.selected, (1).min(items.len() - 1));
        press(&mut state, "G");
        assert_eq!(state.shell.selected, items.len() - 1);
        press(&mut state, "g g");
        assert_eq!(state.shell.selected, 0);
        press(&mut state, "g j");
        assert_eq!(
            state.shell.selected,
            (1).min(items.len() - 1),
            "a stray g is dropped"
        );
        press(&mut state, "l");
        assert_eq!(state.shell.focus, Focus::EditorBody);
        // With no exchange, the response viewer's keys do nothing.
        state.shell.focus = Focus::Response;
        press(&mut state, "j j Ctrl+D k Ctrl+U G g g h");
        assert_eq!(state.shell.focus, Focus::EditorBody);
    }

    #[test]
    fn default_list_keys_move_the_sidebar_selection() {
        let (tmp, _) = shell(160, 45, "", false);
        let dir = tmp.path().join("demo");
        for i in 0..20 {
            std::fs::write(
                dir.join(format!("r{i:02}.rman.yaml")),
                "version: 1\nkind: http\nname: R\nmethod: GET\nurl: http://localhost/\n",
            )
            .unwrap();
        }
        let ws = rustman_core::Workspace::load(&dir).unwrap();
        let mut state = State::with_screen(Screen::Loading(dir));
        update(&mut state, Action::WorkspaceLoaded(Ok(Box::new(ws))));
        update(
            &mut state,
            Action::Resize {
                width: 160,
                height: 12,
            },
        );
        let count = workbench::sidebar_rows(&state).len();
        assert!(count > 20);
        press(&mut state, "Down Down");
        assert_eq!(state.shell.selected, 2);
        let rows = page_size(&state, Pane::Sidebar);
        press(&mut state, "Tab Shift+Tab");
        assert_eq!(state.shell.focus, Focus::Sidebar);
        update(
            &mut state,
            Action::Key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)),
        );
        assert_eq!(state.shell.selected, 2 + rows);
        update(
            &mut state,
            Action::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
        );
        assert_eq!(state.shell.selected, count - 1);
        let scroll = usize::from(state.shell.scroll[Pane::Sidebar.index()]);
        assert_eq!(scroll, count - rows, "the selection is scrolled into view");
        update(
            &mut state,
            Action::Key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)),
        );
        assert_eq!((state.shell.selected, state.shell.scroll[0]), (0, 0));
    }

    #[test]
    fn ctrl_c_when_idle_says_how_to_quit() {
        // Scenario "Ctrl+C when idle".
        let (_tmp, mut state) = shell(120, 36, "", false);
        press(&mut state, "Ctrl+C");
        assert!(!state.quit);
        assert_eq!(toasts(&state), ["Press Ctrl+Q to quit"]);
    }

    #[test]
    fn secrets_reveal_toggles_the_badge() {
        let (_tmp, mut state) = shell(120, 36, "", false);
        press(&mut state, "Alt+r");
        assert!(state.status.secrets_visible);
        assert!(text(&state).contains("SECRETS VISIBLE"));
        press(&mut state, "Alt+r");
        assert!(!state.status.secrets_visible);
    }

    #[test]
    fn unbinding_a_default_key() {
        let (_tmp, mut state) = shell(120, 36, "[global]\n\"Ctrl+B\" = \"none\"\n", false);
        press(&mut state, "Ctrl+B");
        assert!(!state.shell.sidebar_hidden);
    }

    #[test]
    fn keymap_actions_without_a_workspace_do_not_panic() {
        let mut state = State::with_screen(Screen::Shell(Session::Scratch))
            .with_keymap(Keymap::new(true, &UserLayer::default()));
        update(
            &mut state,
            Action::Resize {
                width: 60,
                height: 20,
            },
        );
        for action in ActionId::all().filter(|a| *a != ActionId::Quit) {
            run(&mut state, action);
            render(&state, 60, 20);
            state.overlay = None;
        }
        assert!(!state.quit);
        let _ = Path::new("");
    }
}
