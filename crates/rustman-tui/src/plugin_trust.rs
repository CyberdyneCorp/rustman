//! The workspace plugin trust prompt (plugins spec, "Workspace plugin
//! trust"; add-plugin-system task 4.1). When a workspace opens whose plugin
//! set this machine has not trusted (new, or with other plugins, hashes or
//! grants), a dialog lists each plugin with its version, source and grants
//! and limits in plain language. `y` records the trust in `.rustman/trust.yaml`; `n`
//! or `Esc` leaves every plugin unloaded. The check reads files only: no
//! plugin code runs before, or because of, the answer.

use std::sync::Arc;

use ratatui::Frame;
use ratatui::text::{Line, Span};
use rustman_core::plugins::{PluginGate, PluginTrustPrompt};

use crate::app::{Effect, Overlay, State};
use crate::config::UiOptions;
use crate::keymap::chord::{Chord, Code};
use crate::theme::Token;
use crate::toast::Severity;
use crate::view::{dialog, hint};
use crate::workbench;

/// The front-end's plugin trust store; `None` in a build without plugins.
#[derive(Clone, Default)]
pub struct Gate(pub Option<Arc<dyn PluginGate>>);

impl std::fmt::Debug for Gate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "Gate(..)"
        } else {
            "Gate(None)"
        })
    }
}

/// Ask for trust when the open workspace's plugin set needs it.
pub fn check(state: &mut State) {
    let Some(gate) = state.plugin_gate.0.clone() else {
        return;
    };
    let Some(root) = workbench::workspace(state).map(|ws| ws.root.clone()) else {
        return;
    };
    state.plugin_kinds.clear();
    match gate.check(&root) {
        Ok(Some(prompt)) => state.overlay = Some(Overlay::PluginTrust(Box::new(prompt))),
        Ok(None) => load_kinds(state, gate.as_ref(), &root),
        Err(message) => state.notify(Severity::Error, message),
    }
}

/// Learn the request kinds (and panes) of a trusted plugin set: this runs
/// plugin code, so only once the set is trusted.
fn load_kinds(state: &mut State, gate: &dyn PluginGate, root: &std::path::Path) {
    match gate.kinds(root) {
        Ok(kinds) => state.plugin_kinds = kinds,
        Err(message) => state.notify(Severity::Error, message),
    }
}

/// `y` trusts the set shown, `n` or `Esc` leaves the plugins unloaded.
pub fn key(state: &mut State, chord: Chord) -> Vec<Effect> {
    match chord.code {
        Code::Char('y' | 'Y') => approve(state),
        Code::Char('n' | 'N') | Code::Esc => {
            state.overlay = None;
            state.notify(
                Severity::Warn,
                "plugins not loaded: workspace not trusted".to_owned(),
            );
        }
        _ => {}
    }
    Vec::new()
}

fn approve(state: &mut State) {
    let Some(Overlay::PluginTrust(prompt)) = state.overlay.take() else {
        return;
    };
    let (Some(gate), Some(root)) = (
        state.plugin_gate.0.clone(),
        workbench::workspace(state).map(|ws| ws.root.clone()),
    ) else {
        return;
    };
    match gate.approve(&root, &prompt) {
        Ok(()) => {
            state.notify(Severity::Info, "workspace plugins trusted".to_owned());
            load_kinds(state, gate.as_ref(), &root);
        }
        Err(message) => state.notify(Severity::Error, message),
    }
}

/// The dialog listing each plugin and its grants.
pub fn render(frame: &mut Frame, prompt: &PluginTrustPrompt, ui: &UiOptions) {
    let intro = if prompt.changed {
        "The workspace plugins changed since you trusted them."
    } else {
        "This workspace declares plugins that are not trusted on this machine."
    };
    let mut lines = vec![Line::from(intro), Line::default()];
    for plugin in &prompt.plugins {
        lines.push(Line::from(Span::styled(
            format!("{} {}", plugin.name, plugin.version),
            ui.theme.fg(Token::Accent),
        )));
        lines.push(Line::from(format!("  source: {}", plugin.source)));
        lines.push(Line::from(format!("  limits: {}", plugin.limits)));
        if plugin.grants.is_empty() {
            lines.push(Line::from("  no grants: it runs fully sandboxed"));
        }
        for grant in &plugin.grants {
            lines.push(Line::from(format!("  may {grant}")));
        }
    }
    lines.extend([
        Line::default(),
        Line::from("Nothing runs until you answer."),
        hint("y trust and load them · n do not load them", ui),
    ]);
    dialog(frame, frame.area(), " Trust workspace plugins? ", lines, ui);
}
