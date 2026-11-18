//! The response viewer (spec: Response summary line, Response viewer tabs,
//! JSON tree viewer, JSON filter and search, Highlighting for non-JSON
//! bodies, Large response handling, Timing waterfall, Latency sparkline
//! from history, Diff two exchanges).
//!
//! [`Received`] is the exchange, prepared off the render loop; [`Viewer`] is
//! the per-tab viewing state (tab, scroll, folds, filter, search) that keys
//! change in `update`.

pub mod diff;
pub mod find;
pub mod graphql;
pub mod html;
pub mod json_tree;
pub mod picker;
pub mod received;
pub mod text;
pub mod timing;
pub mod view;
#[cfg(test)]
mod view_tests;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio_util::sync::CancellationToken;

pub use received::{BodyKind, Captured, RawBody, Received, Source};

use crate::keymap::chord::{Chord, Code};
use json_tree::{JsonTree, TreeView};

/// The viewer tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResponseTab {
    #[default]
    Body,
    Headers,
    Timing,
    Assertions,
    /// GraphQL: `data`.
    Data,
    /// GraphQL: `errors`.
    Errors,
    /// GraphQL: `extensions`.
    Extensions,
    /// GraphQL: a subscription's events.
    Events,
    /// A failed, missing or new snapshot and its review.
    Snapshot,
}

impl ResponseTab {
    /// The tabs of an HTTP exchange.
    pub const ALL: [Self; 4] = [Self::Body, Self::Headers, Self::Timing, Self::Assertions];

    /// The tabs of a GraphQL exchange.
    pub const GRAPHQL: [Self; 8] = [
        Self::Data,
        Self::Errors,
        Self::Extensions,
        Self::Events,
        Self::Body,
        Self::Headers,
        Self::Timing,
        Self::Assertions,
    ];

    /// The tabs of an HTTP exchange with a snapshot to review.
    pub const WITH_SNAPSHOT: [Self; 5] = [
        Self::Body,
        Self::Headers,
        Self::Timing,
        Self::Assertions,
        Self::Snapshot,
    ];

    /// The tabs of a GraphQL exchange with a snapshot to review.
    pub const GRAPHQL_WITH_SNAPSHOT: [Self; 9] = [
        Self::Data,
        Self::Errors,
        Self::Extensions,
        Self::Events,
        Self::Body,
        Self::Headers,
        Self::Timing,
        Self::Assertions,
        Self::Snapshot,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Body => "Body",
            Self::Headers => "Headers",
            Self::Timing => "Timing",
            Self::Assertions => "Assertions",
            Self::Data => "Data",
            Self::Errors => "Errors",
            Self::Extensions => "Extensions",
            Self::Events => "Events",
            Self::Snapshot => "Snapshot",
        }
    }

    /// Whether the tab shows the GraphQL result.
    pub fn is_graphql(self) -> bool {
        matches!(
            self,
            Self::Data | Self::Errors | Self::Extensions | Self::Events
        )
    }
}

/// What the one-line box under the body is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxKind {
    /// `f`: a JSONPath filter.
    Filter,
    /// `/`: incremental search.
    Search,
}

/// The filter or search box while it is typed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputBox {
    pub kind: BoxKind,
    pub text: String,
    /// The filter's parse error, shown inline.
    pub error: Option<String>,
}

/// A filter that is applied: the matches, as a tree of their own.
#[derive(Debug, Clone)]
pub struct Filtered {
    pub expr: String,
    pub view: TreeView,
}

/// The search query and the nodes it matches in the shown tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    pub query: String,
    pub matches: Vec<usize>,
    /// The matches are still being computed off the render loop.
    pub pending: bool,
}

/// Trees with more nodes than this are searched off the render loop (a
/// scan of a tree this size takes a few milliseconds in a debug build).
pub const SYNC_SEARCH_NODES: usize = 20_000;

/// Generations of off-loop searches, unique across viewers, so a result
/// for an older query, tree or exchange is never taken for the current one.
static SEARCH_GENERATION: AtomicU64 = AtomicU64::new(1);

/// An incremental search over a large tree, run by the effect runner.
#[derive(Debug, Clone)]
pub struct SearchJob {
    pub generation: u64,
    pub tree: Arc<JsonTree>,
    pub query: String,
    /// The previous query and its matches, to narrow instead of rescanning.
    pub previous: Option<(String, Vec<usize>)>,
    /// Cancelled when a newer keystroke or a closed box supersedes the job.
    pub cancel: CancellationToken,
}

