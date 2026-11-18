//! Shell behaviour: the sidebar, focus and toast commands, and the mouse
//! (spec: Responsive collapse, Default keymap, Mouse support, Toasts).

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use crate::app::{Overlay, Screen, Session, State};
use crate::focus::{Focus, Pane};
use crate::layout::{
    Breakpoint, Regions, SIDEBAR_DEFAULT, SIDEBAR_MAX, SIDEBAR_MIN, editor_targets, regions,
    switcher_tabs,
};
use crate::toast;

/// Layout and focus state that survives resizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellState {
    pub focus: Focus,
    /// Sidebar width in the wide layout and as an overlay (20–50).
    pub sidebar_width: u16,
    /// `Ctrl+B` hid the sidebar in the wide layout.
    pub sidebar_hidden: bool,
    /// `Ctrl+B` opened the sidebar overlay in the medium layout.
    pub overlay_open: bool,
    /// The sidebar border is being dragged.
    pub dragging: bool,
    /// Scroll offset per pane, indexed by [`Pane::index`].
    pub scroll: [u16; 3],
    /// The selected sidebar row.
    pub selected: usize,
}

impl Default for ShellState {
    fn default() -> Self {
        Self {
            focus: Focus::Sidebar,
            sidebar_width: SIDEBAR_DEFAULT,
            sidebar_hidden: false,
            overlay_open: false,
            dragging: false,
            scroll: [0; 3],
            selected: 0,
        }
    }
}

impl ShellState {
    /// Whether the sidebar is on screen (or, when narrow, reachable).
    pub fn sidebar_shown(&self, breakpoint: Breakpoint) -> bool {
        match breakpoint {
            Breakpoint::Wide => !self.sidebar_hidden,
            Breakpoint::Medium => self.overlay_open,
            Breakpoint::Narrow => true,
        }
    }

    pub fn set_sidebar_width(&mut self, width: u16) {
        self.sidebar_width = width.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
    }

    /// Move focus off a sidebar that is no longer shown.
    fn fix_focus(&mut self, breakpoint: Breakpoint) {
        if self.focus == Focus::Sidebar && !self.sidebar_shown(breakpoint) {
            self.focus = Focus::Sidebar.next(false);
        }
    }
}

/// Shell commands; the keymap's shell actions run these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    ToggleSidebar,
    FocusNext,
    FocusPrev,
    GrowSidebar,
    ShrinkSidebar,
    /// Close the open overlay, else the newest toast, else the sidebar
    /// overlay.
    Dismiss,
    ShowNotifications,
}

fn breakpoint(state: &State) -> Breakpoint {
    Breakpoint::of(state.area.width, state.area.height)
}

/// Run a shell command.
pub fn apply(state: &mut State, command: Command) {
    let bp = breakpoint(state);
    let shell = &mut state.shell;
    match command {
        Command::ToggleSidebar => toggle_sidebar(shell, bp),
        Command::FocusNext => shell.focus = shell.focus.next(shell.sidebar_shown(bp)),
        Command::FocusPrev => shell.focus = shell.focus.prev(shell.sidebar_shown(bp)),
        Command::GrowSidebar => shell.set_sidebar_width(shell.sidebar_width.saturating_add(1)),
        Command::ShrinkSidebar => shell.set_sidebar_width(shell.sidebar_width.saturating_sub(1)),
        Command::Dismiss => dismiss(state, bp),
        Command::ShowNotifications => state.overlay = Some(Overlay::Notifications),
    }
}

fn toggle_sidebar(shell: &mut ShellState, bp: Breakpoint) {
    match bp {
        Breakpoint::Wide => shell.sidebar_hidden = !shell.sidebar_hidden,
        Breakpoint::Medium => shell.overlay_open = !shell.overlay_open,
        Breakpoint::Narrow => {}
    }
    let showing = shell.sidebar_shown(bp) && shell.focus != Focus::Sidebar;
    if showing && bp != Breakpoint::Wide {
        shell.focus = Focus::Sidebar;
    } else if bp == Breakpoint::Narrow {
        shell.focus = Focus::Sidebar.next(false);
    }
    shell.fix_focus(bp);
}

fn dismiss(state: &mut State, bp: Breakpoint) {
    if state.overlay.take().is_some() || state.notifications.dismiss_newest() {
        return;
    }
    if bp == Breakpoint::Medium && state.shell.overlay_open {
        state.shell.overlay_open = false;
        state.shell.fix_focus(bp);
    }
}

