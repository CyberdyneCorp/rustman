//! Drawing the MCP inspector: the server list with connection status, the
//! session's server info and capabilities, the tabs and their lists, the
//! argument form and result view, the key hints, and the trust and
//! approval dialogs over the pane.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Padding, Paragraph, Wrap};
use rustman_mcp::client::Operation;
use rustman_mcp::client::output::hints;

use super::form::Form;
use super::{Conn, Focus, Live, McpPane, Tab};
use crate::app::State;
use crate::config::UiOptions;
use crate::theme::Token;
use crate::view::{block, dialog, hint};

/// Draw the pane into `area`.
pub fn render(frame: &mut Frame, area: Rect, state: &State) {
    let ui = &state.ui;
    let pane = &state.mcp;
    frame.render_widget(Clear, area);
    let outer = block(" MCP inspector ", ui)
        .border_style(ui.theme.fg(Token::Focus))
        .padding(Padding::horizontal(1));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);
    let [status, body, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(inner);
    frame.render_widget(Paragraph::new(status_line(pane, ui)), status);
    let [servers, session] =
        Layout::horizontal([Constraint::Length(24), Constraint::Min(10)]).areas(body);
    render_servers(frame, servers, pane, ui);
    match &pane.session {
        Some(live) => render_session(frame, session, pane, live, ui),
        None => {
            let block = block(" Session ", ui);
            let text = "Select a server and press Enter to connect";
            let inner = block.inner(session);
            frame.render_widget(block, session);
            frame.render_widget(Paragraph::new(muted(text, ui)), inner);
        }
    }
    frame.render_widget(Paragraph::new(hint(&hint_text(pane), ui)), hints);
    render_dialogs(frame, area, pane, ui);
}

fn muted(text: &str, ui: &UiOptions) -> Line<'static> {
    Line::from(Span::styled(text.to_owned(), ui.theme.fg(Token::Muted)))
}

/// The waiting approvals and the call in flight.
fn status_line(pane: &McpPane, ui: &UiOptions) -> Line<'static> {
    let mut spans = Vec::new();
    if !pane.approvals.is_empty() {
        let text = format!(" {} APPROVAL PENDING ", pane.approvals.len());
        spans.push(Span::styled(text, ui.theme.badge(Token::Warn)));
        spans.push(Span::raw(" "));
    }
    match pane.session.as_ref().and_then(|s| s.busy.as_ref()) {
        Some(busy) => {
            let percent = busy
                .progress
                .as_ref()
                .and_then(|p| p.percent())
                .map(|p| format!(" {p:.0}%"))
                .unwrap_or_default();
            let text = format!("{}{}{percent}", busy.title, ui.glyphs.ellipsis);
            spans.push(Span::styled(text, ui.theme.fg(Token::Info)));
        }
        None => {
            let count = pane.servers.len();
            let noun = if count == 1 { "server" } else { "servers" };
            spans.push(Span::styled(
                format!("{count} {noun}"),
                ui.theme.fg(Token::Muted),
            ));
        }
    }
    Line::from(spans)
}