impl SearchJob {
    /// The matches, or `None` when cancelled first.
    pub fn run(&self) -> Option<Vec<usize>> {
        let cancelled = || self.cancel.is_cancelled();
        match &self.previous {
            Some((previous, matches)) => {
                find::refine_until(&self.tree, previous, matches, &self.query, &cancelled)
            }
            None => find::search_until(&self.tree, &self.query, &cancelled),
        }
    }
}

/// Jobs are equal when they are the same search (generations are unique).
impl PartialEq for SearchJob {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation && self.query == other.query
    }
}

impl Eq for SearchJob {}

/// One page of the paged raw view.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Page {
    pub number: u64,
    pub lines: Vec<String>,
    pub loading: bool,
}

/// What a key did in the filter or search box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    /// Nothing for the effect runner.
    Handled,
    /// Apply this (parsed) filter off the render loop.
    Filter(String),
    /// Search the (large) tree off the render loop.
    Search(SearchJob),
}

/// Movement keys in the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Up,
    Down,
    Top,
    Bottom,
    PageUp,
    PageDown,
}

/// Where a key moved the paged raw view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paging {
    Stay,
    Load(u64),
}

/// The viewing state of one tab's exchange.
#[derive(Debug, Clone, Default)]
pub struct Viewer {
    pub tab: ResponseTab,
    /// The first line on screen in line-based views.
    pub scroll: usize,
    /// The JSON body's tree.
    pub tree: Option<TreeView>,
    pub filtered: Option<Filtered>,
    /// `r`: pretty-printed text instead of the tree.
    pub raw: bool,
    pub input: Option<InputBox>,
    pub search: Option<Search>,
    /// The paged raw view's current page (bodies above 50 MiB).
    pub page: Option<Page>,
    /// The off-loop search in progress: its generation and cancel token.
    search_job: Option<(u64, CancellationToken)>,
    /// The exchange is a GraphQL one: its result has tabs of its own.
    pub graphql: bool,
    /// The exchange has a snapshot to review: the Snapshot tab is shown.
    pub snapshot: bool,
}

impl Viewer {
    /// A fresh viewer for `received`; the paged view starts on page 0.
    pub fn new(received: &Received) -> Self {
        let tree = match &received.body {
            BodyKind::Json { tree, .. } => Some(TreeView::new(Arc::clone(tree))),
            _ => None,
        };
        let page = matches!(received.body, BodyKind::Paged).then(|| Page {
            loading: true,
            ..Page::default()
        });
        let graphql = received.graphql.is_some();
        Self {
            tree,
            page,
            graphql,
            snapshot: !received.snapshots.is_empty(),
            tab: if graphql {
                ResponseTab::Data
            } else {
                ResponseTab::Body
            },
            ..Self::default()
        }
    }

