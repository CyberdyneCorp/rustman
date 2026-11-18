//! The fuzzy command palette (spec: Command palette): every action with its
//! current binding, every request file and every environment, ranked with
//! nucleo's matcher. With an empty query, recently used entries come first.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Padding, Paragraph};

use crate::config::UiOptions;
use crate::keymap::{ActionId, Keymap};
use crate::theme::Token;
use crate::view::{block, hint};

/// How many recently used entries are remembered (this session only).
pub const RECENT_LIMIT: usize = 20;

/// What a palette entry does when chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Action(ActionId),
    /// A request file, by its path in the workspace.
    Request(String),
    Environment(String),
}

/// What the palette lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Everything,
    /// Only environments: the switcher behind `Switch environment`.
    Environments,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub target: Target,
    pub label: String,
    /// Shown on the right: the binding, `request` or `environment`.
    pub detail: String,
}

/// What the palette can list, gathered from the state when it opens.
#[derive(Debug, Clone, Copy)]
pub struct Sources<'a> {
    pub keymap: &'a Keymap,
    pub requests: &'a [String],
    pub environments: &'a [String],
    pub active_environment: Option<&'a str>,
    /// Joins `environment` and `active`; from the glyph set.
    pub dot: &'a str,
}

/// The candidate entries for `scope`, in source order.
pub fn entries(scope: Scope, sources: Sources<'_>) -> Vec<Entry> {
    let mut out = Vec::new();
    if scope == Scope::Everything {
        out.extend(ActionId::all().map(|action| Entry {
            target: Target::Action(action),
            label: action.title().to_owned(),
            detail: sources.keymap.label(action),
        }));
        out.extend(sources.requests.iter().map(|path| Entry {
            target: Target::Request(path.clone()),
            label: path.clone(),
            detail: "request".to_owned(),
        }));
    }
    out.extend(sources.environments.iter().map(|name| {
        let active = sources.active_environment == Some(name.as_str());
        let detail = match (scope, active) {
            (Scope::Environments, true) => "active".to_owned(),
            (Scope::Environments, false) => String::new(),
            (Scope::Everything, true) => format!("environment {} active", sources.dot),
            (Scope::Everything, false) => "environment".to_owned(),
        };
        Entry {
            target: Target::Environment(name.clone()),
            label: name.clone(),
            detail,
        }
    }));
    out
}

/// The open palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub scope: Scope,
    pub query: String,
    /// Index into `ranked`.
    pub selected: usize,
    pub entries: Vec<Entry>,
    /// Indices into `entries`, best first; only the matching ones.
    pub ranked: Vec<usize>,
}

impl Palette {
    pub fn new(scope: Scope, entries: Vec<Entry>, recent: &[Target]) -> Self {
        let mut palette = Self {
            scope,
            query: String::new(),
            selected: 0,
            entries,
            ranked: Vec::new(),
        };
        palette.rerank(recent);
        palette
    }

    pub fn title(&self) -> &'static str {
        match self.scope {
            Scope::Everything => " Command palette ",
            Scope::Environments => " Switch environment ",
        }
    }

    pub fn set_query(&mut self, query: &str, recent: &[Target]) {
        query.clone_into(&mut self.query);
        self.rerank(recent);
    }

    fn rerank(&mut self, recent: &[Target]) {
        self.ranked = rank(&self.entries, &self.query, recent);
        self.selected = 0;
    }

    /// Move the selection by `delta`, wrapping around.
    pub fn move_by(&mut self, delta: isize) {
        let len = self.ranked.len();
        if len == 0 {
            return;
        }
        let len = isize::try_from(len).unwrap_or(isize::MAX);
        let current = isize::try_from(self.selected).unwrap_or(0);
        self.selected = usize::try_from((current + delta).rem_euclid(len)).unwrap_or(0);
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.ranked.get(self.selected).map(|&i| &self.entries[i])
    }

    /// The ranked entries.
    pub fn visible(&self) -> impl Iterator<Item = &Entry> {
        self.ranked.iter().map(|&i| &self.entries[i])
    }
}