/// Keep the layout state consistent after a resize; nothing else changes.
pub fn on_resize(state: &mut State) {
    let bp = breakpoint(state);
    state.shell.fix_focus(bp);
}

/// Handle a mouse event (ignored with `mouse = false`).
pub fn on_mouse(state: &mut State, event: MouseEvent) {
    if !state.ui.mouse || !matches!(state.screen, Screen::Shell(_)) {
        return;
    }
    let (column, row) = (event.column, event.row);
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => click(state, column, row),
        MouseEventKind::Drag(MouseButton::Left) if state.shell.dragging => {
            state.shell.set_sidebar_width(column.saturating_add(1));
        }
        MouseEventKind::Up(MouseButton::Left) => state.shell.dragging = false,
        MouseEventKind::ScrollDown => scroll(state, column, row, true),
        MouseEventKind::ScrollUp => scroll(state, column, row, false),
        _ => {}
    }
}

fn click(state: &mut State, column: u16, row: u16) {
    let rects = toast::toast_rects(state.area, state.notifications.visible());
    let at = ratatui::layout::Position::new(column, row);
    if let Some(index) = rects.iter().position(|r| r.contains(at)) {
        state.notifications.dismiss(index);
        return;
    }
    if state.overlay.is_some() {
        return;
    }
    let regions = regions(state.area, &state.shell);
    if regions.sidebar_border() == Some(column) {
        state.shell.dragging = true;
        return;
    }
    if let Some(focus) = focus_at(&regions, at) {
        state.shell.focus = focus;
        if focus.pane() != Pane::Sidebar {
            state.shell.overlay_open = false;
        }
    }
}

/// What a click at `at` focuses: a switcher label, an editor field, or the
/// pane under it.
fn focus_at(regions: &Regions, at: ratatui::layout::Position) -> Option<Focus> {
    if let Some(bar) = regions.switcher.filter(|r| r.contains(at)) {
        return switcher_tabs(bar)
            .into_iter()
            .find(|(_, r)| r.contains(at))
            .map(|(pane, _)| pane.entry());
    }
    let pane = regions.pane_at(at.x, at.y)?;
    if pane != Pane::Editor {
        return Some(pane.entry());
    }
    let editor = regions.editor?;
    let target = editor_targets(editor)
        .into_iter()
        .find(|(_, r)| r.contains(at));
    Some(target.map_or(Focus::EditorBody, |(focus, _)| focus))
}

fn scroll(state: &mut State, column: u16, row: u16, down: bool) {
    let regions = regions(state.area, &state.shell);
    let Some(pane) = regions.pane_at(column, row) else {
        return;
    };
    let limit = match pane {
        Pane::Sidebar => sidebar_items(&state.screen).len().saturating_sub(1),
        _ => usize::from(u16::MAX),
    };
    let offset = &mut state.shell.scroll[pane.index()];
    *offset = if down {
        offset
            .saturating_add(1)
            .min(u16::try_from(limit).unwrap_or(u16::MAX))
    } else {
        offset.saturating_sub(1)
    };
}

