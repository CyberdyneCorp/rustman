//! `{{variable}}` highlighting and the resolution tooltip (spec: Variable
//! highlighting in editors).
//!
//! Variables are looked up through the engine's resolver with the request's
//! own precedence chain. Secret values are never read for this: the secret
//! store is not consulted on the render loop, and a secret is shown masked
//! (`••••••••`, or `********` with ASCII glyphs) unless the user revealed
//! secrets for this session (see [`crate::reveal`]).

use std::path::Path;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use rustman_core::Workspace;
use rustman_core::model::Variables;
use rustman_core::vars::{
    EnvironmentSource, Layer, Lookup, Resolver, SecretLookup, VariablesSource,
};

use crate::config::UiOptions;
use crate::glyphs::Glyphs;
use crate::reveal::Revealed;
use crate::theme::Token;

/// A `{{...}}` placeholder in a line: character columns `start..end`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarSpan {
    pub start: usize,
    pub end: usize,
    pub name: String,
}

/// The placeholders of `line`, in order. An unclosed `{{` is not one.
pub fn spans(line: &str) -> Vec<VarSpan> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < chars.len() {
        if chars[i] != '{' || chars[i + 1] != '{' {
            i += 1;
            continue;
        }
        let Some(close) = (i + 2..chars.len().saturating_sub(1))
            .find(|&j| chars[j] == '}' && chars[j + 1] == '}')
        else {
            break;
        };
        let name: String = chars[i + 2..close].iter().collect();
        out.push(VarSpan {
            start: i,
            end: close + 2,
            name: name.trim().to_owned(),
        });
        i = close + 2;
    }
    out
}

/// The name of the placeholder covering character column `col`.
pub fn at(line: &str, col: usize) -> Option<String> {
    spans(line)
        .into_iter()
        .find(|s| (s.start..s.end).contains(&col))
        .map(|s| s.name)
}

/// What a variable resolves to, for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Info {
    Value {
        value: String,
        layer: Layer,
    },
    /// A secret of the active environment; its value is never read here.
    Secret {
        layer: Layer,
    },
    /// `{{$uuid}}` and friends: generated when the request is sent.
    Dynamic,
    Unresolved,
}

impl Info {
    pub fn resolved(&self) -> bool {
        !matches!(self, Self::Unresolved)
    }
}

/// A secret lookup that never touches the store: every secret reads as set.
struct NoRead;

impl SecretLookup for NoRead {
    fn secret(&self, _env: &str, _name: &str) -> Result<Option<String>, String> {
        Ok(Some(String::new()))
    }
}

static NO_READ: NoRead = NoRead;

/// The variables visible to one request.
pub struct VarScope<'a> {
    resolver: Option<Resolver<'a>>,
    environment: Option<&'a str>,
    /// Secret values the user revealed (`Alt+R`), when they are visible.
    revealed: Option<&'a Revealed>,
}

impl<'a> VarScope<'a> {
    /// No variables at all (no workspace).
    pub fn empty() -> Self {
        Self {
            resolver: None,
            environment: None,
            revealed: None,
        }
    }

    /// Show the secrets in `revealed` unmasked (`None`: keep them masked).
    #[must_use]
    pub fn with_revealed(mut self, revealed: Option<&'a Revealed>) -> Self {
        self.revealed = revealed;
        self
    }

    /// The chain of the request at `path` (workspace-relative) in `ws`, with
    /// `environment` active. A request not in the workspace (a new tab) sees
    /// its own `variables`, the environment and `rustman.toml`.
    pub fn new(
        ws: &'a Workspace,
        environment: Option<&'a str>,
        path: Option<&Path>,
        own: Option<&'a Variables>,
    ) -> Self {
        let env = environment.and_then(|name| ws.environments.iter().find(|e| e.name == name));
        let found = path.and_then(|p| ws.requests().into_iter().find(|r| r.entry.path == p));
        let resolver = match found {
            Some(request) => Resolver::for_request(&ws.settings, &request, env, &NO_READ),
            None => {
                let mut resolver = Resolver::new();
                if let Some(vars) = own {
                    resolver.push(VariablesSource::new(Layer::Request, vars));
                }
                if let Some(env) = env {
                    resolver.push(EnvironmentSource::new(env, &NO_READ));
                }
                resolver.with(VariablesSource::new(
                    Layer::Settings,
                    &ws.settings.variables,
                ))
            }
        };
        Self {
            resolver: Some(resolver),
            environment,
            revealed: None,
        }
    }