fn render_servers(frame: &mut Frame, rect: Rect, pane: &McpPane, ui: &UiOptions) {
    let focused = pane.focus == Focus::Servers;
    let block = focus_block(" Servers ", focused, ui);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if pane.servers.is_empty() {
        let text = "No servers: define [mcp_servers] in rustman.toml";
        let paragraph = Paragraph::new(muted(text, ui)).wrap(Wrap { trim: true });
        frame.render_widget(paragraph, inner);
        return;
    }
    let lines: Vec<Line> = pane
        .servers
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let (mark, token) = match &row.conn {
                Conn::Idle => ("○", Token::Muted),
                Conn::Connecting => ("…", Token::Info),
                Conn::Connected => ("●", Token::Ok),
                Conn::Failed(_) => ("✗", Token::Error),
            };
            let selected = i == pane.selected;
            let pointer = if selected { ui.glyphs.pointer } else { " " };
            let name = format!("{pointer} {} ", row.name);
            let style = if selected && focused {
                ui.theme.selection()
            } else {
                ui.theme.fg(Token::Text)
            };
            Line::from(vec![
                Span::styled(name, style),
                Span::styled(format!("{mark} "), ui.theme.fg(token)),
                Span::styled(row.transport, ui.theme.fg(Token::Muted)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn focus_block(title: &str, focused: bool, ui: &UiOptions) -> ratatui::widgets::Block<'static> {
    let token = if focused { Token::Focus } else { Token::Border };
    block(title, ui).border_style(ui.theme.fg(token))
}

fn render_session(frame: &mut Frame, rect: Rect, pane: &McpPane, live: &Live, ui: &UiOptions) {
    let info = &live.info;
    let title = format!(
        " {} {} {} {} {} {} ",
        info.server_title.as_deref().unwrap_or(&info.server_name),
        info.server_version,
        ui.glyphs.dot,
        info.transport.label(),
        ui.glyphs.dot,
        info.protocol_version
    );
    let block = focus_block(&title, pane.focus != Focus::Servers, ui);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let [caps, tabs, content] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(inner);
    frame.render_widget(Paragraph::new(capability_line(live, ui)), caps);
    frame.render_widget(Paragraph::new(tab_line(pane.tab, ui)), tabs);
    match pane.tab {
        Tab::Notifications => render_log(
            frame,
            content,
            &live.notifications,
            "No notifications yet",
            ui,
        ),
        Tab::Stderr => render_log(frame, content, &live.stderr, "No stderr output", ui),
        _ => render_items(frame, content, pane, live, ui),
    }
}

/// `tools ✓  resources ✓ (subscribe)  prompts ✗  logging ✓  completions ✗`.
fn capability_line(live: &Live, ui: &UiOptions) -> Line<'static> {
    let caps = &live.info.capabilities;
    let resources = caps.resources.map(|r| r.subscribe);
    let entries = [
        ("tools", caps.tools.is_some(), ""),
        (
            "resources",
            resources.is_some(),
            if resources == Some(true) {
                " (subscribe)"
            } else {
                ""
            },
        ),
        ("prompts", caps.prompts.is_some(), ""),
        ("logging", caps.logging, ""),
        ("completions", caps.completions, ""),
    ];
    let mut spans = Vec::new();
    for (name, declared, extra) in entries {
        let (mark, token) = if declared {
            (ui.glyphs.ok, Token::Ok)
        } else {
            (ui.glyphs.fail, Token::Muted)
        };
        spans.push(Span::styled(format!("{name} "), ui.theme.fg(Token::Text)));
        spans.push(Span::styled(format!("{mark}{extra}  "), ui.theme.fg(token)));
    }
    Line::from(spans)
}

fn tab_line(current: Tab, ui: &UiOptions) -> Line<'static> {
    let spans: Vec<Span> = Tab::ALL
        .iter()
        .enumerate()
        .flat_map(|(i, &tab)| {
            let text = format!("{} {}", i + 1, tab.label());
            let style = if tab == current {
                ui.theme
                    .fg(Token::Accent)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                ui.theme.fg(Token::Muted)
            };
            [Span::styled(text, style), Span::raw("  ")]
        })
        .collect();
    Line::from(spans)
}

fn render_log(
    frame: &mut Frame,
    rect: Rect,
    log: &std::collections::VecDeque<String>,
    empty: &str,
    ui: &UiOptions,
) {
    if log.is_empty() {
        frame.render_widget(Paragraph::new(muted(empty, ui)), rect);
        return;
    }
    let rows = usize::from(rect.height);
    let lines: Vec<Line> = log
        .iter()
        .skip(log.len().saturating_sub(rows))
        .map(|l| Line::from(l.clone()))
        .collect();
    frame.render_widget(Paragraph::new(lines), rect);
}

/// The disabled reason of the tab, when the server lacks its capability.
fn disabled(live: &Live, tab: Tab) -> Option<String> {
    let operation = match tab {
        Tab::Tools => Operation::ListTools,
        Tab::Resources => Operation::ListResources,
        Tab::Prompts => Operation::ListPrompts,
        Tab::Notifications | Tab::Stderr => return None,
    };
    live.info
        .capabilities
        .check(operation)
        .err()
        .map(|d| d.reason)
}

/// How many rows the current tab's list has.
pub fn item_count(pane: &McpPane) -> usize {
    item_lines(pane).len()
}

fn item_lines(pane: &McpPane) -> Vec<String> {
    let Some(live) = &pane.session else {
        return Vec::new();
    };
    let lists = &live.lists;
    match pane.tab {
        Tab::Tools => lists
            .tools
            .iter()
            .map(|t| {
                let hints = hints(t);
                if hints.is_empty() {
                    t.name.to_string()
                } else {
                    format!("{} [{}]", t.name, hints.join(", "))
                }
            })
            .collect(),
        Tab::Resources => lists
            .resources
            .iter()
            .map(|r| r.uri.clone())
            .chain(
                lists
                    .templates
                    .iter()
                    .map(|t| format!("{} (template)", t.uri_template)),
            )
            .collect(),
        Tab::Prompts => lists.prompts.iter().map(|p| p.name.clone()).collect(),
        Tab::Notifications | Tab::Stderr => Vec::new(),
    }
}

fn render_items(frame: &mut Frame, rect: Rect, pane: &McpPane, live: &Live, ui: &UiOptions) {
    if let Some(reason) = disabled(live, pane.tab) {
        frame.render_widget(Paragraph::new(muted(&reason, ui)), rect);
        return;
    }
    let [list, detail] =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(rect);
    let lines: Vec<Line> = item_lines(pane)
        .into_iter()
        .enumerate()
        .map(|(i, text)| item_line(text, i == pane.item, pane.focus == Focus::List, ui))
        .collect();
    frame.render_widget(Paragraph::new(lines), list);
    let mut lines = Vec::new();
    if let Some(form) = &pane.form {
        lines.extend(form_lines(form, pane.focus == Focus::Form, ui));
    }
    if let Some(result) = &pane.result {
        let token = if result.label == "ok" {
            Token::Ok
        } else {
            Token::Error
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", result.title), ui.theme.fg(Token::Text)),
            Span::styled(format!(" {} ", result.label), ui.theme.badge(token)),
        ]));
        lines.extend(result.lines.iter().map(|l| Line::from(l.clone())));
        if result.overridable {
            lines.push(muted("Ctrl+O sends the arguments anyway", ui));
        }
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), detail);
}

