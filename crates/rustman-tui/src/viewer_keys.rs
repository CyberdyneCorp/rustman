//! The response viewer's actions and completions: tabs, the JSON tree keys,
//! filter and search, copying, saving the body, the paged view and the
//! exchange diff. The pure parts live in [`crate::response`]; this module
//! reads and changes the [`State`] and returns the effects.

use std::path::{Path, PathBuf};

use crate::app::{Action, Effect, Overlay, State};
use crate::dialogs::{Input, Purpose};
use crate::focus::{Focus, Pane};
use crate::keymap::{ActionId, Chord};
use crate::layout::regions;
use crate::response::json_tree::TreeView;
use crate::response::picker::{Picked, Picker};
use crate::response::view::{content_height, line_count};
use crate::response::{BodyKind, BoxKind, Move, Paging, Typed, Viewer, picker, received};
use crate::tabs::{Tab, TabId};
use crate::toast::Severity;
use crate::workbench;

/// The rows and columns of the response content on screen.
fn viewport(state: &State, viewer: &Viewer) -> (usize, usize) {
    let rect = regions(state.area, &state.shell).pane(Pane::Response);
    rect.map_or((10, 80), |rect| {
        let width = usize::from(rect.width.saturating_sub(4));
        (content_height(rect, viewer), width)
    })
}

/// The tab whose exchange is shown, with one.
fn shown(state: &mut State) -> Option<&mut Tab> {
    state
        .tabs
        .active_mut()
        .filter(|t| t.exchange.is_some() && t.in_flight.is_none())
}

/// A key while the filter or search box is open (the response is focused).
pub fn box_key(state: &mut State, chord: Chord) -> Option<Vec<Effect>> {
    if state.shell.focus != Focus::Response {
        return None;
    }
    let tab = shown(state)?;
    tab.viewer.input.as_ref()?;
    let effect = match tab.viewer.box_key(chord)? {
        Typed::Handled => return Some(Vec::new()),
        Typed::Search(job) => Effect::Search { tab: tab.id, job },
        Typed::Filter(expr) => Effect::Filter {
            tab: tab.id,
            expr,
            source: tab.exchange.as_ref()?.source.clone()?,
        },
    };
    Some(vec![effect])
}

/// Run a response viewer action; `None` when `action` is not one.
pub fn run(state: &mut State, action: ActionId) -> Option<Vec<Effect>> {
    let effects = match action {
        ActionId::JsonCopy => copy(state),
        ActionId::ResponseDiff => diff(state),
        ActionId::ResponseSaveBody => {
            prompt_save(state);
            Vec::new()
        }
        _ => {
            viewer_command(state, action)?;
            Vec::new()
        }
    };
    Some(effects)
}

/// The viewer actions that need no effect; `None` for other actions.
fn viewer_command(state: &mut State, action: ActionId) -> Option<()> {
    let tree_action: fn(&mut TreeView) = match action {
        ActionId::JsonToggle => TreeView::toggle,
        ActionId::JsonCollapse => TreeView::collapse,
        ActionId::JsonExpand => TreeView::expand,
        ActionId::JsonCollapseAll => TreeView::collapse_all,
        ActionId::JsonExpandAll => TreeView::expand_all,
        _ => return viewer_other(state, action),
    };
    if let Some(tab) = shown(state) {
        tab.viewer.with_tree(tree_action);
    }
    Some(())
}

fn viewer_other(state: &mut State, action: ActionId) -> Option<()> {
    let tab = match action {
        ActionId::ResponseNextTab
        | ActionId::ResponsePrevTab
        | ActionId::ResponseRaw
        | ActionId::ResponseDirection
        | ActionId::ResponseFilter
        | ActionId::ResponseSearch
        | ActionId::SearchNext
        | ActionId::SearchPrev => shown(state),
        _ => return None,
    };
    let Some(viewer) = tab.map(|t| &mut t.viewer) else {
        return Some(());
    };
    match action {
        ActionId::ResponseNextTab => viewer.step_tab(true),
        ActionId::ResponsePrevTab => viewer.step_tab(false),
        ActionId::ResponseRaw => viewer.toggle_raw(),
        ActionId::ResponseDirection => {
            viewer.cycle_direction();
        }
        ActionId::ResponseFilter => open_box(viewer, BoxKind::Filter),
        ActionId::ResponseSearch => open_box(viewer, BoxKind::Search),
        ActionId::SearchNext => viewer.search_step(true),
        _ => viewer.search_step(false),
    }
    Some(())
}

