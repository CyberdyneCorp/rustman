//! Request tabs (spec: Multiple request tabs, Unsaved change tracking).
//!
//! Each tab owns its editor, its last exchange and its in-flight state, so
//! switching tabs never touches another tab's request. A tab is dirty when
//! its request, written as YAML, differs from what its file holds.

use std::path::{Path, PathBuf};
use std::time::Instant;

use rustman_core::format::write::request_to_yaml;
use rustman_core::model::Request;
use rustman_core::run::RequestRecord;
use tokio_util::sync::CancellationToken;

use crate::http_pane::HttpEditor;
use crate::pane::{PaneEditor, Registry};
use crate::response::{Received, Viewer};

/// A tab's stable id: positions change as tabs close, ids do not.
pub type TabId = u64;

#[derive(Debug)]
pub struct Tab {
    pub id: TabId,
    /// The request file, relative to the workspace root; `None` for a new
    /// request that was never saved.
    pub path: Option<PathBuf>,
    pub editor: Box<dyn PaneEditor>,
    /// The request as its file holds it, normalised through the editor
    /// (`None` until it is saved once).
    pub saved: Option<String>,
    /// The file's new version while a conflict is unresolved.
    pub on_disk: Option<Box<Request>>,
    pub dirty: bool,
    /// The file changed on disk while the tab had unsaved changes.
    pub conflict: bool,
    /// The request in flight, if any.
    pub in_flight: Option<InFlight>,
    /// The tab's last exchange.
    pub exchange: Option<Box<Received>>,
    /// How the last exchange is being viewed.
    pub viewer: Viewer,
    /// How many sends this tab started.
    pub sends: u64,
}

/// A send in progress (spec: Non-blocking send).
#[derive(Debug, Clone)]
pub struct InFlight {
    /// Which send of this tab it is; a completion for another is stale.
    pub seq: u64,
    pub started: Instant,
    /// Cancelled by `Esc` / `Ctrl+C`, closing the tab, or quitting.
    pub token: CancellationToken,
}

impl Tab {
    /// The title: the file name without `.rman.yaml`, or `untitled`.
    pub fn title(&self) -> String {
        let Some(path) = &self.path else {
            return "untitled".to_owned();
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        name.strip_suffix(rustman_core::model::REQUEST_FILE_SUFFIX)
            .unwrap_or(&name)
            .to_owned()
    }

    /// The editor's request as YAML; `None` while it does not parse.
    pub fn yaml(&self) -> Option<String> {
        self.editor.request().ok().map(|r| request_to_yaml(&r))
    }

    /// Recompute the dirty flag after an edit.
    pub fn refresh(&mut self) {
        self.dirty = match (&self.saved, self.yaml()) {
            (Some(saved), Some(now)) => *saved != now,
            _ => true,
        };
    }

    /// Replace the editor with the file's new contents.
    pub fn reload(&mut self, request: &Request, registry: &Registry) {
        self.editor = registry.open(request);
        self.saved = self.yaml();
        self.on_disk = None;
        self.conflict = false;
        self.refresh();
    }

    /// The file was written: the tab is clean.
    pub fn mark_saved(&mut self, path: PathBuf) {
        self.path = Some(path);
        self.saved = self.yaml();
        self.conflict = false;
        self.refresh();
    }

    /// A request was sent from this tab; returns its send number and
    /// cancellation token. A send still in flight is cancelled.
    pub fn start(&mut self, now: Instant) -> (u64, CancellationToken) {
        if let Some(previous) = &self.in_flight {
            previous.token.cancel();
        }
        self.sends += 1;
        let seq = self.sends;
        let token = CancellationToken::new();
        self.in_flight = Some(InFlight {
            seq,
            started: now,
            token: token.clone(),
        });
        (seq, token)
    }

    /// Send `seq` finished with `received`; a stale completion (cancelled,
    /// or superseded) is dropped. Returns whether it was shown.
    pub fn finish(&mut self, seq: u64, received: Received) -> bool {
        if self.in_flight.as_ref().map(|f| f.seq) != Some(seq) {
            return false;
        }
        self.in_flight = None;
        self.show(received);
        true
    }

    /// Show `received` as the tab's last exchange, in a fresh viewer.
    pub fn show(&mut self, received: Received) {
        self.viewer = Viewer::new(&received);
        self.exchange = Some(Box::new(received));
    }

    /// Cancel the request in flight (spec: Cancel in-flight request): the
    /// connection is dropped and the exchange is recorded as cancelled
    /// with its elapsed time. False when nothing is in flight.
    pub fn cancel(&mut self, now: Instant) -> bool {
        let Some(flight) = self.in_flight.take() else {
            return false;
        };
        flight.token.cancel();
        let elapsed = now.saturating_duration_since(flight.started);
        let file = self
            .path
            .as_deref()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let record = RequestRecord::new(self.title(), self.title(), file, 1);
        let elapsed_ms = elapsed.as_secs_f64() * 1000.0;
        self.show(Received::cancelled(record, elapsed_ms));
        true
    }
}

/// The open tabs and which one is shown.
#[derive(Debug, Default)]
pub struct Tabs {
    pub list: Vec<Tab>,
    pub active: usize,
    next_id: TabId,
}

impl Tabs {
    pub fn active(&self) -> Option<&Tab> {
        self.list.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut Tab> {
        self.list.get_mut(self.active)
    }

    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.list.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.list.iter_mut().find(|t| t.id == id)
    }

