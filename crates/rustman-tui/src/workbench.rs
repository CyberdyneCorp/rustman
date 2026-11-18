//! The request workbench: opening requests in tabs, saving, closing and
//! quitting with unsaved changes, creating, renaming and deleting files from
//! the sidebar, and following changes made on disk (spec: Collection tree
//! sidebar, Multiple request tabs, Unsaved change tracking, External editor
//! for bodies). Every function is synchronous; I/O is returned as effects.

use std::path::{Path, PathBuf};

use rustman_core::Workspace;
use rustman_core::model::Request;

use crate::app::{Effect, Overlay, Screen, Session, State};
use crate::dialogs::{Confirm, Decision, Input, Purpose, Typed};
use crate::files::{self, FileOp};
use crate::focus::Focus;
use crate::layout::Breakpoint;
use crate::pane::Registry;
use crate::tabs::TabId;
use crate::toast::Severity;
use crate::tree::{self, RowKind, TreeRow};

/// Work the workbench is waiting for.
#[derive(Debug, Default)]
pub struct Pending {
    /// A workspace reload is running.
    pub reloading: bool,
    /// Files changed while it ran: reload once more after it.
    pub reload_again: bool,
    /// Tabs being saved.
    pub saves: Vec<TabId>,
    /// Tabs to close once saved.
    pub close_after_save: Vec<TabId>,
    /// Quit once every save succeeded.
    pub quit_after_save: bool,
    /// Open this request once the workspace is reloaded (a new request).
    pub open_after_reload: Option<PathBuf>,
    /// Renames running: tabs follow once they succeed.
    pub renames: Vec<(PathBuf, PathBuf)>,
    /// Conflicts found while another dialog was open, asked about once it
    /// closes.
    pub conflicts: Vec<TabId>,
}

pub fn workspace(state: &State) -> Option<&Workspace> {
    match &state.screen {
        Screen::Shell(Session::Workspace(ws)) => Some(ws),
        _ => None,
    }
}

/// The visible sidebar rows.
pub fn sidebar_rows(state: &State) -> Vec<TreeRow> {
    workspace(state).map_or_else(Vec::new, |ws| tree::rows(ws, &state.shell.tree))
}

/// Reload the workspace (coalesced while one is running).
pub fn reload(state: &mut State) -> Vec<Effect> {
    let Some(root) = workspace(state).map(|ws| ws.root.clone()) else {
        return Vec::new();
    };
    if state.pending.reloading {
        state.pending.reload_again = true;
        return Vec::new();
    }
    state.pending.reloading = true;
    vec![Effect::LoadWorkspace(root)]
}

// ---- opening and tabs ----------------------------------------------------

/// `Enter` in the sidebar: open the selected request, or fold the folder.
pub fn open_selected(state: &mut State) -> Vec<Effect> {
    let rows = sidebar_rows(state);
    let Some(row) = rows.get(state.shell.selected) else {
        return Vec::new();
    };
    match row.kind {
        RowKind::Folder { collapsed } => set_collapsed(state, &row.path, !collapsed),
        RowKind::Request(_) => {
            if let Some(filter) = &mut state.shell.tree.filter {
                filter.editing = false;
            }
            open_path(state, &row.path);
        }
    }
    Vec::new()
}

/// Fold or unfold a folder row.
pub fn set_collapsed(state: &mut State, path: &Path, collapsed: bool) {
    let set = &mut state.shell.tree.collapsed;
    if collapsed {
        set.insert(path.to_path_buf());
    } else {
        set.remove(path);
    }
}

/// `Left`/`Right` on the selected folder.
pub fn fold_selected(state: &mut State, collapse: bool) {
    let rows = sidebar_rows(state);
    if let Some(row) = rows.get(state.shell.selected)
        && !row.is_request()
    {
        set_collapsed(state, &row.path.clone(), collapse);
    }
}

/// Open the request at the workspace-relative `path` in a tab (or focus
/// its tab) and move the focus to the editor.
pub fn open_path(state: &mut State, path: &Path) {
    let Some(request) = workspace(state).and_then(|ws| find(ws, path)).cloned() else {
        return;
    };
    state.tabs.open(path, &request, &Registry::builtin());
    state.shell.focus = Focus::Url;
    state.shell.overlay_open = false;
}

fn find<'a>(ws: &'a Workspace, path: &Path) -> Option<&'a Request> {
    ws.requests()
        .into_iter()
        .find(|r| r.entry.path == path)
        .map(|r| &r.entry.request)
}