    /// The tabs this exchange has.
    pub fn tabs(&self) -> &'static [ResponseTab] {
        match (self.graphql, self.snapshot) {
            (true, false) => &ResponseTab::GRAPHQL,
            (true, true) => &ResponseTab::GRAPHQL_WITH_SNAPSHOT,
            (false, false) => &ResponseTab::ALL,
            (false, true) => &ResponseTab::WITH_SNAPSHOT,
        }
    }

    /// The tree on screen: the filter's matches, else the whole body.
    pub fn shown_tree(&self) -> Option<&TreeView> {
        if self.raw {
            return None;
        }
        self.filtered
            .as_ref()
            .map(|f| &f.view)
            .or(self.tree.as_ref())
    }

    fn shown_tree_mut(&mut self) -> Option<&mut TreeView> {
        if self.raw {
            return None;
        }
        match &mut self.filtered {
            Some(filtered) => Some(&mut filtered.view),
            None => self.tree.as_mut(),
        }
    }

    /// `]` / `[`: the next or previous viewer tab.
    pub fn step_tab(&mut self, forward: bool) {
        let tabs = self.tabs();
        let n = tabs.len();
        let i = tabs.iter().position(|t| *t == self.tab).unwrap_or(0);
        let next = if forward { i + 1 } else { i + n - 1 } % n;
        self.set_tab(tabs[next]);
    }

    pub fn set_tab(&mut self, tab: ResponseTab) {
        if self.tab != tab {
            self.tab = tab;
            self.scroll = 0;
        }
    }

    /// Whether the JSON tree has the keys (Body tab, tree shown).
    pub fn tree_keys(&self) -> bool {
        self.tab == ResponseTab::Body && self.shown_tree().is_some()
    }

    /// Apply a tree action (fold, unfold) to the shown tree.
    pub fn with_tree(&mut self, act: impl FnOnce(&mut TreeView)) {
        if self.tab == ResponseTab::Body
            && let Some(tree) = self.shown_tree_mut()
        {
            act(tree);
        }
    }

    /// Move in the current view; `lines` is the length of a line-based
    /// view, `height` the rows on screen. A move past the end of a page of
    /// the paged view loads the next one.
    pub fn move_by(&mut self, how: Move, lines: usize, height: usize) -> Paging {
        let height = height.max(1);
        if self.tab == ResponseTab::Body
            && let Some(tree) = self.shown_tree_mut()
        {
            let delta = step(how, tree.cursor, tree.rows(), height);
            tree.move_by(delta);
            tree.follow(height);
            return Paging::Stay;
        }
        let max = lines.saturating_sub(height);
        let delta = step(how, self.scroll, max + 1, height);
        let next = self.scroll.saturating_add_signed(delta).min(max);
        let paging = self.page_turn(how, next == self.scroll);
        self.scroll = next;
        paging
    }

    /// The paged view turns the page when a move cannot scroll further.
    fn page_turn(&mut self, how: Move, stuck: bool) -> Paging {
        let Some(page) = &mut self.page else {
            return Paging::Stay;
        };
        if self.tab != ResponseTab::Body || !stuck || page.loading {
            return Paging::Stay;
        }
        let forward = matches!(how, Move::Down | Move::PageDown | Move::Bottom);
        let target = if forward {
            page.number + 1
        } else {
            page.number.saturating_sub(1)
        };
        if target == page.number {
            return Paging::Stay;
        }
        page.loading = true;
        Paging::Load(target)
    }

    /// A page of the paged view was read (`forward`: start at its top).
    pub fn page_loaded(&mut self, number: u64, lines: Vec<String>, last_page: u64) {
        let Some(page) = &mut self.page else { return };
        let back = number < page.number;
        if lines.is_empty() && number > last_page {
            page.loading = false;
            return;
        }
        *page = Page {
            number,
            lines,
            loading: false,
        };
        self.scroll = if back { usize::MAX } else { 0 };
    }

    /// `r`: switch between the tree and the pretty-printed text.
    pub fn toggle_raw(&mut self) {
        if self.tree.is_some() {
            self.raw = !self.raw;
            self.scroll = 0;
        }
    }

    /// `f` or `/`: open the filter or search box (on the Body tab's tree).
    pub fn open_box(&mut self, kind: BoxKind) -> bool {
        if self.tree.is_none() {
            return false;
        }
        self.tab = ResponseTab::Body;
        self.raw = false;
        let text = match kind {
            BoxKind::Filter => self.filtered.as_ref().map(|f| f.expr.clone()),
            BoxKind::Search => self.search.as_ref().map(|s| s.query.clone()),
        };
        self.input = Some(InputBox {
            kind,
            text: text.unwrap_or_default(),
            error: None,
        });
        true
    }

    /// A key for the open box; `None` when no box is open.
    pub fn box_key(&mut self, chord: Chord) -> Option<Typed> {
        let input = self.input.as_mut()?;
        match chord.code {
            Code::Esc => {
                if input.kind == BoxKind::Filter {
                    self.filtered = None;
                } else {
                    self.clear_search();
                }
                self.input = None;
            }
            Code::Enter => return Some(self.submit()),
            Code::Backspace => {
                input.text.pop();
                input.error = None;
                return Some(self.after_typing());
            }
            _ => {
                let c = chord.typed_char()?;
                input.text.push(c);
                input.error = None;
                return Some(self.after_typing());
            }
        }
        Some(Typed::Handled)
    }

    fn after_typing(&mut self) -> Typed {
        if let Some(InputBox {
            kind: BoxKind::Search,
            text,
            ..
        }) = &self.input
        {
            let query = text.clone();
            if let Some(job) = self.run_search(&query) {
                return Typed::Search(job);
            }
        }
        Typed::Handled
    }

    /// Drop the search and cancel its off-loop job, if any.
    fn clear_search(&mut self) {
        if let Some((_, cancel)) = self.search_job.take() {
            cancel.cancel();
        }
        self.search = None;
    }

    fn submit(&mut self) -> Typed {
        let Some(input) = self.input.take() else {
            return Typed::Handled;
        };
        if input.kind == BoxKind::Search {
            return Typed::Handled;
        }
        if input.text.trim().is_empty() {
            // The shown tree changes: earlier matches no longer apply.
            self.filtered = None;
            self.clear_search();
            return Typed::Handled;
        }
        match find::parse(&input.text) {
            Ok(_) => Typed::Filter(input.text),
            Err(error) => {
                // Scenario "Invalid filter": the error shows and the view stays.
                self.input = Some(InputBox {
                    error: Some(error),
                    ..input
                });
                Typed::Handled
            }
        }
    }

    /// The matches of filter `expr` arrived.
    pub fn filtered(&mut self, expr: String, result: Result<Arc<JsonTree>, String>) {
        match result {
            Ok(tree) => {
                self.filtered = Some(Filtered {
                    expr,
                    view: TreeView::new(tree),
                });
                self.clear_search();
            }
            Err(error) => {
                self.input = Some(InputBox {
                    kind: BoxKind::Filter,
                    text: expr,
                    error: Some(error),
                });
            }
        }
    }

    /// Search the shown tree for `query`: at once on a small tree, else as
    /// a job for the effect runner (the previous job is cancelled; its late
    /// result is dropped by [`Viewer::searched`]).
    fn run_search(&mut self, query: &str) -> Option<SearchJob> {
        let previous = self.search.take().filter(|p| !p.pending);
        self.clear_search();
        let tree = self.shown_tree_mut()?;
        let previous = previous.map(|p| (p.query, p.matches));
        if tree.tree.len() <= SYNC_SEARCH_NODES || query.is_empty() {
            let matches = match &previous {
                Some((q, m)) => find::refine(&tree.tree, q, m, query),
                None => find::search(&tree.tree, query),
            };
            self.show_matches(query, matches);
            return None;
        }
        let job = SearchJob {
            generation: SEARCH_GENERATION.fetch_add(1, Ordering::Relaxed),
            tree: Arc::clone(&tree.tree),
            query: query.to_owned(),
            previous,
            cancel: CancellationToken::new(),
        };
        self.search_job = Some((job.generation, job.cancel.clone()));
        self.search = Some(Search {
            query: query.to_owned(),
            matches: Vec::new(),
            pending: true,
        });
        Some(job)
    }

    /// The matches of off-loop search `generation` arrived (`None`: it was
    /// cancelled). Results of superseded searches are ignored.
    pub fn searched(&mut self, generation: u64, matches: Option<Vec<usize>>) {
        if self.search_job.as_ref().map(|(g, _)| *g) != Some(generation) {
            return;
        }
        self.search_job = None;
        let (Some(matches), Some(search)) = (matches, &self.search) else {
            return;
        };
        let query = search.query.clone();
        self.show_matches(&query, matches);
    }

    /// Show `matches` for `query` and jump to the first one after the
    /// selection.
    fn show_matches(&mut self, query: &str, matches: Vec<usize>) {
        if let Some(tree) = self.shown_tree_mut() {
            let from = tree.selected().map(|n| n.saturating_sub(1));
            if let Some(first) = find::step(&matches, from, true) {
                tree.reveal(first);
            }
        }
        self.search = Some(Search {
            query: query.to_owned(),
            matches,
            pending: false,
        });
    }

    /// `n` / `N`: the next or previous search match.
    pub fn search_step(&mut self, forward: bool) {
        let Some(matches) = self.search.as_ref().map(|s| s.matches.clone()) else {
            return;
        };
        self.with_tree(|tree| {
            if let Some(next) = find::step(&matches, tree.selected(), forward) {
                tree.reveal(next);
            }
        });
    }

    /// What `y` copies: the selected node's value.
    pub fn copy_text(&self) -> Option<String> {
        if self.tab != ResponseTab::Body {
            return None;
        }
        let tree = self.shown_tree()?;
        tree.selected().map(|node| tree.tree.copy_text(node))
    }
}

