//! Drawing the `evm-call` pane's tabs: the function picker, the typed
//! argument fields, the block selector, and the result (outputs in the
//! chosen unit form, revert, gas estimate), the call trace and the event
//! timeline.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::evm_pane::{BLOCK_CHOICES, EvmEditor, EvmTab};
use crate::focus::Focus;
use crate::pane::RenderCtx;
use crate::theme::Token;

pub fn content(frame: &mut Frame, rect: Rect, editor: &EvmEditor, ctx: &RenderCtx) {
    if rect.height == 0 {
        return;
    }
    let focused = ctx.focus == Focus::EditorBody;
    let lines = match editor.tab {
        EvmTab::Function => picker(editor, focused, ctx),
        EvmTab::Args => args(editor, focused, ctx),
        EvmTab::Block => block(editor, ctx),
        EvmTab::Result => result(editor, ctx),
        EvmTab::Trace => listing(
            trace_lines(editor),
            "No trace yet: send with --trace from the CLI, or the result has none.",
            ctx,
        ),
        EvmTab::Events => listing(editor.events.clone(), "No events yet.", ctx),
    };
    frame.render_widget(Paragraph::new(lines), rect);
}

fn muted(text: &str, ctx: &RenderCtx) -> Line<'static> {
    Line::styled(format!(" {text}"), ctx.ui.theme.fg(Token::Muted))
}

fn picker(editor: &EvmEditor, focused: bool, ctx: &RenderCtx) -> Vec<Line<'static>> {
    let theme = &ctx.ui.theme;
    let filter = if editor.show_all {
        "all functions (non-view ones run as read-only eth_call simulations)"
    } else {
        "view and pure functions"
    };
    let mut lines = vec![muted(&format!("{filter} · a: toggle · Enter: choose"), ctx)];
    let functions = editor.functions();
    if functions.is_empty() {
        lines.push(muted(
            editor
                .abi_note
                .as_deref()
                .unwrap_or("The ABI has no such function."),
            ctx,
        ));
    }
    for (i, function) in functions.iter().enumerate() {
        let chosen = editor.function.as_deref() == Some(function.name.as_str())
            || editor.function.as_deref() == Some(function.signature().as_str());
        let marker = if chosen { "●" } else { " " };
        let text = format!(" {marker} {}", function.signature());
        let style = if focused && i == editor.cursor {
            theme.fg(Token::Accent).add_modifier(Modifier::REVERSED)
        } else {
            theme.fg(Token::Text)
        };
        let mutability = format!("  {}", function.state_mutability.as_json_str());
        lines.push(Line::from(vec![
            Span::styled(text, style),
            Span::styled(mutability, theme.fg(Token::Muted)),
        ]));
    }
    lines
}

fn args(editor: &EvmEditor, focused: bool, ctx: &RenderCtx) -> Vec<Line<'static>> {
    let theme = &ctx.ui.theme;
    let title = match &editor.function {
        Some(function) => format!("Arguments of {function}"),
        None => "Choose a function first".to_owned(),
    };
    let mut lines = vec![muted(&title, ctx)];
    if editor.args.is_empty() {
        lines.push(muted("No arguments.", ctx));
    }
    for (i, field) in editor.args.iter().enumerate() {
        let label = format!(" {} ({}): ", field.name, field.ty);
        let cursor = if focused && i == editor.arg {
            "▏"
        } else {
            ""
        };
        let style = if focused && i == editor.arg {
            theme.fg(Token::Accent)
        } else {
            theme.fg(Token::Text)
        };
        lines.push(Line::from(vec![
            Span::styled(label, theme.fg(Token::Muted)),
            Span::styled(format!("{}{cursor}", field.value), style),
        ]));
    }
    lines
}

fn block(editor: &EvmEditor, ctx: &RenderCtx) -> Vec<Line<'static>> {
    let theme = &ctx.ui.theme;
    let mut spans = vec![Span::styled(" Block: ", theme.fg(Token::Muted))];
    for choice in BLOCK_CHOICES {
        let style = if choice == editor.block {
            theme.badge(Token::Accent)
        } else {
            theme.fg(Token::Text)
        };
        spans.push(Span::styled(format!(" {choice} "), style));
    }
    let mut lines = vec![
        Line::from(spans),
        muted(
            "←/→: change · a number or hash can be typed in the file",
            ctx,
        ),
    ];
    if !BLOCK_CHOICES.contains(&editor.block.as_str()) {
        lines.push(muted(&format!("pinned to {}", editor.block), ctx));
    }
    lines
}

fn result(editor: &EvmEditor, ctx: &RenderCtx) -> Vec<Line<'static>> {
    let theme = &ctx.ui.theme;
    let Some(view) = &editor.result else {
        return vec![muted("Not sent yet.", ctx)];
    };
    let mut lines = Vec::new();
    if let Some(status) = &view.status {
        lines.push(muted(status, ctx));
    }
    if let Some(error) = &view.error {
        let text = format!(" {} {error}", ctx.ui.glyphs.fail);
        lines.push(Line::styled(text, theme.fg(Token::Error)));
    }
    if let Some(revert) = &view.revert {
        lines.push(Line::styled(format!(" {revert}"), theme.fg(Token::Error)));
    }
    for row in &view.outputs {
        lines.push(Line::from(vec![
            Span::styled(format!(" {} = ", row.name), theme.fg(Token::Muted)),
            Span::styled(editor.shown(row), theme.fg(Token::Text)),
        ]));
    }
    if let Some(gas) = &view.gas_estimate {
        lines.push(muted(&format!("gas estimate: {gas}"), ctx));
    }
    if !view.outputs.is_empty() {
        lines.push(muted("u: toggle units", ctx));
    }
    lines
}

fn trace_lines(editor: &EvmEditor) -> Vec<String> {
    editor
        .result
        .as_ref()
        .map(|r| r.trace.clone())
        .unwrap_or_default()
}

fn listing(items: Vec<String>, empty: &str, ctx: &RenderCtx) -> Vec<Line<'static>> {
    if items.is_empty() {
        return vec![muted(empty, ctx)];
    }
    let theme = &ctx.ui.theme;
    items
        .into_iter()
        .map(|item| {
            let token = if item.starts_with("REORGED") {
                Token::Warn
            } else {
                Token::Text
            };
            Line::styled(format!(" {item}"), theme.fg(token))
        })
        .collect()
}