fn item_line(text: String, selected: bool, focused: bool, ui: &UiOptions) -> Line<'static> {
    let pointer = if selected { ui.glyphs.pointer } else { " " };
    let style = if selected && focused {
        ui.theme.selection()
    } else {
        ui.theme.fg(Token::Text)
    };
    Line::from(Span::styled(format!("{pointer} {text}"), style))
}

/// The form's rows: `> mode*  one of  fast | [safe]`, with descriptions.
pub fn form_lines(form: &Form, focused: bool, ui: &UiOptions) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(raw) = &form.raw {
        lines.push(Line::from(format!("JSON: {raw}")));
    } else {
        for (i, row) in form.rows.iter().enumerate() {
            let selected = focused && i == form.selected;
            let pointer = if selected { ui.glyphs.pointer } else { " " };
            let star = if row.required { "*" } else { "" };
            let style = if selected {
                ui.theme.selection()
            } else {
                ui.theme.fg(Token::Text)
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{pointer} {}{star}", row.name), style),
                Span::styled(
                    format!("  {}  ", row.kind_label()),
                    ui.theme.fg(Token::Muted),
                ),
                Span::styled(row.shown(), ui.theme.fg(Token::Accent)),
            ]));
            if let Some(description) = &row.description {
                lines.push(muted(&format!("    {description}"), ui));
            }
        }
    }
    for error in &form.errors {
        lines.push(Line::from(Span::styled(
            error.clone(),
            ui.theme.fg(Token::Error),
        )));
    }
    lines
}

fn hint_text(pane: &McpPane) -> String {
    match pane.focus {
        Focus::Servers => "Enter connect · d disconnect · Tab focus · 1-5 tabs · Esc close".to_owned(),
        Focus::List => {
            "Enter open · s subscribe · r read again · l log level · Ctrl+S save call · Ctrl+C cancel"
                .to_owned()
        }
        Focus::Form => "Enter send · ←/→ pick · Space toggle · Ctrl+J raw JSON · Esc back".to_owned(),
    }
}

/// The trust dialog, else the oldest approval, over the pane.
fn render_dialogs(frame: &mut Frame, area: Rect, pane: &McpPane, ui: &UiOptions) {
    if let Some(trust) = &pane.trust {
        let pending = &trust.pending;
        let verb = if pending.changed {
            "changed since you approved it"
        } else {
            "has not been approved on this machine"
        };
        let mut lines = vec![
            Line::from(format!("The stdio server {} {verb}.", pending.server)),
            Line::default(),
            Line::from("Command line:"),
            Line::from(Span::styled(
                format!("  {}", pending.command_line),
                ui.theme.fg(Token::Accent),
            )),
            Line::from(format!("Working directory: {}", pending.cwd)),
        ];
        if !pending.env.is_empty() {
            lines.push(Line::from("Environment:"));
            lines.extend(
                pending
                    .env
                    .iter()
                    .map(|(key, value)| Line::from(format!("  {key}={value}"))),
            );
        }
        lines.extend([
            Line::default(),
            hint("y run it and remember · n do not run it", ui),
        ]);
        dialog(frame, area, " Run this command? ", lines, ui);
        return;
    }
    let Some(waiting) = pane.approvals.front() else {
        return;
    };
    let mut lines: Vec<Line> = waiting.lines().into_iter().map(Line::from).collect();
    lines.push(Line::default());
    if waiting.is_sampling() {
        lines.push(Line::from("Your response:"));
        lines.push(Line::from(Span::styled(
            format!("> {}_", waiting.response),
            ui.theme.fg(Token::Accent),
        )));
        lines.extend([
            Line::default(),
            hint("Ctrl+S send · Ctrl+R or Esc reject", ui),
        ]);
    } else {
        if let Some(form) = &waiting.form {
            lines.extend(form_lines(form, true, ui));
        }
        lines.extend([
            Line::default(),
            hint("Ctrl+S accept · Ctrl+D decline · Esc cancel", ui),
        ]);
    }
    dialog(frame, area, &waiting.title(), lines, ui);
}