/// The read-only request list in the sidebar.
pub fn sidebar_items(screen: &Screen) -> Vec<String> {
    match screen {
        Screen::Shell(Session::Workspace(ws)) => ws
            .requests()
            .iter()
            .map(|r| r.entry.path.display().to_string().replace('\\', "/"))
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Action, update};
    use crate::toast::Severity;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Rect;

    fn shell_state(width: u16, height: u16) -> State {
        let mut state = State::with_screen(Screen::Shell(Session::Scratch));
        update(&mut state, Action::Resize { width, height });
        state
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Action {
        Action::Key(KeyEvent::new(code, modifiers))
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Action {
        Action::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    const LEFT: MouseButton = MouseButton::Left;

    #[test]
    fn tab_and_shift_tab_cycle_focus() {
        let mut state = shell_state(160, 45);
        let mut seen = vec![state.shell.focus];
        for _ in 0..6 {
            update(&mut state, key(KeyCode::Tab, KeyModifiers::NONE));
            seen.push(state.shell.focus);
        }
        assert_eq!(&seen[..6], &Focus::RING);
        assert_eq!(seen[6], Focus::Sidebar);
        update(&mut state, key(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(state.shell.focus, Focus::Response);
    }

    #[test]
    fn ctrl_b_toggles_the_sidebar_per_breakpoint() {
        let ctrl_b = || key(KeyCode::Char('b'), KeyModifiers::CONTROL);
        let mut state = shell_state(160, 45);
        update(&mut state, ctrl_b());
        assert!(state.shell.sidebar_hidden);
        assert_eq!(state.shell.focus, Focus::Method);

        let mut state = shell_state(80, 24);
        assert_eq!(
            state.shell.focus,
            Focus::Method,
            "hidden sidebar loses focus"
        );
        update(&mut state, ctrl_b());
        assert!(state.shell.overlay_open);
        assert_eq!(state.shell.focus, Focus::Sidebar);
        update(&mut state, ctrl_b());
        assert!(!state.shell.overlay_open);
        assert_eq!(state.shell.focus, Focus::Method);

        let mut state = shell_state(60, 20);
        update(&mut state, ctrl_b());
        assert_eq!(state.shell.focus, Focus::Method);
        update(&mut state, ctrl_b());
        assert_eq!(state.shell.focus, Focus::Sidebar);
    }

    #[test]
    fn keyboard_resize_is_clamped() {
        let mut state = shell_state(160, 45);
        for _ in 0..40 {
            update(&mut state, key(KeyCode::Right, KeyModifiers::ALT));
        }
        assert_eq!(state.shell.sidebar_width, SIDEBAR_MAX);
        for _ in 0..40 {
            update(&mut state, key(KeyCode::Left, KeyModifiers::ALT));
        }
        assert_eq!(state.shell.sidebar_width, SIDEBAR_MIN);
    }

    #[test]
    fn esc_closes_the_log_then_toasts_then_the_overlay() {
        let mut state = shell_state(80, 24);
        update(&mut state, key(KeyCode::Char('b'), KeyModifiers::CONTROL));
        update(&mut state, Action::Notify(Severity::Error, "boom".into()));
        apply(&mut state, Command::ShowNotifications);
        let esc = || key(KeyCode::Esc, KeyModifiers::NONE);
        update(&mut state, esc());
        assert!(state.overlay.is_none());
        assert_eq!(state.notifications.visible().len(), 1);
        update(&mut state, esc());
        assert_eq!(state.notifications.visible().len(), 0);
        assert!(state.shell.overlay_open);
        update(&mut state, esc());
        assert!(!state.shell.overlay_open);
    }

    #[test]
    fn clicking_focuses_panes_and_editor_fields() {
        let mut state = shell_state(160, 45);
        let r = regions(state.area, &state.shell);
        let response = r.response.unwrap();
        update(
            &mut state,
            mouse(MouseEventKind::Down(LEFT), response.x + 3, 10),
        );
        assert_eq!(state.shell.focus, Focus::Response);
        let [_, (_, url), (_, tabs), _] = editor_targets(r.editor.unwrap());
        update(
            &mut state,
            mouse(MouseEventKind::Down(LEFT), url.x + 2, url.y),
        );
        assert_eq!(state.shell.focus, Focus::Url);
        update(
            &mut state,
            mouse(MouseEventKind::Down(LEFT), tabs.x, tabs.y),
        );
        assert_eq!(state.shell.focus, Focus::EditorTabs);
        update(&mut state, mouse(MouseEventKind::Down(LEFT), 3, 3));
        assert_eq!(state.shell.focus, Focus::Sidebar);
    }

    #[test]
    fn clicking_the_switcher_bar_switches_panes() {
        let mut state = shell_state(60, 20);
        let tabs = switcher_tabs(Rect::new(0, 0, 60, 1));
        let (_, response) = tabs[2];
        update(
            &mut state,
            mouse(MouseEventKind::Down(LEFT), response.x + 1, 0),
        );
        assert_eq!(state.shell.focus, Focus::Response);
        let r = regions(state.area, &state.shell);
        assert!(r.response.is_some() && r.editor.is_none());
    }

    #[test]
    fn clicking_outside_the_overlay_closes_it() {
        let mut state = shell_state(100, 30);
        update(&mut state, key(KeyCode::Char('b'), KeyModifiers::CONTROL));
        update(&mut state, mouse(MouseEventKind::Down(LEFT), 70, 20));
        assert!(!state.shell.overlay_open);
        assert_eq!(state.shell.focus, Focus::Response);
    }

    #[test]
    fn dragging_the_sidebar_border_resizes_within_limits() {
        let mut state = shell_state(160, 45);
        update(&mut state, mouse(MouseEventKind::Down(LEFT), 27, 5));
        assert!(state.shell.dragging);
        update(&mut state, mouse(MouseEventKind::Drag(LEFT), 39, 5));
        assert_eq!(state.shell.sidebar_width, 40);
        update(&mut state, mouse(MouseEventKind::Drag(LEFT), 120, 5));
        assert_eq!(state.shell.sidebar_width, SIDEBAR_MAX);
        update(&mut state, mouse(MouseEventKind::Drag(LEFT), 2, 5));
        assert_eq!(state.shell.sidebar_width, SIDEBAR_MIN);
        update(&mut state, mouse(MouseEventKind::Up(LEFT), 2, 5));
        assert!(!state.shell.dragging);
        update(&mut state, mouse(MouseEventKind::Drag(LEFT), 30, 5));
        assert_eq!(
            state.shell.sidebar_width, SIDEBAR_MIN,
            "no drag after release"
        );
    }

    #[test]
    fn the_wheel_scrolls_the_pane_under_the_pointer() {
        let mut state = shell_state(160, 45);
        let response = regions(state.area, &state.shell).response.unwrap();
        for _ in 0..3 {
            update(
                &mut state,
                mouse(MouseEventKind::ScrollDown, response.x + 1, 5),
            );
        }
        update(
            &mut state,
            mouse(MouseEventKind::ScrollUp, response.x + 1, 5),
        );
        assert_eq!(state.shell.scroll[Pane::Response.index()], 2);
        update(&mut state, mouse(MouseEventKind::ScrollDown, 1, 5));
        assert_eq!(state.shell.scroll[Pane::Sidebar.index()], 0, "empty list");
    }

    #[test]
    fn clicking_a_toast_dismisses_it() {
        let mut state = shell_state(160, 45);
        update(&mut state, Action::Notify(Severity::Warn, "first".into()));
        update(&mut state, Action::Notify(Severity::Warn, "second".into()));
        let rects = toast::toast_rects(state.area, state.notifications.visible());
        update(
            &mut state,
            mouse(MouseEventKind::Down(LEFT), rects[0].x + 1, rects[0].y),
        );
        let left: Vec<_> = state
            .notifications
            .visible()
            .map(|t| t.message.clone())
            .collect();
        assert_eq!(left, ["first"]);
    }

    #[test]
    fn mouse_false_ignores_every_mouse_event() {
        let mut state = shell_state(160, 45);
        state.ui.mouse = false;
        let before = state.shell.clone();
        for kind in [
            MouseEventKind::Down(LEFT),
            MouseEventKind::Drag(LEFT),
            MouseEventKind::ScrollDown,
        ] {
            update(&mut state, mouse(kind, 27, 5));
        }
        update(&mut state, mouse(MouseEventKind::Down(LEFT), 100, 5));
        assert_eq!(state.shell, before);
    }

    #[test]
    fn resizing_keeps_layout_state() {
        let mut state = shell_state(160, 45);
        update(&mut state, key(KeyCode::Right, KeyModifiers::ALT));
        update(&mut state, key(KeyCode::Tab, KeyModifiers::NONE));
        update(&mut state, key(KeyCode::Tab, KeyModifiers::NONE));
        update(
            &mut state,
            Action::Resize {
                width: 80,
                height: 24,
            },
        );
        update(
            &mut state,
            Action::Resize {
                width: 160,
                height: 45,
            },
        );
        assert_eq!(state.shell.focus, Focus::Url);
        assert_eq!(state.shell.sidebar_width, SIDEBAR_DEFAULT + 1);
    }

    #[test]
    fn resizing_keeps_unsaved_body_edits() {
        // Scenario "Resize keeps edits": 160x45 -> 80x24 -> 160x45.
        let mut state = shell_state(160, 45);
        state.shell.focus = Focus::EditorBody;
        for c in "{\"a\":1}".chars() {
            update(&mut state, key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        update(&mut state, key(KeyCode::Left, KeyModifiers::NONE));
        let (text, cursor) = (state.body.text(), state.body.cursor());
        assert_eq!(text, "{\"a\":1}");
        for (width, height) in [(80, 24), (160, 45)] {
            update(&mut state, Action::Resize { width, height });
            assert_eq!(state.body.text(), text, "{width}x{height}");
            assert_eq!(state.body.cursor(), cursor, "{width}x{height}");
            assert_eq!(state.shell.focus, Focus::EditorBody);
        }
    }
}