fn open_box(viewer: &mut Viewer, kind: BoxKind) {
    viewer.open_box(kind);
}

/// Movement keys while the response pane is focused.
pub fn list_move(state: &mut State, action: ActionId) -> Vec<Effect> {
    let how = match action {
        ActionId::ListUp => Move::Up,
        ActionId::ListDown => Move::Down,
        ActionId::ListTop => Move::Top,
        ActionId::ListBottom => Move::Bottom,
        ActionId::ListPageUp => Move::PageUp,
        _ => Move::PageDown,
    };
    let Some(tab) = state.tabs.active() else {
        return Vec::new();
    };
    let (height, width) = viewport(state, &tab.viewer);
    let ui = state.ui.clone();
    let Some(tab) = shown(state) else {
        return Vec::new();
    };
    let Some(received) = tab.exchange.as_deref() else {
        return Vec::new();
    };
    let lines = line_count(received, &tab.viewer, width, &ui);
    let source = received.source.clone();
    match (tab.viewer.move_by(how, lines, height), source) {
        (Paging::Load(page), Some(source)) => vec![Effect::ReadPage {
            tab: tab.id,
            page,
            source,
        }],
        _ => Vec::new(),
    }
}

/// `y`: copy the selected JSON value.
fn copy(state: &mut State) -> Vec<Effect> {
    match shown(state).and_then(|t| t.viewer.copy_text()) {
        Some(text) => vec![Effect::Copy(text)],
        None => {
            state.notify(
                Severity::Info,
                "Select a JSON value in the Body tab to copy it",
            );
            Vec::new()
        }
    }
}

/// `D`: load earlier exchanges for the diff picker.
fn diff(state: &mut State) -> Vec<Effect> {
    let Some(dir) = workbench::workspace(state).map(|ws| {
        rustman_core::history::HistoryStore::for_workspace(&ws.root, &ws.settings.history)
            .dir()
            .to_path_buf()
    }) else {
        state.notify(
            Severity::Info,
            "Diffs need a workspace: history is kept there",
        );
        return Vec::new();
    };
    let Some(tab) = shown(state) else {
        state.notify(
            Severity::Info,
            "Send the request first, then diff its exchange",
        );
        return Vec::new();
    };
    let file = tab
        .exchange
        .as_ref()
        .map(|r| r.record.file.clone())
        .unwrap_or_default();
    vec![Effect::LoadExchanges {
        tab: tab.id,
        dir,
        file,
    }]
}

/// `s`: ask where to save the body.
fn prompt_save(state: &mut State) {
    let Some(tab) = shown(state) else {
        return;
    };
    let Some(received) = tab.exchange.as_deref() else {
        return;
    };
    if received.source.is_none() || matches!(received.body, BodyKind::Empty) {
        state.notify(Severity::Info, "There is no body to save");
        return;
    }
    let name = default_name(received.content_type.as_deref());
    let input = Input::new(
        "Save body",
        "Save the response body (secrets redacted) to:",
        Purpose::SaveBody { tab: tab.id },
        &name,
    );
    state.overlay = Some(Overlay::Input(input));
}

fn default_name(content_type: Option<&str>) -> String {
    let ct = content_type.unwrap_or_default().to_ascii_lowercase();
    let extension = [
        ("json", "json"),
        ("png", "png"),
        ("jpeg", "jpg"),
        ("gif", "gif"),
        ("pdf", "pdf"),
        ("xml", "xml"),
        ("html", "html"),
        ("text/", "txt"),
    ]
    .iter()
    .find(|(needle, _)| ct.contains(needle))
    .map_or("bin", |(_, ext)| ext);
    format!("response.{extension}")
}

/// The save prompt was confirmed with `text`.
pub fn save_to(state: &mut State, tab: TabId, text: &str) -> Vec<Effect> {
    state.overlay = None;
    let base: Option<PathBuf> = workbench::workspace(state).map(|ws| ws.root.clone());
    let typed = Path::new(text.trim());
    let path = match base {
        Some(root) if typed.is_relative() => root.join(typed),
        _ => typed.to_path_buf(),
    };
    let source = state
        .tabs
        .get(tab)
        .and_then(|t| t.exchange.as_ref())
        .and_then(|r| r.source.clone());
    match source {
        Some(source) => vec![Effect::SaveBody { source, path }],
        None => Vec::new(),
    }
}