/// `Ctrl+T`: a new, unsaved request.
pub fn new_tab(state: &mut State) {
    state.tabs.open_new();
    state.shell.focus = Focus::Url;
}

/// `Ctrl+W`: close the current tab, asking first when it is dirty.
pub fn close_tab(state: &mut State) {
    let Some(tab) = state.tabs.active() else {
        return;
    };
    if tab.dirty {
        let confirm = Confirm::close_tab(tab.id, &tab.title());
        state.overlay = Some(Overlay::Confirm(confirm));
    } else {
        let id = tab.id;
        state.tabs.close(id);
    }
}

/// `Ctrl+Q`: quit, asking first when tabs are dirty.
pub fn quit(state: &mut State) {
    let dirty = state.tabs.list.iter().filter(|t| t.dirty).count();
    if dirty == 0 {
        state.quit = true;
    } else {
        state.overlay = Some(Overlay::Confirm(Confirm::quit(dirty)));
    }
}

/// Cancel every request in flight (the loop ends).
pub fn cancel_all(state: &mut State) {
    let now = state.now;
    for tab in &mut state.tabs.list {
        tab.cancel(now);
    }
    state.status.in_flight = 0;
}

/// Alt+M: cycle the body mode or auth type of the current tab.
pub fn cycle_mode(state: &mut State) {
    if let Some(tab) = state.tabs.active_mut()
        && tab.editor.cycle_mode()
    {
        tab.refresh();
    }
}

// ---- saving ----------------------------------------------------------------

/// `Ctrl+S`: save the current tab (asking for a name if it has none).
pub fn save(state: &mut State) -> Vec<Effect> {
    let Some(id) = state.tabs.active().map(|t| t.id) else {
        return Vec::new();
    };
    save_tab(state, id)
}

fn save_tab(state: &mut State, id: TabId) -> Vec<Effect> {
    let Some(root) = workspace(state).map(|ws| ws.root.clone()) else {
        state.notify(Severity::Warn, "Scratch session: nothing is saved");
        return Vec::new();
    };
    let Some(tab) = state.tabs.get(id) else {
        return Vec::new();
    };
    let Some(path) = tab.path.clone() else {
        let input = Input::new("Save as", "File name:", Purpose::SaveAs { tab: id }, "");
        state.overlay = Some(Overlay::Input(input));
        return Vec::new();
    };
    match tab.editor.request() {
        Ok(request) => {
            let text = rustman_core::format::write::request_to_yaml(&request);
            state.pending.saves.push(id);
            vec![Effect::Save {
                tab: id,
                path: path.clone(),
                op: FileOp::Write {
                    path: root.join(&path),
                    text,
                },
            }]
        }
        Err(message) => {
            state.notify(
                Severity::Error,
                format!("Cannot save {}: {message}", tab.title()),
            );
            abandon_quit(state);
            Vec::new()
        }
    }
}

/// A save finished.
pub fn on_saved(
    state: &mut State,
    id: TabId,
    path: PathBuf,
    result: Result<(), String>,
) -> Vec<Effect> {
    state.pending.saves.retain(|t| *t != id);
    let shown = tree::display_path(&path);
    match result {
        Ok(()) => {
            if let Some(tab) = state.tabs.get_mut(id) {
                tab.mark_saved(path);
            }
            state.notify(Severity::Success, format!("Saved {shown}"));
        }
        Err(message) => {
            state.notify(Severity::Error, format!("Cannot save {shown}: {message}"));
            state.pending.close_after_save.retain(|t| *t != id);
            abandon_quit(state);
        }
    }
    after_saves(state);
    reload(state)
}

fn after_saves(state: &mut State) {
    let closing: Vec<TabId> = state.pending.close_after_save.drain(..).collect();
    for id in closing {
        if state.pending.saves.contains(&id) {
            state.pending.close_after_save.push(id);
        } else {
            state.tabs.close(id);
        }
    }
    if state.pending.quit_after_save && state.pending.saves.is_empty() && !state.tabs.any_dirty() {
        state.quit = true;
    }
}

fn abandon_quit(state: &mut State) {
    state.pending.quit_after_save = false;
}

// ---- prompts -----------------------------------------------------------------

