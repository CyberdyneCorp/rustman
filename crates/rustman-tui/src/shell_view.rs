//! Drawing the shell: panes per breakpoint, the pane switcher, the size
//! notice, the status bar, toasts and the notification log.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Padding, Paragraph, Wrap};

use crate::app::{Overlay, Session, State};
use crate::config::UiOptions;
use crate::focus::{Focus, Pane};
use crate::help;
use crate::keymap::Context;
use crate::layout::{regions, switcher_tabs};
use crate::palette;
use crate::response;
use crate::status;
use crate::theme::Token;
use crate::toast::{Severity, toast_rects};
use crate::view::{block, dialog, hint};
use crate::workbench_view;

/// The notice shown below the recommended terminal size.
pub const SIZE_NOTICE: &str = "Smaller than the recommended minimum of 80x24";

/// Draw the shell for `session`.
pub fn shell(frame: &mut Frame, state: &State, session: &Session) {
    let regions = regions(frame.area(), &state.shell);
    if let Some(rect) = regions.editor {
        workbench_view::editor(frame, rect, state);
    }
    if let Some(rect) = regions.response {
        let ctx = response::view::Ctx {
            ui: &state.ui,
            focused: state.shell.focus.pane() == Pane::Response,
            now: state.now,
        };
        response::view::render(frame, rect, state.tabs.active(), &ctx);
    }
    if let Some(rect) = regions.sidebar {
        if regions.sidebar_overlay {
            frame.render_widget(Clear, rect);
        }
        pane(
            frame,
            rect,
            Pane::Sidebar,
            workbench_view::sidebar_lines(state, session),
            state,
        );
    }
    if let Some(rect) = regions.switcher {
        switcher(frame, rect, state);
    }
    if let Some(rect) = regions.notice {
        let notice = Span::styled(SIZE_NOTICE, state.ui.theme.fg(Token::Warn));
        frame.render_widget(Paragraph::new(Line::from(notice)), rect);
    }
    status::render(
        frame,
        regions.status,
        &session.title(),
        &state.status,
        &state.ui,
    );
}

/// A bordered pane; the focused one has the focus colour and a pointer.
fn pane(frame: &mut Frame, rect: Rect, pane: Pane, lines: Vec<Line<'static>>, state: &State) {
    let ui = &state.ui;
    let focused = state.shell.focus.pane() == pane;
    let title = if focused {
        format!(" {} {} ", ui.glyphs.pointer, pane.label())
    } else {
        format!(" {} ", pane.label())
    };
    let mut block = block(&title, ui).padding(Padding::horizontal(1));
    if focused {
        block = block
            .border_style(ui.theme.fg(Token::Focus))
            .title_style(ui.theme.fg(Token::Focus).add_modifier(Modifier::BOLD));
    }
    frame.render_widget(Paragraph::new(lines).block(block), rect);
}

fn switcher(frame: &mut Frame, bar: Rect, state: &State) {
    let ui = &state.ui;
    for (pane, rect) in switcher_tabs(bar) {
        let style = if state.shell.focus.pane() == pane {
            ui.theme.selection().add_modifier(Modifier::BOLD)
        } else {
            ui.theme.fg(Token::Muted)
        };
        let label = Span::styled(format!(" {} ", pane.label()), style);
        frame.render_widget(Paragraph::new(Line::from(label)), rect);
    }
}

fn symbol(severity: Severity, ui: &UiOptions) -> &'static str {
    match severity {
        Severity::Info => ui.glyphs.info,
        Severity::Success => ui.glyphs.ok,
        Severity::Warn => ui.glyphs.warn,
        Severity::Error => ui.glyphs.fail,
    }
}

/// Toasts in the top-right corner, newest on top.
pub fn toasts(frame: &mut Frame, state: &State) {
    let ui = &state.ui;
    let rects = toast_rects(frame.area(), state.notifications.visible());
    for (toast, rect) in state.notifications.visible().zip(rects) {
        let style = ui.theme.fg(toast.severity.token());
        let title = format!(
            " {} {} ",
            symbol(toast.severity, ui),
            toast.severity.label()
        );
        let block = block(&title, ui)
            .border_style(style)
            .title_style(style.add_modifier(Modifier::BOLD))
            .padding(Padding::horizontal(1));
        frame.render_widget(Clear, rect);
        let text = Paragraph::new(toast.message.clone())
            .block(block)
            .wrap(Wrap { trim: true });
        frame.render_widget(text, rect);
    }
}

