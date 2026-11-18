//! Drawing the request workbench: the collection tree in the sidebar, and
//! the editor with its request tab strip, method, URL, editor tabs, content
//! and the variable tooltip.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Padding, Paragraph};
use rustman_core::model::Request;

use crate::app::{Session, State};
use crate::focus::{Focus, Pane};
use crate::keymap::actions::ActionId;
use crate::layout::{editor_targets, label_rects, tab_strip};
use crate::pane::RenderCtx;
use crate::shell::request_tab_labels;
use crate::tabs::Tab;
use crate::theme::Token;
use crate::tree::{RowKind, TreeRow, method_token};
use crate::vars_view::{VarScope, highlight};
use crate::view::{block, empty_hint, hint};
use crate::workbench::{sidebar_rows, workspace};

/// Columns of a request badge (`DELETE` plus a space).
const BADGE_WIDTH: usize = 7;

// ---- sidebar -------------------------------------------------------------

/// The sidebar's lines: the filter, then the visible tree rows.
pub fn sidebar_lines(state: &State, session: &Session) -> Vec<Line<'static>> {
    let ui = &state.ui;
    let rows = sidebar_rows(state);
    let mut lines = Vec::new();
    if let Some(filter) = &state.shell.tree.filter {
        let cursor = if filter.editing { "_" } else { "" };
        let count = format!("  {} found", rows.len());
        lines.push(Line::from(vec![
            Span::styled(
                format!("/{}{cursor}", filter.query),
                ui.theme.fg(Token::Accent),
            ),
            Span::styled(count, ui.theme.fg(Token::Muted)),
        ]));
    }
    if rows.is_empty() && state.shell.tree.filter.is_none() {
        let text = match session {
            Session::Scratch => vec![
                "Scratch session.".to_owned(),
                "Nothing is saved.".to_owned(),
            ],
            Session::Workspace(_) => {
                let new = state.keymap.key(ActionId::SidebarNew);
                let next = new.map(|key| format!("Press {key} to add one."));
                ["No requests yet.".to_owned()]
                    .into_iter()
                    .chain(next)
                    .collect()
            }
        };
        lines.extend(text.iter().map(|line| hint(line, ui)));
        return lines;
    }
    let skip = usize::from(state.shell.scroll[Pane::Sidebar.index()]);
    let focused = state.shell.focus == Focus::Sidebar;
    lines.extend(rows.iter().enumerate().skip(skip).map(|(i, row)| {
        let selected = focused && i == state.shell.selected;
        tree_line(state, row, selected)
    }));
    lines
}

fn tree_line(state: &State, row: &TreeRow, selected: bool) -> Line<'static> {
    let ui = &state.ui;
    let text = ui.theme.fg(Token::Text);
    let mut spans = vec![Span::raw("  ".repeat(row.depth))];
    match &row.kind {
        RowKind::Folder { collapsed } => {
            let arrow = if *collapsed {
                ui.glyphs.tree_closed
            } else {
                ui.glyphs.tree_open
            };
            spans.push(Span::styled(format!("{arrow} "), ui.theme.fg(Token::Muted)));
            spans.push(Span::styled(
                row.label.clone(),
                text.add_modifier(Modifier::BOLD),
            ));
        }
        RowKind::Request(badge) => {
            let style = ui.theme.fg(badge.token).add_modifier(Modifier::BOLD);
            spans.push(Span::styled(format!("{:<BADGE_WIDTH$}", badge.text), style));
            spans.extend(matched_label(&row.label, &row.matched, text, ui));
            if state.tabs.is_dirty(&row.path) {
                let dirty = format!(" {}", ui.glyphs.dirty);
                spans.push(Span::styled(dirty, ui.theme.fg(Token::Warn)));
            }
        }
    }
    let line = Line::from(spans);
    if selected {
        line.style(ui.theme.selection())
    } else {
        line
    }
}