    /// The tab of the request file at `path`.
    pub fn find(&self, path: &Path) -> Option<&Tab> {
        self.list.iter().find(|t| t.path.as_deref() == Some(path))
    }

    pub fn is_dirty(&self, path: &Path) -> bool {
        self.find(path).is_some_and(|t| t.dirty)
    }

    pub fn any_dirty(&self) -> bool {
        self.list.iter().any(|t| t.dirty)
    }

    pub fn in_flight(&self) -> usize {
        self.list.iter().filter(|t| t.in_flight.is_some()).count()
    }

    /// Focus the tab of `path`, or open one for `request` (spec: Open a
    /// request). Returns the tab's id.
    pub fn open(&mut self, path: &Path, request: &Request, registry: &Registry) -> TabId {
        if let Some(i) = self
            .list
            .iter()
            .position(|t| t.path.as_deref() == Some(path))
        {
            self.active = i;
            return self.list[i].id;
        }
        let editor = registry.open(request);
        let id = self.push(Some(path.to_path_buf()), editor);
        let tab = self.list.last_mut().expect("just pushed");
        tab.saved = tab.yaml();
        tab.refresh();
        id
    }

    /// Open a new, never saved request (`Ctrl+T`).
    pub fn open_new(&mut self) -> TabId {
        let id = self.push(None, Box::new(HttpEditor::blank()));
        if let Some(tab) = self.list.last_mut() {
            tab.dirty = false;
        }
        id
    }

    fn push(&mut self, path: Option<PathBuf>, editor: Box<dyn PaneEditor>) -> TabId {
        self.next_id += 1;
        let id = self.next_id;
        self.list.push(Tab {
            id,
            path,
            editor,
            saved: None,
            on_disk: None,
            dirty: false,
            conflict: false,
            in_flight: None,
            exchange: None,
            viewer: Viewer::default(),
            sends: 0,
        });
        self.active = self.list.len() - 1;
        id
    }

    /// Close the tab `id`; the one to its left (or the new first) is shown.
    pub fn close(&mut self, id: TabId) -> Option<Tab> {
        let i = self.list.iter().position(|t| t.id == id)?;
        let tab = self.list.remove(i);
        if let Some(flight) = &tab.in_flight {
            flight.token.cancel();
        }
        if self.active >= i && self.active > 0 {
            self.active -= 1;
        }
        Some(tab)
    }

    /// Show the tab at `index` (0-based), if there is one.
    pub fn select(&mut self, index: usize) {
        if index < self.list.len() {
            self.active = index;
        }
    }