    /// `text` with its variables replaced; `None` when one is missing or
    /// a secret (secrets are never read here).
    pub fn resolve(&self, text: &str) -> Option<String> {
        let resolved = self.resolver.as_ref()?.resolve(text).ok()?;
        resolved.secret_spans.is_empty().then_some(resolved.text)
    }

    pub fn info(&self, name: &str) -> Info {
        if name.starts_with('$') {
            return Info::Dynamic;
        }
        let found = self
            .resolver
            .as_ref()
            .and_then(|r| r.lookup_with_layer(name));
        match found {
            Some((layer, Lookup::Value(value))) => Info::Value { value, layer },
            Some((layer, Lookup::Secret(_) | Lookup::SecretNotSet { .. })) => {
                Info::Secret { layer }
            }
            None => Info::Unresolved,
        }
    }

    /// The tooltip text for `name`, in the glyph set `g` (secrets masked).
    pub fn tooltip(&self, name: &str, g: &Glyphs) -> String {
        let (dot, mask) = (g.dot, g.mask);
        match self.info(name) {
            Info::Value { value, layer } => {
                format!("{{{{{name}}}}} = {value} {dot} {}", layer_name(layer))
            }
            Info::Secret { layer } => {
                let value = self
                    .revealed
                    .and_then(|r| r.value(self.environment, name))
                    .unwrap_or(mask);
                format!(
                    "{{{{{name}}}}} = {value} {dot} secret ({})",
                    layer_name(layer)
                )
            }
            Info::Dynamic => format!("{{{{{name}}}}} {dot} dynamic, generated when sent"),
            Info::Unresolved => format!("{{{{{name}}}}} is not defined in any layer"),
        }
    }
}

/// How a layer is named in the tooltip.
pub fn layer_name(layer: Layer) -> &'static str {
    match layer {
        Layer::Override => "override",
        Layer::Capture => "capture",
        Layer::DataRow => "data row",
        Layer::Request => "request",
        Layer::Collection => "collection",
        Layer::PersistedCapture => "persisted capture",
        Layer::Environment => "environment",
        Layer::Settings => "rustman.toml",
    }
}

/// `line` with resolved variables in the accent colour, unresolved ones in
/// the error colour, and the cursor (a character column) in reverse video.
pub fn highlight(
    line: &str,
    base: Style,
    cursor: Option<usize>,
    scope: &VarScope,
    ui: &UiOptions,
) -> Line<'static> {
    let chars: Vec<char> = line.chars().collect();
    let mut styles = vec![base; chars.len() + 1];
    for span in spans(line) {
        let token = if scope.info(&span.name).resolved() {
            Token::Accent
        } else {
            Token::Error
        };
        let mut style = ui.theme.fg(token).add_modifier(Modifier::BOLD);
        if token == Token::Error {
            // Still visible without colour (NO_COLOR).
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        styles[span.start..span.end].fill(style);
    }
    if let Some(col) = cursor {
        let col = col.min(chars.len());
        styles[col] = styles[col].add_modifier(Modifier::REVERSED);
    }
    let mut text: Vec<char> = chars;
    if cursor.is_some_and(|c| c >= text.len()) {
        text.push(' ');
    }
    group(&text, &styles)
}

/// Consecutive characters with the same style become one span.
fn group(chars: &[char], styles: &[Style]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut current = styles.first().copied().unwrap_or_default();
    for (c, style) in chars.iter().zip(styles) {
        if *style != current && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), current));
        }
        current = *style;
        run.push(*c);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, current));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_found_by_character_column() {
        let line = "é {{ base_url }}/users/{{id}}?x={{";
        let found = spans(line);
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].start, found[0].end), (2, 16));
        assert_eq!(found[0].name, "base_url");
        assert_eq!(found[1].name, "id");
        assert_eq!(at(line, 3).as_deref(), Some("base_url"));
        assert_eq!(at(line, 16), None);
        assert_eq!(at(line, 25).as_deref(), Some("id"));
        assert!(spans("{{}").is_empty());
    }

    #[test]
    fn an_empty_scope_resolves_only_dynamic_variables() {
        let scope = VarScope::empty();
        assert_eq!(scope.info("$uuid"), Info::Dynamic);
        assert_eq!(scope.info("x"), Info::Unresolved);
        assert_eq!(
            scope.tooltip("x", &crate::glyphs::ASCII),
            "{{x}} is not defined in any layer"
        );
    }

    #[test]
    fn the_cursor_past_the_end_is_drawn_on_a_blank() {
        let ui = UiOptions::default();
        let line = highlight("ab", Style::default(), Some(2), &VarScope::empty(), &ui);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "ab ");
        let last = line.spans.last().unwrap();
        assert!(last.style.add_modifier.contains(Modifier::REVERSED));
    }
}
