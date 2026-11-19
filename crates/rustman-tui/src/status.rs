//! The status bar: workspace, environment, TLS verification, in-flight count
//! on the left; vim mode, `OTLP →<host>` while spans are exported,
//! `SECRETS VISIBLE` and the quit hint on the right.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::config::UiOptions;
use crate::theme::Token;

/// The vim mode shown in the status bar while vim keys are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimMode {
    Normal,
    Insert,
    Visual,
}

impl VimMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Insert => "INSERT",
            Self::Visual => "VISUAL",
        }
    }
}

/// What the status bar reports besides the workspace name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusInfo {
    pub environment: Option<String>,
    /// TLS certificate verification is off for the active environment.
    pub tls_insecure: bool,
    pub in_flight: usize,
    pub vim: Option<VimMode>,
    pub secrets_visible: bool,
    /// The webhook listener, while it runs.
    pub webhook: Option<WebhookBadge>,
    /// MCP sampling or elicitation requests waiting for the user.
    pub mcp: Option<McpBadge>,
    /// The collector client spans are exported to (`[trace.export]`), as
    /// `host:port`.
    pub otlp: Option<String>,
}

/// The MCP approvals waiting for the user, shown on every screen until
/// they are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpBadge {
    pub pending: usize,
}

/// The running webhook listener in the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookBadge {
    pub addr: String,
    /// Reachable from the network: shown as a warning on every screen.
    pub remote: bool,
}

/// Draw the status bar for `workspace` into the one-row `area`; `quit` is
/// the key that quits.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    workspace: &str,
    info: &StatusInfo,
    quit: &str,
    ui: &UiOptions,
) {
    let right = right_spans(info, quit, ui);
    let right_width = u16::try_from(Line::from(right.clone()).width()).unwrap_or(u16::MAX);
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width)]).areas(area);
    frame.render_widget(
        Paragraph::new(Line::from(left_spans(workspace, info, ui))),
        left_area,
    );
    frame.render_widget(Paragraph::new(Line::from(right)), right_area);
}

fn left_spans(workspace: &str, info: &StatusInfo, ui: &UiOptions) -> Vec<Span<'static>> {
    let theme = &ui.theme;
    let separator = Span::styled(ui.glyphs.separator, theme.fg(Token::Border));
    let environment = match &info.environment {
        Some(name) => Span::styled(format!(" env: {name} "), theme.fg(Token::Text)),
        None => Span::styled(" no environment ", theme.fg(Token::Muted)),
    };
    let tls = if info.tls_insecure {
        Span::styled(" TLS: INSECURE ", theme.badge(Token::Error))
    } else {
        Span::styled(" TLS verified ", theme.fg(Token::Muted))
    };
    let in_flight = if info.in_flight == 0 {
        Span::styled(" 0 in flight ", theme.fg(Token::Muted))
    } else {
        let text = format!(" {} in flight ", info.in_flight);
        Span::styled(text, theme.fg(Token::Accent).add_modifier(Modifier::BOLD))
    };
    vec![
        Span::styled(
            format!(" {workspace} "),
            theme.fg(Token::Text).add_modifier(Modifier::BOLD),
        ),
        separator.clone(),
        environment,
        separator.clone(),
        tls,
        separator,
        in_flight,
    ]
}