/// Indices of `entries` matching `query`, best first. An empty query keeps
/// every entry, recently used ones first; otherwise the fuzzy score decides
/// and recency breaks ties.
pub fn rank(entries: &[Entry], query: &str, recent: &[Target]) -> Vec<usize> {
    let recency = |entry: &Entry| {
        recent
            .iter()
            .position(|t| *t == entry.target)
            .unwrap_or(usize::MAX)
    };
    if query.trim().is_empty() {
        let mut all: Vec<usize> = (0..entries.len()).collect();
        all.sort_by_key(|&i| (recency(&entries[i]), i));
        return all;
    }
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, usize, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, entry)| {
            let haystack = Utf32Str::new(&entry.label, &mut buf);
            let score = pattern.score(haystack, &mut matcher)?;
            Some((score, recency(entry), i))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    scored.into_iter().map(|(_, _, i)| i).collect()
}

/// Put `target` first in the recently used list.
pub fn touch(recent: &mut Vec<Target>, target: Target) {
    recent.retain(|t| *t != target);
    recent.insert(0, target);
    recent.truncate(RECENT_LIMIT);
}

/// Draw the palette near the top of `area`.
pub fn render(frame: &mut Frame, area: Rect, palette: &Palette, ui: &UiOptions) {
    let width = area.width.saturating_sub(4).min(72);
    let height = area.height.saturating_sub(2).min(18);
    let [column] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);
    let rect = Rect {
        y: area.y + area.height.saturating_sub(height).min(2),
        height,
        ..column
    };
    frame.render_widget(Clear, rect);
    let block = block(palette.title(), ui)
        .border_style(ui.theme.fg(Token::Focus))
        .padding(Padding::horizontal(1));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let rows = usize::from(inner.height.saturating_sub(3));
    let mut lines = vec![query_line(palette, ui), Line::default()];
    lines.extend(entry_lines(palette, rows, inner.width, ui));
    if palette.ranked.is_empty() {
        lines.push(hint("No matches.", ui));
    }
    frame.render_widget(Paragraph::new(lines), inner);
    let footer = Rect {
        y: inner.bottom().saturating_sub(1),
        height: 1.min(inner.height),
        ..inner
    };
    let g = &ui.glyphs;
    let keys = format!(
        "{} choose {} Enter run {} Esc close",
        g.up_down, g.dot, g.dot
    );
    frame.render_widget(Paragraph::new(hint(&keys, ui)), footer);
}

fn query_line(palette: &Palette, ui: &UiOptions) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{} ", ui.glyphs.pointer),
            ui.theme.fg(Token::Accent),
        ),
        Span::styled(palette.query.clone(), ui.theme.fg(Token::Text)),
        Span::styled(" ", ui.theme.selection()),
    ])
}