/// Keys for the diff picker and the diff overlays.
pub fn overlay_key(state: &mut State, chord: Chord) -> Vec<Effect> {
    let picked = match &mut state.overlay {
        Some(Overlay::Exchanges(picker)) => picker.key(chord),
        Some(Overlay::Diff(view)) => view.key(chord),
        _ => return Vec::new(),
    };
    match picked {
        Picked::Stay => Vec::new(),
        Picked::Close => {
            state.overlay = None;
            Vec::new()
        }
        Picked::Compute(job) => vec![Effect::Diff(job)],
    }
}

/// A diff finished: show it, unless the picker was closed meanwhile.
fn on_diffed(state: &mut State, view: Box<picker::DiffView>) {
    if matches!(state.overlay, Some(Overlay::Exchanges(_))) {
        state.overlay = Some(Overlay::Diff(view));
    }
}

/// Completions of the viewer's effects.
pub fn on_action(state: &mut State, action: Action) -> Vec<Effect> {
    match action {
        Action::Filtered { tab, expr, result } => {
            if let Some(tab) = state.tabs.get_mut(tab) {
                tab.viewer.filtered(expr, result);
            }
        }
        Action::Searched {
            tab,
            generation,
            matches,
        } => {
            if let Some(tab) = state.tabs.get_mut(tab) {
                tab.viewer.searched(generation, matches);
            }
        }
        Action::PageRead { tab, page, result } => on_page(state, tab, page, result),
        Action::ExchangesLoaded { tab, result } => on_exchanges(state, tab, result),
        Action::Diffed(view) => on_diffed(state, view),
        Action::BodySaved { path, result } => match result {
            Ok(()) => state.notify(Severity::Success, format!("Saved {}", path.display())),
            Err(e) => state.notify(Severity::Error, format!("Cannot save the body: {e}")),
        },
        Action::Copied { text, tool } => on_copied(state, text, tool),
        Action::ReportWritten(result) => crate::response::html::on_written(state, result),
        _ => {}
    }
    Vec::new()
}

fn on_page(state: &mut State, tab: TabId, page: u64, result: Result<Vec<String>, String>) {
    let Some(tab) = state.tabs.get_mut(tab) else {
        return;
    };
    let size = tab.exchange.as_ref().map_or(0, |r| r.size);
    match result {
        Ok(lines) => {
            let last = received::page_count(size) - 1;
            tab.viewer.page_loaded(page, lines, last);
        }
        Err(e) => {
            if let Some(page) = &mut tab.viewer.page {
                page.loading = false;
            }
            state.notify(Severity::Error, format!("Cannot read the body: {e}"));
        }
    }
}

fn on_exchanges(state: &mut State, tab: TabId, result: Result<Vec<picker::Earlier>, String>) {
    let current = state
        .tabs
        .get(tab)
        .and_then(|t| t.exchange.as_deref())
        .map(picker::current);
    match (result, current) {
        (Ok(items), Some(current)) => {
            let picker = Picker {
                items,
                selected: 0,
                current,
                pending: false,
            };
            state.overlay = Some(Overlay::Exchanges(Box::new(picker)));
        }
        (Err(e), _) => state.notify(Severity::Error, format!("Cannot read history: {e}")),
        _ => {}
    }
}

/// Copied with a tool, or else hand the text to the terminal (OSC 52).
fn on_copied(state: &mut State, text: String, tool: Option<&'static str>) {
    let shown: String = text.chars().take(40).collect();
    let ellipsis = if shown.len() < text.len() {
        state.ui.glyphs.ellipsis
    } else {
        ""
    };
    let how = match tool {
        Some(tool) => format!("with {tool}"),
        None => {
            state.clipboard = Some(text.clone());
            "through the terminal (OSC 52)".to_owned()
        }
    };
    state.notify(Severity::Success, format!("Copied {shown}{ellipsis} {how}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_names_follow_the_content_type() {
        assert_eq!(default_name(Some("image/png")), "response.png");
        assert_eq!(default_name(Some("application/json")), "response.json");
        assert_eq!(default_name(Some("text/plain")), "response.txt");
        assert_eq!(default_name(None), "response.bin");
    }
}