fn right_spans(info: &StatusInfo, quit: &str, ui: &UiOptions) -> Vec<Span<'static>> {
    let theme = &ui.theme;
    let mut spans = Vec::new();
    if let Some(mode) = info.vim {
        spans.push(Span::styled(
            format!(" {} ", mode.label()),
            theme.badge(Token::Accent),
        ));
        spans.push(Span::raw(" "));
    }
    if let Some(webhook) = &info.webhook {
        spans.push(if webhook.remote {
            let text = format!(" WEBHOOK ON NETWORK {} ", webhook.addr);
            Span::styled(text, theme.badge(Token::Error))
        } else {
            let text = format!("webhook {} ", webhook.addr);
            Span::styled(text, theme.fg(Token::Accent))
        });
        spans.push(Span::raw(" "));
    }
    if let Some(mcp) = info.mcp {
        let text = format!(" MCP APPROVAL ({}) ", mcp.pending);
        spans.push(Span::styled(text, theme.badge(Token::Warn)));
        spans.push(Span::raw(" "));
    }
    if let Some(host) = &info.otlp {
        spans.push(Span::styled(
            format!("OTLP →{host} "),
            theme.fg(Token::Accent),
        ));
        spans.push(Span::raw(" "));
    }
    if info.secrets_visible {
        spans.push(Span::styled(" SECRETS VISIBLE ", theme.badge(Token::Warn)));
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled(
        format!("{quit} quit "),
        theme.fg(Token::Muted),
    ));
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{buffer_text, style_of};
    use crate::theme::{ColorMode, Theme, ThemeName};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;

    fn draw(info: &StatusInfo, ui: &UiOptions) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(100, 1)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), "demo", info, "Ctrl+Q", ui))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn text(info: &StatusInfo) -> String {
        buffer_text(&draw(info, &UiOptions::default()))
    }

    #[test]
    fn status_default() {
        let info = StatusInfo {
            environment: Some("dev".into()),
            ..StatusInfo::default()
        };
        insta::assert_snapshot!(text(&info));
    }

    #[test]
    fn status_otlp_export() {
        let info = StatusInfo {
            environment: Some("dev".into()),
            otlp: Some("localhost:4317".into()),
            ..StatusInfo::default()
        };
        insta::assert_snapshot!(text(&info));
        let ui = UiOptions::default();
        let style = style_of(&draw(&info, &ui), "OTLP →localhost:4317").unwrap();
        assert_eq!(style.fg, ui.theme.color(Token::Accent));
    }

    #[test]
    fn status_no_environment() {
        insta::assert_snapshot!(text(&StatusInfo::default()));
    }

    #[test]
    fn status_tls_insecure() {
        let info = StatusInfo {
            environment: Some("dev".into()),
            tls_insecure: true,
            ..StatusInfo::default()
        };
        insta::assert_snapshot!(text(&info));
        let ui = UiOptions::default();
        let style = style_of(&draw(&info, &ui), "TLS: INSECURE").unwrap();
        assert_eq!(style.bg, ui.theme.color(Token::Error));
    }

    #[test]
    fn status_in_flight() {
        let info = StatusInfo {
            in_flight: 2,
            ..StatusInfo::default()
        };
        insta::assert_snapshot!(text(&info));
    }

    #[test]
    fn status_vim_modes() {
        for mode in [VimMode::Normal, VimMode::Insert, VimMode::Visual] {
            let info = StatusInfo {
                vim: Some(mode),
                ..StatusInfo::default()
            };
            let line = text(&info);
            assert!(
                line.contains(&format!(" {}  Ctrl+Q quit", mode.label())),
                "{line}"
            );
        }
        let info = StatusInfo {
            vim: Some(VimMode::Insert),
            ..StatusInfo::default()
        };
        insta::assert_snapshot!(text(&info));
    }

    #[test]
    fn status_secrets_visible() {
        let info = StatusInfo {
            secrets_visible: true,
            ..StatusInfo::default()
        };
        insta::assert_snapshot!(text(&info));
        // Scenario "Badge while revealed": the warn colour.
        let ui = UiOptions::default();
        let style = style_of(&draw(&info, &ui), "SECRETS VISIBLE").unwrap();
        assert_eq!(style.bg, ui.theme.color(Token::Warn));
    }

    #[test]
    fn status_every_badge() {
        let info = StatusInfo {
            environment: Some("staging".into()),
            tls_insecure: true,
            in_flight: 1,
            vim: Some(VimMode::Normal),
            secrets_visible: true,
            webhook: None,
            mcp: None,
            otlp: None,
        };
        insta::assert_snapshot!(text(&info));
    }

    #[test]
    fn a_network_reachable_webhook_listener_is_a_red_badge() {
        // Spec: TUI webhook pane, the persistent non-loopback indicator.
        let ui = UiOptions::default();
        let remote = StatusInfo {
            webhook: Some(WebhookBadge {
                addr: "0.0.0.0:9090".into(),
                remote: true,
            }),
            ..StatusInfo::default()
        };
        let buffer = draw(&remote, &ui);
        assert!(buffer_text(&buffer).contains(" WEBHOOK ON NETWORK 0.0.0.0:9090 "));
        let style = style_of(&buffer, "WEBHOOK ON NETWORK").unwrap();
        assert_eq!(style.bg, ui.theme.color(Token::Error));
        let local = StatusInfo {
            webhook: Some(WebhookBadge {
                addr: "127.0.0.1:9090".into(),
                remote: false,
            }),
            ..StatusInfo::default()
        };
        let text = text(&local);
        assert!(text.contains("webhook 127.0.0.1:9090"), "{text}");
        assert!(!text.contains("NETWORK"), "{text}");
    }

    #[test]
    fn badges_without_colour_use_reverse_video() {
        let ui = UiOptions {
            theme: Theme::new(ThemeName::Dark, ColorMode::NoColor),
            ..UiOptions::default()
        };
        let info = StatusInfo {
            tls_insecure: true,
            secrets_visible: true,
            ..StatusInfo::default()
        };
        let buffer = draw(&info, &ui);
        for badge in ["TLS: INSECURE", "SECRETS VISIBLE"] {
            let style = style_of(&buffer, badge).unwrap();
            let plain = |c: Option<Color>| c.is_none_or(|c| c == Color::Reset);
            assert!(plain(style.fg) && plain(style.bg), "{style:?}");
            assert!(style.add_modifier.contains(Modifier::REVERSED));
        }
    }
}