/// The visible entries, scrolled so the selection shows, with the detail
/// right-aligned.
fn entry_lines(palette: &Palette, rows: usize, width: u16, ui: &UiOptions) -> Vec<Line<'static>> {
    let skip = (palette.selected + 1).saturating_sub(rows);
    let width = usize::from(width);
    palette
        .visible()
        .enumerate()
        .skip(skip)
        .take(rows)
        .map(|(i, entry)| {
            let selected = i == palette.selected;
            let gap = width
                .saturating_sub(entry.label.chars().count() + entry.detail.chars().count() + 2)
                .max(1);
            let (label_style, detail_style) = if selected {
                let style = ui.theme.selection().add_modifier(Modifier::BOLD);
                (style, style)
            } else {
                (ui.theme.fg(Token::Text), ui.theme.fg(Token::Muted))
            };
            Line::from(vec![
                Span::styled(format!(" {}", entry.label), label_style),
                Span::styled(" ".repeat(gap), label_style),
                Span::styled(format!("{} ", entry.detail), detail_style),
            ])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glyphs::{ASCII, UNICODE};
    use crate::keymap::UserLayer;

    fn sources<'a>(keymap: &'a Keymap, requests: &'a [String], envs: &'a [String]) -> Sources<'a> {
        Sources {
            keymap,
            requests,
            environments: envs,
            active_environment: Some("dev"),
            dot: UNICODE.dot,
        }
    }

    fn labels(palette: &Palette) -> Vec<String> {
        palette.visible().map(|e| e.label.clone()).collect()
    }

    fn fixture() -> (Keymap, Vec<String>, Vec<String>) {
        let requests = vec![
            "users/get-user.rman.yaml".to_owned(),
            "health.rman.yaml".to_owned(),
        ];
        let envs = vec!["dev".to_owned(), "staging".to_owned()];
        (Keymap::default(), requests, envs)
    }

    #[test]
    fn swenv_ranks_switch_environment_first() {
        let (keymap, requests, envs) = fixture();
        let mut palette = Palette::new(
            Scope::Everything,
            entries(Scope::Everything, sources(&keymap, &requests, &envs)),
            &[],
        );
        palette.set_query("swenv", &[]);
        let first = palette.selected().unwrap();
        assert_eq!(first.label, "Switch environment");
        assert_eq!(first.target, Target::Action(ActionId::EnvSwitch));
    }

    #[test]
    fn entries_show_the_current_binding() {
        let (layer, _) = UserLayer::parse("[global]\n\"F5\" = \"request.send\"");
        let keymap = Keymap::new(false, &layer);
        let (_, requests, envs) = fixture();
        let all = entries(Scope::Everything, sources(&keymap, &requests, &envs));
        let send = all.iter().find(|e| e.label == "Send request").unwrap();
        assert_eq!(send.detail, "F5, Ctrl+Enter, Ctrl+R");
        let default = Keymap::default();
        let all = entries(Scope::Everything, sources(&default, &requests, &envs));
        let send = all.iter().find(|e| e.label == "Send request").unwrap();
        assert_eq!(send.detail, "Ctrl+Enter, Ctrl+R");
        assert!(
            all.iter()
                .any(|e| e.target == Target::Request("health.rman.yaml".into()))
        );
        let staging = all.iter().find(|e| e.label == "staging").unwrap();
        assert_eq!(staging.detail, "environment");
        let dev = all.iter().find(|e| e.label == "dev").unwrap();
        assert_eq!(dev.detail, "environment · active");
    }

    #[test]
    fn ascii_mode_joins_the_environment_detail_with_an_ascii_dot() {
        let keymap = Keymap::default();
        let envs = vec!["dev".to_owned()];
        let ascii = Sources {
            dot: ASCII.dot,
            ..sources(&keymap, &[], &envs)
        };
        let all = entries(Scope::Everything, ascii);
        let dev = all.iter().find(|e| e.label == "dev").unwrap();
        assert_eq!(dev.detail, "environment - active");
        assert!(dev.detail.is_ascii());
    }

    #[test]
    fn recent_entries_rank_first_with_an_empty_query() {
        let (keymap, requests, envs) = fixture();
        let all = entries(Scope::Everything, sources(&keymap, &requests, &envs));
        let mut recent = Vec::new();
        touch(&mut recent, Target::Action(ActionId::HelpOpen));
        touch(&mut recent, Target::Environment("staging".into()));
        touch(&mut recent, Target::Action(ActionId::HelpOpen));
        let palette = Palette::new(Scope::Everything, all.clone(), &recent);
        assert_eq!(labels(&palette)[..3], ["Help", "staging", "Send request"]);
        assert_eq!(palette.ranked.len(), all.len());
    }

    #[test]
    fn recency_breaks_score_ties() {
        let entry = |name: &str| Entry {
            target: Target::Environment(name.into()),
            label: name.into(),
            detail: String::new(),
        };
        let all = vec![entry("prod-a"), entry("prod-b")];
        let recent = vec![Target::Environment("prod-b".into())];
        let palette_order = rank(&all, "prod", &recent);
        assert_eq!(palette_order, [1, 0]);
        assert_eq!(rank(&all, "prod", &[]), [0, 1]);
        assert!(rank(&all, "zzz", &[]).is_empty());
    }

    #[test]
    fn the_environment_scope_lists_environments_only() {
        let (keymap, requests, envs) = fixture();
        let palette = Palette::new(
            Scope::Environments,
            entries(Scope::Environments, sources(&keymap, &requests, &envs)),
            &[],
        );
        assert_eq!(labels(&palette), ["dev", "staging"]);
        assert_eq!(palette.entries[0].detail, "active");
        assert_eq!(palette.title(), " Switch environment ");
    }

    #[test]
    fn selection_wraps() {
        let (keymap, requests, envs) = fixture();
        let mut palette = Palette::new(
            Scope::Environments,
            entries(Scope::Environments, sources(&keymap, &requests, &envs)),
            &[],
        );
        palette.move_by(-1);
        assert_eq!(palette.selected().unwrap().label, "staging");
        palette.move_by(1);
        assert_eq!(palette.selected().unwrap().label, "dev");
        let mut empty = Palette::new(Scope::Environments, Vec::new(), &[]);
        empty.move_by(1);
        assert!(empty.selected().is_none());
    }

    #[test]
    fn recency_is_bounded() {
        let mut recent = Vec::new();
        for i in 0..30 {
            touch(&mut recent, Target::Request(format!("r{i}")));
        }
        assert_eq!(recent.len(), RECENT_LIMIT);
        assert_eq!(recent[0], Target::Request("r29".into()));
    }
}