/// The label with the filter's matched characters highlighted.
fn matched_label(
    label: &str,
    matched: &[usize],
    base: Style,
    ui: &crate::config::UiOptions,
) -> Vec<Span<'static>> {
    let hit = ui
        .theme
        .fg(Token::Accent)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    label
        .chars()
        .enumerate()
        .map(|(i, c)| {
            let style = if matched.contains(&i) { hit } else { base };
            Span::styled(c.to_string(), style)
        })
        .collect()
}

// ---- editor --------------------------------------------------------------

/// The editor pane.
pub fn editor(frame: &mut Frame, rect: Rect, state: &State) {
    let focused = state.shell.focus.pane() == Pane::Editor;
    let ui = &state.ui;
    let mut pane = block("", ui).padding(Padding::horizontal(1));
    if focused {
        pane = pane.border_style(ui.theme.fg(Token::Focus));
    }
    frame.render_widget(pane, rect);
    tab_strip_line(frame, rect, state, focused);
    match state.tabs.active() {
        Some(tab) => open_tab(frame, rect, state, tab),
        None => placeholders(frame, rect, state),
    }
}

/// The request tabs in the top border, or the pane title with no tab open.
fn tab_strip_line(frame: &mut Frame, rect: Rect, state: &State, focused: bool) {
    let ui = &state.ui;
    let strip = tab_strip(rect);
    let title_style = |active: bool| match (active, focused) {
        (true, true) => ui.theme.fg(Token::Focus).add_modifier(Modifier::BOLD),
        (true, false) => ui.theme.fg(Token::Text).add_modifier(Modifier::BOLD),
        _ => ui.theme.fg(Token::Muted),
    };
    if state.tabs.list.is_empty() {
        let title = if focused {
            format!(" {} {} ", ui.glyphs.pointer, Pane::Editor.label())
        } else {
            format!(" {} ", Pane::Editor.label())
        };
        frame.render_widget(
            Paragraph::new(Span::styled(title, title_style(true))),
            strip,
        );
        return;
    }
    let labels = request_tab_labels(state);
    for (i, (label, at)) in labels.iter().zip(label_rects(strip, &labels)).enumerate() {
        let style = title_style(i == state.tabs.active);
        frame.render_widget(
            Paragraph::new(Span::styled(format!(" {label} "), style)),
            at,
        );
    }
}

/// What to do with no request open: `Open a request from the sidebar ·
/// press Ctrl+T for a new one`.
fn open_hint(state: &State) -> String {
    let new = state.keymap.key(ActionId::TabNew);
    let new = new.map(|key| format!("press {key} for a new one"));
    empty_hint("Open a request from the sidebar", new, &state.ui)
}

fn placeholders(frame: &mut Frame, rect: Rect, state: &State) {
    let ui = &state.ui;
    let open = open_hint(state);
    for (focus, target) in editor_targets(rect) {
        let text = match focus {
            Focus::Method => "METHOD",
            Focus::Url => "No request open",
            Focus::EditorTabs => "Params  Headers  Body  Auth  Assertions",
            _ => &open,
        };
        let style = if state.shell.focus == focus {
            ui.theme.selection()
        } else {
            ui.theme.fg(Token::Muted)
        };
        let line = Line::from(Span::styled(format!(" {text}"), style));
        frame.render_widget(Paragraph::new(line).style(style), target);
    }
}