/// A choice in a Confirm dialog.
pub fn decide(state: &mut State, decision: Decision) -> Vec<Effect> {
    match decision {
        Decision::Cancel => {
            crate::webhook::cancel_confirm(state);
            Vec::new()
        }
        Decision::WebhookStart => crate::webhook::confirm_remote(state),
        Decision::SaveAndClose(id) => {
            state.pending.close_after_save.push(id);
            save_tab(state, id)
        }
        Decision::DiscardAndClose(id) => {
            state.tabs.close(id);
            Vec::new()
        }
        Decision::SaveAllAndQuit => save_all_and_quit(state),
        Decision::DiscardAndQuit => {
            state.quit = true;
            Vec::new()
        }
        Decision::Delete(path) => delete(state, path),
        Decision::KeepMine(id) => {
            if let Some(tab) = state.tabs.get_mut(id) {
                tab.conflict = false;
            }
            Vec::new()
        }
        Decision::LoadFromDisk(id) => load_from_disk(state, id),
    }
}

fn save_all_and_quit(state: &mut State) -> Vec<Effect> {
    state.pending.quit_after_save = true;
    let dirty: Vec<TabId> = state
        .tabs
        .list
        .iter()
        .filter(|t| t.dirty)
        .map(|t| t.id)
        .collect();
    let mut effects = Vec::new();
    for id in dirty {
        effects.extend(save_tab(state, id));
        if state.overlay.is_some() {
            // A new tab needs a name first; quitting waits for the user.
            abandon_quit(state);
            return effects;
        }
    }
    after_saves(state);
    effects
}

fn load_from_disk(state: &mut State, id: TabId) -> Vec<Effect> {
    let Some(tab) = state.tabs.get_mut(id) else {
        return Vec::new();
    };
    if let Some(request) = tab.on_disk.take() {
        tab.reload(&request, &Registry::builtin());
    }
    Vec::new()
}

/// A key while a text prompt is open.
pub fn input_key(state: &mut State, chord: crate::keymap::Chord) -> Vec<Effect> {
    let Some(Overlay::Input(input)) = &mut state.overlay else {
        return Vec::new();
    };
    match input.key(chord) {
        Typed::Editing => Vec::new(),
        Typed::Cancel => {
            state.overlay = None;
            Vec::new()
        }
        Typed::Submit(text) => match input.purpose.clone() {
            Purpose::SaveBody { tab } => crate::viewer_keys::save_to(state, tab, &text),
            Purpose::Webhook(prompt) => crate::webhook::submit(state, prompt, &text),
            purpose => submit(state, purpose, &text),
        },
    }
}

fn submit(state: &mut State, purpose: Purpose, text: &str) -> Vec<Effect> {
    let Some(root) = workspace(state).map(|ws| ws.root.clone()) else {
        state.overlay = None;
        return Vec::new();
    };
    let Some(name) = files::request_file_name(text) else {
        set_input_error(state, "Type a name without `..` or a drive.");
        return Vec::new();
    };
    state.overlay = None;
    match purpose {
        Purpose::NewRequest { folder } => {
            let path = folder.join(&name);
            let stem = name.file_name().map(|n| n.to_string_lossy().into_owned());
            let title = stem.unwrap_or_default();
            let title = title.trim_end_matches(rustman_core::model::REQUEST_FILE_SUFFIX);
            state.pending.open_after_reload = Some(path.clone());
            let op = FileOp::Create {
                path: root.join(&path),
                text: files::new_request_text(title),
            };
            vec![Effect::File(op)]
        }
        Purpose::Rename { from } => rename(state, &root, from, text),
        Purpose::SaveAs { tab } => {
            if let Some(t) = state.tabs.get_mut(tab) {
                t.path = Some(name);
            }
            save_tab(state, tab)
        }
        Purpose::SaveBody { .. } | Purpose::Webhook(_) => Vec::new(),
    }
}

fn set_input_error(state: &mut State, message: &str) {
    if let Some(Overlay::Input(input)) = &mut state.overlay {
        input.error = Some(message.to_owned());
    }
}

fn rename(state: &mut State, root: &Path, from: PathBuf, typed: &str) -> Vec<Effect> {
    let is_request = from
        .to_string_lossy()
        .ends_with(rustman_core::model::REQUEST_FILE_SUFFIX);
    let new_name = if is_request {
        files::request_file_name(typed)
    } else {
        Some(PathBuf::from(typed.trim()))
    };
    let Some(new_name) = new_name.filter(|n| n.components().count() == 1) else {
        state.notify(Severity::Warn, "A new name cannot contain `/`");
        return Vec::new();
    };
    let to = from.parent().unwrap_or(Path::new("")).join(new_name);
    let op = FileOp::Rename {
        from: root.join(&from),
        to: root.join(&to),
    };
    state.pending.renames.push((from, to));
    vec![Effect::File(op)]
}