/// The open modal, if any.
pub fn overlay(frame: &mut Frame, state: &State) {
    match &state.overlay {
        None => {}
        Some(Overlay::Notifications) => notification_log(frame, state),
        Some(Overlay::Help { scroll }) => {
            let context = Context::of(state.shell.focus);
            let vim_text = state.vim.is_some() && state.shell.focus == Focus::EditorBody;
            let lines = help::lines(&state.keymap, context, vim_text, &state.ui);
            help::render(frame, frame.area(), lines, *scroll, &state.ui);
        }
        Some(Overlay::Palette(palette)) => palette::render(
            frame,
            frame.area(),
            palette,
            crate::reveal::visible(state),
            &state.ui,
        ),
        Some(Overlay::Confirm(confirm)) => confirm.render(frame, &state.ui),
        Some(Overlay::Input(input)) => input.render(frame, &state.ui),
        Some(Overlay::Exchanges(picker)) => picker.render(frame, &state.ui),
        Some(Overlay::Diff(diff)) => diff.render(frame, &state.ui),
    }
}

fn notification_log(frame: &mut Frame, state: &State) {
    let ui = &state.ui;
    let room = usize::from(frame.area().height.saturating_sub(4));
    let mut lines: Vec<Line<'static>> = state
        .notifications
        .log()
        .take(room.saturating_sub(2))
        .map(|entry| {
            let style = ui.theme.fg(entry.severity.token());
            let label = format!("{} {}", symbol(entry.severity, ui), entry.severity.label());
            Line::from(vec![
                Span::styled(label, style),
                Span::raw(format!(" {}", entry.message)),
            ])
        })
        .collect();
    if lines.is_empty() {
        lines.push(hint("No notifications yet.", ui));
    }
    lines.extend([Line::default(), hint("Esc close", ui)]);
    dialog(frame, frame.area(), " Notifications ", lines, ui);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Action, Screen, update};
    use crate::config::{Overrides, TuiConfig, UiOptions};
    use crate::glyphs::ASCII;
    use crate::shell::{Command, apply};
    use crate::snapshot::{buffer_ansi, buffer_text, render, style_of};
    use crate::theme::{ColorMode, Theme, ThemeName};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// A workspace shell at `width` x `height`; the tempdir must outlive it.
    fn workspace_state(width: u16, height: u16) -> (tempfile::TempDir, State) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("demo");
        rustman_core::init::init(&dir, "demo", false).unwrap();
        let ws = rustman_core::Workspace::load(&dir).unwrap();
        let mut state = State::with_screen(Screen::Loading(dir));
        update(&mut state, Action::WorkspaceLoaded(Ok(Box::new(ws))));
        update(&mut state, Action::Resize { width, height });
        (tmp, state)
    }

    fn text(state: &State) -> String {
        buffer_text(&render(state, state.area.width, state.area.height))
    }

    #[test]
    fn shell_160x45() {
        // Scenario "Wide terminal".
        let (_tmp, state) = workspace_state(160, 45);
        let text = text(&state);
        for pane in ["Sidebar", "Request", "Response"] {
            assert!(text.contains(pane), "{pane} missing:\n{text}");
        }
        assert!(text.contains("env: local"), "{text}");
        insta::assert_snapshot!(text);
    }

    #[test]
    fn shell_80x24() {
        // Scenario "80x24 terminal": stacked, sidebar hidden.
        let (_tmp, state) = workspace_state(80, 24);
        let text = text(&state);
        assert!(!text.contains("Sidebar"), "{text}");
        insta::assert_snapshot!(text);
    }

    #[test]
    fn shell_80x24_sidebar_overlay() {
        let (_tmp, mut state) = workspace_state(80, 24);
        let ctrl_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
        update(&mut state, Action::Key(ctrl_b));
        let text = text(&state);
        assert!(text.contains("› Sidebar"), "{text}");
        insta::assert_snapshot!(text);
    }

    #[test]
    fn shell_60x20() {
        // Scenario "Very small terminal": one pane, switcher bar, notice.
        let (_tmp, state) = workspace_state(60, 20);
        let text = text(&state);
        assert!(text.contains(SIZE_NOTICE), "{text}");
        assert!(text.contains("80x24"));
        let panes = ["│ Request", "│ Response", "Sidebar ─"];
        assert_eq!(
            panes.iter().filter(|p| text.contains(*p)).count(),
            1,
            "{text}"
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn shell_wide_with_resized_sidebar_and_hidden_sidebar() {
        let (_tmp, mut state) = workspace_state(160, 45);
        for _ in 0..12 {
            apply(&mut state, Command::GrowSidebar);
        }
        let buffer = render(&state, 160, 45);
        assert_eq!(buffer.cell((39, 1)).unwrap().symbol(), "│");
        apply(&mut state, Command::ToggleSidebar);
        assert!(!text(&state).contains("Sidebar"));
    }

    #[test]
    fn the_sidebar_scrolls() {
        let (_tmp, mut state) = workspace_state(160, 45);
        let first = crate::workbench::sidebar_rows(&state)[0].label.clone();
        assert!(text(&state).contains(&first));
        state.shell.scroll[Pane::Sidebar.index()] = 1;
        assert!(!text(&state).contains(&first));
    }

    #[test]
    fn scratch_sessions_say_nothing_is_saved() {
        let mut state = State::with_screen(Screen::Shell(Session::Scratch));
        update(
            &mut state,
            Action::Resize {
                width: 160,
                height: 45,
            },
        );
        let text = text(&state);
        assert!(text.contains("Nothing is saved."), "{text}");
        assert!(text.contains(" Scratch session │ no environment"), "{text}");
    }

    #[test]
    fn shell_no_color() {
        let (_tmp, mut state) = workspace_state(80, 24);
        state.ui = UiOptions::resolve(
            &TuiConfig::default(),
            Overrides::default(),
            ColorMode::NoColor,
        );
        state.notify(Severity::Error, "Permission denied (os error 13)");
        let ansi = buffer_ansi(&render(&state, 80, 24));
        assert!(!ansi.contains(";38;") && !ansi.contains(";48;"), "{ansi}");
        assert!(!ansi.contains("[0;3") && !ansi.contains("[0;4"), "{ansi}");
        assert!(ansi.contains("✖ error"), "{ansi}");
        insta::assert_snapshot!(ansi);
    }

    #[test]
    fn shell_ascii() {
        let (_tmp, mut state) = workspace_state(80, 24);
        state.ui.glyphs = ASCII;
        state.notify(Severity::Warn, "careful");
        apply(&mut state, Command::ToggleSidebar);
        let text = text(&state);
        assert!(text.is_ascii(), "{text}");
        assert!(text.contains("+-") && text.contains('|'));
        insta::assert_snapshot!(text);
    }

    #[test]
    fn ascii_prompt_and_narrow_shell() {
        let (_tmp, mut state) = workspace_state(60, 20);
        state.ui.glyphs = ASCII;
        assert!(text(&state).is_ascii());
        let mut prompt = State::with_screen(Screen::Prompt(crate::app::Prompt::new("/x".into())));
        prompt.ui.glyphs = ASCII;
        assert!(buffer_text(&render(&prompt, 80, 24)).is_ascii());
    }

    #[test]
    fn themes_colour_the_focused_border() {
        for name in ThemeName::ALL {
            let (_tmp, mut state) = workspace_state(160, 45);
            state.ui.theme = Theme::new(name, ColorMode::TrueColor);
            let buffer = render(&state, 160, 45);
            let focus = state.ui.theme.color(Token::Focus);
            assert_eq!(style_of(&buffer, "› Sidebar").unwrap().fg, focus, "{name}");
        }
    }

    #[test]
    fn toasts_stack_three_deep() {
        let (_tmp, mut state) = workspace_state(120, 30);
        for (severity, message) in [
            (Severity::Info, "one"),
            (Severity::Success, "Saved users/get-user.rman.yaml"),
            (
                Severity::Warn,
                "Unknown action `request.sned` in keybindings.toml",
            ),
            (Severity::Error, "Permission denied (os error 13)"),
        ] {
            state.notify(severity, message);
        }
        let text = text(&state);
        assert!(!text.contains("one"), "{text}");
        insta::assert_snapshot!(text);
    }

    #[test]
    fn notification_log() {
        let (_tmp, mut state) = workspace_state(80, 24);
        apply(&mut state, Command::ShowNotifications);
        assert!(text(&state).contains("No notifications yet."));
        state.notify(Severity::Info, "Copied 42");
        state.notify(Severity::Error, "Permission denied (os error 13)");
        state
            .notifications
            .expire(state.now + std::time::Duration::from_secs(11));
        insta::assert_snapshot!(text(&state));
    }

    #[test]
    fn tiny_shells_do_not_panic() {
        let (_tmp, mut state) = workspace_state(160, 45);
        state.notify(Severity::Error, "boom");
        apply(&mut state, Command::ShowNotifications);
        for (w, h) in [(0, 0), (1, 1), (10, 3), (79, 23), (200, 2)] {
            update(
                &mut state,
                Action::Resize {
                    width: w,
                    height: h,
                },
            );
            render(&state, w, h);
        }
    }
}