fn open_tab(frame: &mut Frame, rect: Rect, state: &State, tab: &Tab) {
    let request = tab.editor.request().ok();
    let scope = scope(state, tab, request.as_ref());
    let [(_, method), (_, url), (_, bar), (_, content)] = editor_targets(rect);
    let focus = state.shell.focus;
    match tab.editor.method() {
        Some(name) => method_field(frame, method, name, state),
        None => {
            let notice = tab.editor.notice().unwrap_or_default();
            let row = method.union(url);
            let line = Span::styled(format!(" {notice}"), state.ui.theme.fg(Token::Warn));
            frame.render_widget(Paragraph::new(line), row);
        }
    }
    if let Some(area) = tab.editor.url() {
        url_field(frame, url, area, state, &scope);
    }
    tabs_bar(frame, bar, tab, state);
    let tooltip = tab.editor.variable_at_cursor(focus);
    let content = match &tooltip {
        Some(name) if content.height > 1 => {
            let tip = Rect {
                y: content.bottom() - 1,
                height: 1,
                ..content
            };
            let text = scope.tooltip(name, &state.ui.glyphs);
            let style = state
                .ui
                .theme
                .fg(Token::Info)
                .add_modifier(Modifier::ITALIC);
            frame.render_widget(Paragraph::new(Span::styled(format!(" {text}"), style)), tip);
            Rect {
                height: content.height - 1,
                ..content
            }
        }
        _ => content,
    };
    let ctx = RenderCtx {
        ui: &state.ui,
        focus,
        vars: &scope,
        keymap: &state.keymap,
    };
    tab.editor.render(frame, content, &ctx);
}

/// The variables the open tab's request sees.
fn scope<'a>(state: &'a State, tab: &'a Tab, request: Option<&'a Request>) -> VarScope<'a> {
    let Some(ws) = workspace(state) else {
        return VarScope::empty();
    };
    let own = request.and_then(Request::variables);
    VarScope::new(
        ws,
        state.status.environment.as_deref(),
        tab.path.as_deref(),
        own,
    )
    .with_revealed(crate::reveal::visible(state))
}

fn method_field(frame: &mut Frame, rect: Rect, method: &str, state: &State) {
    let ui = &state.ui;
    let token = method_token(method);
    let style = if state.shell.focus == Focus::Method {
        ui.theme.selection().add_modifier(Modifier::BOLD)
    } else {
        ui.theme.fg(token).add_modifier(Modifier::BOLD)
    };
    frame.render_widget(
        Paragraph::new(Span::styled(format!(" {method}"), style)),
        rect,
    );
}

/// The URL on one line, scrolled so the cursor shows, with variables
/// highlighted.
fn url_field(
    frame: &mut Frame,
    rect: Rect,
    area: &crate::textarea::TextArea,
    state: &State,
    scope: &VarScope,
) {
    let focused = state.shell.focus == Focus::Url;
    let text = area.lines().first().cloned().unwrap_or_default();
    let width = usize::from(rect.width.saturating_sub(1)).max(1);
    let col = area.cursor().col;
    let skip = if focused {
        (col + 1).saturating_sub(width)
    } else {
        0
    };
    let shown: String = text.chars().skip(skip).collect();
    let base = state.ui.theme.fg(Token::Text);
    let cursor = focused.then_some(col - skip);
    let mut line = if text.is_empty() && !focused {
        Line::styled(
            "Type a URL, e.g. {{base_url}}/users",
            state.ui.theme.fg(Token::Muted),
        )
    } else {
        highlight(&shown, base, cursor, scope, &state.ui)
    };
    line.spans.insert(0, Span::raw(" "));
    frame.render_widget(Paragraph::new(line), rect);
}

fn tabs_bar(frame: &mut Frame, rect: Rect, tab: &Tab, state: &State) {
    let ui = &state.ui;
    let labels: Vec<String> = tab.editor.tabs().iter().map(|l| (*l).to_owned()).collect();
    let focused = state.shell.focus == Focus::EditorTabs;
    for (i, (label, at)) in labels.iter().zip(label_rects(rect, &labels)).enumerate() {
        let active = i == tab.editor.tab();
        let style = match (active, focused) {
            (true, true) => ui.theme.selection().add_modifier(Modifier::BOLD),
            (true, false) => ui
                .theme
                .fg(Token::Accent)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            _ => ui.theme.fg(Token::Muted),
        };
        frame.render_widget(
            Paragraph::new(Span::styled(format!(" {label} "), style)),
            at,
        );
    }
}