// ---- sidebar commands ---------------------------------------------------------

/// `n`: a new request in the selected folder.
pub fn prompt_new(state: &mut State) {
    if workspace(state).is_none() {
        state.notify(Severity::Warn, "Scratch session: nothing is saved");
        return;
    }
    let rows = sidebar_rows(state);
    let folder = tree::folder_of(rows.get(state.shell.selected));
    let shown = if folder.as_os_str().is_empty() {
        "the workspace root".to_owned()
    } else {
        tree::display_path(&folder)
    };
    let label = format!("Name of the new request in {shown} (`folder/name` makes folders):");
    let input = Input::new("New request", &label, Purpose::NewRequest { folder }, "");
    state.overlay = Some(Overlay::Input(input));
}

/// `r`: rename the selected request or folder.
pub fn prompt_rename(state: &mut State) {
    let rows = sidebar_rows(state);
    let Some(row) = rows.get(state.shell.selected) else {
        return;
    };
    let current = row
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let current = current.trim_end_matches(rustman_core::model::REQUEST_FILE_SUFFIX);
    let purpose = Purpose::Rename {
        from: row.path.clone(),
    };
    let input = Input::new("Rename", "New name:", purpose, current);
    state.overlay = Some(Overlay::Input(input));
}

/// `d`: delete the selected request or folder, after confirmation.
pub fn prompt_delete(state: &mut State) {
    let rows = sidebar_rows(state);
    let Some(row) = rows.get(state.shell.selected) else {
        return;
    };
    let shown = tree::display_path(&row.path);
    state.overlay = Some(Overlay::Confirm(Confirm::delete(row.path.clone(), &shown)));
}

fn delete(state: &mut State, path: PathBuf) -> Vec<Effect> {
    let Some(root) = workspace(state).map(|ws| ws.root.clone()) else {
        return Vec::new();
    };
    vec![Effect::File(FileOp::Delete {
        path: root.join(path),
    })]
}

/// A sidebar file operation finished.
pub fn on_file_done(state: &mut State, op: &FileOp, result: Result<(), String>) -> Vec<Effect> {
    if let Err(message) = result {
        state.notify(Severity::Error, message);
        state.pending.open_after_reload = None;
        state.pending.renames.clear();
        return Vec::new();
    }
    let root = workspace(state)
        .map(|ws| ws.root.clone())
        .unwrap_or_default();
    let rel = |p: &Path| p.strip_prefix(&root).unwrap_or(p).to_path_buf();
    match op {
        FileOp::Delete { path } => {
            let gone = rel(path);
            let closing: Vec<TabId> = state
                .tabs
                .list
                .iter()
                .filter(|t| t.path.as_deref().is_some_and(|p| p.starts_with(&gone)))
                .map(|t| t.id)
                .collect();
            closing
                .into_iter()
                .for_each(|id| drop(state.tabs.close(id)));
            state.notify(
                Severity::Success,
                format!("Deleted {}", tree::display_path(&gone)),
            );
        }
        FileOp::Rename { .. } => {
            for (from, to) in std::mem::take(&mut state.pending.renames) {
                state.tabs.renamed(&from, &to);
            }
        }
        FileOp::Create { .. } | FileOp::Write { .. } => {}
    }
    reload(state)
}

// ---- the workspace and the disk ---------------------------------------------------

/// The workspace finished (re)loading while the shell is open.
pub fn on_reloaded(state: &mut State, result: Result<Box<Workspace>, String>) -> Vec<Effect> {
    state.pending.reloading = false;
    match result {
        Ok(ws) => state.screen = Screen::Shell(Session::Workspace(ws)),
        Err(message) => state.notify(Severity::Warn, format!("Workspace not reloaded: {message}")),
    }
    if let Some(path) = state.pending.open_after_reload.take() {
        open_path(state, &path);
        select_path(state, &path);
    }
    clamp_selection(state);
    if std::mem::take(&mut state.pending.reload_again) {
        return reload(state);
    }
    Vec::new()
}