    /// Show the next (or previous) tab, wrapping around.
    pub fn step(&mut self, forward: bool) {
        let n = self.list.len();
        if n == 0 {
            return;
        }
        self.active = if forward {
            (self.active + 1) % n
        } else {
            (self.active + n - 1) % n
        };
    }

    /// Follow a renamed file.
    pub fn renamed(&mut self, from: &Path, to: &Path) {
        for tab in &mut self.list {
            if let Some(path) = &tab.path
                && let Ok(rest) = path.strip_prefix(from)
            {
                tab.path = Some(to.join(rest));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::focus::Focus;
    use crate::keymap::chord::parse_chord;
    use rustman_core::model::KindRegistry;

    fn request(url: &str) -> Request {
        let text = format!("version: 1\nkind: http\nname: R\nurl: {url}\n");
        KindRegistry::default().load(Path::new("r"), &text).unwrap()
    }

    fn type_url(tab: &mut Tab, text: &str) {
        for c in text.chars() {
            let key = parse_chord(&c.to_string()).unwrap();
            tab.editor.key(Focus::Url, key, None);
        }
        tab.refresh();
    }

    #[test]
    fn opening_a_request_twice_focuses_its_tab() {
        let registry = Registry::builtin();
        let mut tabs = Tabs::default();
        let a = tabs.open(Path::new("a.rman.yaml"), &request("http://a/"), &registry);
        let b = tabs.open(Path::new("b.rman.yaml"), &request("http://b/"), &registry);
        assert_eq!(tabs.active, 1);
        assert_eq!(
            tabs.open(Path::new("a.rman.yaml"), &request("x"), &registry),
            a
        );
        assert_eq!((tabs.active, tabs.list.len()), (0, 2));
        assert_eq!(tabs.active().unwrap().title(), "a");
        tabs.step(false);
        assert_eq!(tabs.active().unwrap().id, b);
        tabs.close(b);
        assert_eq!((tabs.active, tabs.list.len()), (0, 1));
        assert!(tabs.close(b).is_none());
    }

    #[test]
    fn edits_make_a_tab_dirty_and_saving_cleans_it() {
        let registry = Registry::builtin();
        let mut tabs = Tabs::default();
        tabs.open(Path::new("a.rman.yaml"), &request("http://a/"), &registry);
        let tab = tabs.active_mut().unwrap();
        assert!(!tab.dirty);
        type_url(tab, "x");
        assert!(tab.dirty);
        tab.mark_saved("a.rman.yaml".into());
        assert!(!tab.dirty);
        assert!(!tabs.any_dirty());
        let new = tabs.open_new();
        assert_eq!(tabs.get(new).unwrap().title(), "untitled");
        assert!(!tabs.get(new).unwrap().dirty, "a blank tab is clean");
    }

    #[test]
    fn tabs_keep_their_own_in_flight_state() {
        let registry = Registry::builtin();
        let mut tabs = Tabs::default();
        let a = tabs.open(Path::new("a.rman.yaml"), &request("http://a/"), &registry);
        let (seq, token) = tabs.get_mut(a).unwrap().start(Instant::now());
        tabs.open_new();
        assert_eq!(tabs.in_flight(), 1);
        let tab = tabs.get_mut(a).unwrap();
        let (again, second) = tab.start(Instant::now());
        assert_eq!(again, seq + 1);
        assert!(token.is_cancelled(), "a new send cancels the previous one");
        let stale = Received::without_body(RequestRecord::new(
            "a".into(),
            "a".into(),
            "a.rman.yaml".into(),
            1,
        ));
        assert!(!tab.finish(seq, stale), "a superseded send is dropped");
        tabs.renamed(Path::new("a.rman.yaml"), Path::new("b.rman.yaml"));
        assert!(tabs.find(Path::new("b.rman.yaml")).is_some());
        tabs.close(a);
        assert!(second.is_cancelled(), "closing a tab cancels its send");
    }
}