/// The signed step for `how` from `at`, in a list of `len` rows.
fn step(how: Move, at: usize, len: usize, page: usize) -> isize {
    let signed = |n: usize| isize::try_from(n).unwrap_or(isize::MAX);
    match how {
        Move::Up => -1,
        Move::Down => 1,
        Move::Top => -signed(at),
        Move::Bottom => signed(len.saturating_sub(1)) - signed(at),
        Move::PageUp => -signed(page),
        Move::PageDown => signed(page),
    }
}

#[cfg(test)]
mod tests {
    use super::received::fixtures::received;
    use super::*;
    use crate::keymap::chord::parse_chord;

    fn json_viewer() -> Viewer {
        let body = br#"{"user":{"id":42},"items":[{"id":1},{"id":2},{"id":3}]}"#;
        Viewer::new(&received(200, "application/json", body))
    }

    fn type_text(viewer: &mut Viewer, text: &str) {
        for c in text.chars() {
            viewer.box_key(parse_chord(&c.to_string()).unwrap());
        }
    }

    fn enter(viewer: &mut Viewer) -> Option<Typed> {
        viewer.box_key(parse_chord("Enter").unwrap())
    }

    #[test]
    fn tabs_step_and_wrap() {
        let mut viewer = json_viewer();
        viewer.step_tab(false);
        assert_eq!(viewer.tab, ResponseTab::Assertions);
        viewer.step_tab(true);
        assert_eq!(viewer.tab, ResponseTab::Body);
    }