fn select_path(state: &mut State, path: &Path) {
    if let Some(i) = sidebar_rows(state).iter().position(|r| r.path == path) {
        state.shell.selected = i;
    }
}

fn clamp_selection(state: &mut State) {
    let last = sidebar_rows(state).len().saturating_sub(1);
    state.shell.selected = state.shell.selected.min(last);
}

/// Files changed on disk: reload the workspace and re-read open requests.
pub fn on_files_changed(state: &mut State, paths: &[PathBuf]) -> Vec<Effect> {
    let Some(root) = workspace(state).map(|ws| ws.root.clone()) else {
        return Vec::new();
    };
    let mut effects = reload(state);
    for tab in &state.tabs.list {
        if let Some(path) = tab.path.as_ref().filter(|p| paths.contains(p)) {
            effects.push(Effect::ReadRequest {
                tab: tab.id,
                root: root.clone(),
                path: path.clone(),
            });
        }
    }
    effects
}

/// An open request was re-read after a change on disk: reload a clean tab,
/// warn about a dirty one (spec: External edit conflict).
pub fn on_request_read(state: &mut State, id: TabId, result: Result<Box<Request>, String>) {
    let registry = Registry::builtin();
    let Some(tab) = state.tabs.get_mut(id) else {
        return;
    };
    let title = tab.title();
    let request = match result {
        Ok(request) => request,
        Err(message) => {
            if !state.pending.saves.contains(&id) {
                tab.saved = None;
                tab.refresh();
                state.notify(
                    Severity::Warn,
                    format!("{title} changed on disk: {message}"),
                );
            }
            return;
        }
    };
    let on_disk = registry.open(&request).request().ok();
    let normalised = on_disk.map(|r| rustman_core::format::write::request_to_yaml(&r));
    if normalised.is_some() && normalised == tab.saved {
        return;
    }
    if tab.dirty {
        tab.conflict = true;
        tab.on_disk = Some(request);
        if !state.pending.conflicts.contains(&id) {
            state.pending.conflicts.push(id);
        }
        ask_conflict(state);
    } else {
        tab.reload(&request, &registry);
        state.notify(Severity::Info, format!("Reloaded {title} from disk"));
    }
}

/// Show the next unresolved external edit conflict, unless another dialog
/// is open (it is never replaced; the conflict waits for it to close).
pub fn ask_conflict(state: &mut State) {
    if state.overlay.is_some() {
        return;
    }
    while !state.pending.conflicts.is_empty() {
        let id = state.pending.conflicts.remove(0);
        let Some(tab) = state.tabs.get(id).filter(|t| t.conflict) else {
            continue;
        };
        let shown = tab
            .path
            .as_deref()
            .map_or_else(|| tab.title(), tree::display_path);
        state.overlay = Some(Overlay::Confirm(Confirm::conflict(id, &shown)));
        return;
    }
}

/// `Ctrl+E`: edit the current body in the external editor.
pub fn external(state: &mut State) -> Vec<Effect> {
    let Some(tab) = state.tabs.active() else {
        return Vec::new();
    };
    match tab.editor.external_text() {
        Some((text, extension)) => vec![Effect::EditExternally {
            tab: tab.id,
            text,
            extension,
        }],
        None => {
            state.notify(
                Severity::Info,
                "Ctrl+E edits raw bodies: switch the body to raw with Alt+M",
            );
            Vec::new()
        }
    }
}

/// The external editor returned.
pub fn on_external(state: &mut State, id: TabId, result: Result<crate::external::Edited, String>) {
    match result {
        Ok(edited) => {
            if let Some(notice) = edited.fallback {
                state.notify(Severity::Warn, format!("External editor: {notice}"));
            }
            if let Some(tab) = state.tabs.get_mut(id) {
                tab.editor.set_external_text(&edited.text);
                tab.refresh();
            }
        }
        Err(message) => state.notify(Severity::Error, format!("External editor: {message}")),
    }
}

/// Reveal `path` in the sidebar (palette): select it, then open it.
pub fn reveal(state: &mut State, path: &Path) {
    state.shell.tree.filter = None;
    if let Some(parent) = path.parent() {
        for ancestor in parent.ancestors() {
            state.shell.tree.collapsed.remove(ancestor);
        }
    }
    select_path(state, path);
    if Breakpoint::of(state.area.width, state.area.height) == Breakpoint::Wide {
        state.shell.sidebar_hidden = false;
    }
    open_path(state, path);
}