    #[test]
    fn a_valid_filter_is_handed_to_the_runner_and_an_invalid_one_stays() {
        let mut viewer = json_viewer();
        assert!(viewer.open_box(BoxKind::Filter));
        type_text(&mut viewer, "$.items[");
        assert_eq!(enter(&mut viewer), Some(Typed::Handled));
        let input = viewer.input.as_ref().unwrap();
        assert!(input.error.is_some(), "the parse error is inline");
        assert!(viewer.filtered.is_none(), "the previous view stays");
        type_text(&mut viewer, "*].id");
        let Some(Typed::Filter(expr)) = enter(&mut viewer) else {
            panic!("expected a filter");
        };
        let path = find::parse(&expr).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(r#"{"user":{"id":42},"items":[{"id":1},{"id":2},{"id":3}]}"#)
                .unwrap();
        viewer.filtered(expr, Ok(Arc::new(find::select(&value, &path))));
        assert_eq!(viewer.shown_tree().unwrap().rows(), 3);
        // An empty filter clears it.
        viewer.open_box(BoxKind::Filter);
        viewer.input.as_mut().unwrap().text.clear();
        enter(&mut viewer);
        assert!(viewer.filtered.is_none());
    }

    #[test]
    fn search_jumps_to_matches_and_n_cycles() {
        let mut viewer = json_viewer();
        viewer.with_tree(TreeView::collapse_all);
        viewer.open_box(BoxKind::Search);
        type_text(&mut viewer, "id");
        let tree = viewer.shown_tree().unwrap();
        assert_eq!(tree.tree.path(tree.selected().unwrap()), "$.user.id");
        assert_eq!(enter(&mut viewer), Some(Typed::Handled));
        viewer.search_step(true);
        let tree = viewer.shown_tree().unwrap();
        assert_eq!(tree.tree.path(tree.selected().unwrap()), "$.items[0].id");
        viewer.search_step(false);
        viewer.search_step(false);
        let tree = viewer.shown_tree().unwrap();
        assert_eq!(tree.tree.path(tree.selected().unwrap()), "$.items[2].id");
        viewer.box_key(parse_chord("Esc").unwrap());
        viewer.open_box(BoxKind::Search);
        viewer.box_key(parse_chord("Esc").unwrap());
        assert!(viewer.search.is_none());
    }

    #[test]
    fn copy_takes_the_selected_value() {
        // Scenario "Copy a value": `$.user.id` is 42.
        let mut viewer = json_viewer();
        viewer.move_by(Move::Down, 0, 10);
        viewer.move_by(Move::Down, 0, 10);
        assert_eq!(viewer.copy_text().as_deref(), Some("42"));
        viewer.toggle_raw();
        assert_eq!(viewer.copy_text(), None);
    }

    #[test]
    fn line_views_scroll_within_their_length() {
        let mut viewer = Viewer::new(&received(200, "text/plain", b"a\nb\nc\n"));
        viewer.set_tab(ResponseTab::Headers);
        viewer.move_by(Move::Bottom, 30, 10);
        assert_eq!(viewer.scroll, 20);
        viewer.move_by(Move::PageUp, 30, 10);
        assert_eq!(viewer.scroll, 10);
        assert_eq!(viewer.move_by(Move::Top, 30, 10), Paging::Stay);
        assert_eq!(viewer.scroll, 0);
    }

    #[test]
    fn the_paged_view_turns_pages_at_its_ends() {
        let mut viewer = Viewer {
            page: Some(Page::default()),
            ..Viewer::default()
        };
        viewer.page_loaded(0, vec!["a".into(); 5], 3);
        assert_eq!(viewer.move_by(Move::Down, 5, 10), Paging::Load(1));
        assert_eq!(
            viewer.move_by(Move::Down, 5, 10),
            Paging::Stay,
            "loading already"
        );
        viewer.page_loaded(1, vec!["b".into(); 5], 3);
        assert_eq!(viewer.move_by(Move::Up, 5, 10), Paging::Load(0));
        viewer.page_loaded(0, vec!["a".into(); 30], 3);
        assert_eq!(viewer.scroll, usize::MAX, "going back starts at the end");
        // Past the last page, nothing changes.
        viewer.page_loaded(9, Vec::new(), 3);
        assert_eq!(viewer.page.as_ref().unwrap().number, 0);
    }
}
