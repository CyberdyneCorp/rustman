//! Snapshot review (snapshot-testing "TUI snapshot review"): the response
//! viewer's Snapshot tab shows each failed, missing or new snapshot of an
//! exchange with its diff; `a` approves (writes the received snapshot) and
//! `r` rejects (keeps the stored one). `Alt+A` opens a queue that steps
//! through every open failure across the tabs. Exchanges sent in CI mode
//! cannot be approved.
//!
//! Writing is an effect: [`ApproveJob`] runs on the effect runner and comes
//! back as [`Approved`].

use ratatui::Frame;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use rustman_core::snapshot::{SnapshotMode, SnapshotReview, SnapshotState, is_ci_value};

use crate::app::{Action, Effect, Overlay, State};
use crate::config::UiOptions;
use crate::focus::Focus;
use crate::keymap::chord::{Chord, Code};
use crate::response::ResponseTab;
use crate::tabs::TabId;
use crate::theme::Token;
use crate::toast::Severity;
use crate::view::{dialog, hint};

/// The message shown when approving is attempted in CI mode.
pub const CI_DISABLED: &str = "Snapshot approval is disabled in CI mode";

/// The snapshot mode of the TUI's sends: CI mode when `CI` says so.
pub fn mode_from_env(ci: Option<&str>) -> SnapshotMode {
    if is_ci_value(ci) {
        SnapshotMode::Ci
    } else {
        SnapshotMode::Record
    }
}

/// What the user decided about one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Open,
    /// The write is in progress.
    Approving,
    Approved,
    Rejected,
    /// The write failed.
    Failed(String),
}

impl Decision {
    fn label(&self) -> Option<&str> {
        match self {
            Self::Open => None,
            Self::Approving => Some("approving…"),
            Self::Approved => Some("approved"),
            Self::Rejected => Some("rejected"),
            Self::Failed(message) => Some(message),
        }
    }
}

/// One snapshot of an exchange and its decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewItem {
    pub review: SnapshotReview,
    pub decision: Decision,
}

impl ReviewItem {
    /// Still waiting for `a` or `r`.
    pub fn is_open(&self) -> bool {
        self.decision == Decision::Open && self.review.approvable()
    }
}

/// The snapshots of one exchange to review.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reviews {
    pub items: Vec<ReviewItem>,
    /// The exchange was sent in CI mode: approving is disabled.
    pub ci: bool,
}

impl Reviews {
    pub fn new(reviews: Vec<SnapshotReview>, mode: SnapshotMode) -> Self {
        let items = reviews
            .into_iter()
            .map(|review| ReviewItem {
                review,
                decision: Decision::Open,
            })
            .collect();
        Self {
            items,
            ci: mode == SnapshotMode::Ci,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The first snapshot still waiting for a decision.
    fn first_open(&self) -> Option<usize> {
        self.items.iter().position(ReviewItem::is_open)
    }
}

/// Write an approved snapshot (run by the effect runner).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApproveJob {
    pub tab: TabId,
    pub index: usize,
    pub review: SnapshotReview,
}

/// An approve write finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approved {
    pub tab: TabId,
    pub index: usize,
    pub result: Result<(), String>,
}

/// Run `job`: write the received snapshot, atomically and under its lock.
pub fn execute(job: &ApproveJob) -> Action {
    let result = job
        .review
        .approve(SnapshotMode::Record)
        .map(|_| ())
        .map_err(|e| e.to_string());
    Action::SnapshotApproved(Box::new(Approved {
        tab: job.tab,
        index: job.index,
        result,
    }))
}

/// The queue opened with `Alt+A`: every open snapshot failure, by tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queue {
    pub entries: Vec<(TabId, usize)>,
    pub at: usize,
}

/// `a` / `r` on the Snapshot tab of the focused response.
pub fn key(state: &mut State, chord: Chord) -> Option<Vec<Effect>> {
    if state.shell.focus != Focus::Response || chord.ctrl || chord.alt {
        return None;
    }
    let approve = match chord.code {
        Code::Char('a') => true,
        Code::Char('r') => false,
        _ => return None,
    };
    let tab = state.tabs.active().filter(|t| t.in_flight.is_none())?;
    if tab.viewer.tab != ResponseTab::Snapshot {
        return None;
    }
    let id = tab.id;
    let Some(index) = tab.exchange.as_ref()?.snapshots.first_open() else {
        state.notify(Severity::Info, "No snapshot is waiting for review");
        return Some(Vec::new());
    };
    Some(decide(state, id, index, approve))
}

/// Approve or reject snapshot `index` of tab `tab`.
fn decide(state: &mut State, tab: TabId, index: usize, approve: bool) -> Vec<Effect> {
    let Some(reviews) = reviews_mut(state, tab) else {
        return Vec::new();
    };
    if approve && reviews.ci {
        state.notify(Severity::Warn, CI_DISABLED);
        return Vec::new();
    }
    let Some(item) = reviews.items.get_mut(index).filter(|i| i.is_open()) else {
        return Vec::new();
    };
    if !approve {
        item.decision = Decision::Rejected;
        let message = format!("Kept {}", item.review.display_path);
        state.notify(Severity::Info, message);
        return Vec::new();
    }
    item.decision = Decision::Approving;
    let job = ApproveJob {
        tab,
        index,
        review: item.review.clone(),
    };
    vec![Effect::ApproveSnapshot(Box::new(job))]
}

fn reviews_mut(state: &mut State, tab: TabId) -> Option<&mut Reviews> {
    let tab = state.tabs.get_mut(tab)?;
    tab.exchange.as_mut().map(|r| &mut r.snapshots)
}

/// An approve write finished: show `approved`, or why it failed.
pub fn on_approved(state: &mut State, done: Approved) {
    let Some(item) = reviews_mut(state, done.tab).and_then(|r| r.items.get_mut(done.index)) else {
        return;
    };
    let path = item.review.display_path.clone();
    match done.result {
        Ok(()) => {
            item.decision = Decision::Approved;
            state.notify(Severity::Success, format!("Approved {path}"));
        }
        Err(message) => {
            item.decision = Decision::Failed(message.clone());
            state.notify(Severity::Error, message);
        }
    }
}

/// `Alt+A`: open the review queue over every open snapshot failure.
pub fn open_queue(state: &mut State) {
    let entries: Vec<(TabId, usize)> = state
        .tabs
        .list
        .iter()
        .filter_map(|tab| Some((tab.id, &tab.exchange.as_ref()?.snapshots)))
        .flat_map(|(id, reviews)| {
            let open = reviews
                .items
                .iter()
                .enumerate()
                .filter(|(_, i)| i.is_open());
            open.map(move |(index, _)| (id, index))
        })
        .collect();
    if entries.is_empty() {
        state.notify(Severity::Info, "No failing snapshots to review");
        return;
    }
    state.overlay = Some(Overlay::SnapshotQueue(Box::new(Queue { entries, at: 0 })));
}

/// A key while the review queue is open.
pub fn queue_key(state: &mut State, chord: Chord) -> Vec<Effect> {
    let Some(Overlay::SnapshotQueue(queue)) = &mut state.overlay else {
        return Vec::new();
    };
    let last = queue.entries.len() - 1;
    let (tab, index) = queue.entries[queue.at];
    let approve = match chord.code {
        Code::Esc | Code::Char('q') => {
            state.overlay = None;
            return Vec::new();
        }
        Code::Right | Code::Down | Code::Char('n' | 'j') => {
            queue.at = (queue.at + 1).min(last);
            return Vec::new();
        }
        Code::Left | Code::Up | Code::Char('p' | 'k') => {
            queue.at = queue.at.saturating_sub(1);
            return Vec::new();
        }
        Code::Char('a') => true,
        Code::Char('r') => false,
        _ => return Vec::new(),
    };
    let effects = decide(state, tab, index, approve);
    advance(state);
    effects
}

/// Step to the next entry still open; close the queue when none is left.
fn advance(state: &mut State) {
    let open: Vec<bool> = match &state.overlay {
        Some(Overlay::SnapshotQueue(queue)) => queue
            .entries
            .iter()
            .map(|&(tab, index)| item(state, tab, index).is_some_and(ReviewItem::is_open))
            .collect(),
        _ => return,
    };
    let Some(Overlay::SnapshotQueue(queue)) = &mut state.overlay else {
        return;
    };
    let after = (queue.at + 1..open.len()).chain(0..=queue.at);
    match after.into_iter().find(|&i| open[i]) {
        Some(next) => queue.at = next,
        None => {
            let reviewed = queue.entries.len();
            state.overlay = None;
            let noun = if reviewed == 1 {
                "snapshot"
            } else {
                "snapshots"
            };
            state.notify(Severity::Info, format!("Reviewed {reviewed} {noun}"));
        }
    }
}

fn item(state: &State, tab: TabId, index: usize) -> Option<&ReviewItem> {
    let tab = state.tabs.get(tab)?;
    tab.exchange.as_ref()?.snapshots.items.get(index)
}

/// The review queue dialog: the current entry's diff and the keys.
pub fn render_queue(frame: &mut Frame, state: &State, queue: &Queue) {
    let ui = &state.ui;
    let (tab, index) = queue.entries[queue.at];
    let title = format!(
        "Snapshot review {} of {}",
        queue.at + 1,
        queue.entries.len()
    );
    let mut lines = match item(state, tab, index) {
        Some(item) => item_lines(item, ui),
        None => vec![hint("This exchange is gone.", ui)],
    };
    let room = usize::from(frame.area().height.saturating_sub(6)).max(1);
    lines.truncate(room);
    lines.extend([
        Line::default(),
        hint("a approve · r reject · n/p next/previous · Esc close", ui),
    ]);
    dialog(frame, frame.area(), &title, lines, ui);
}

/// The Snapshot tab of an exchange.
pub fn lines(reviews: &Reviews, ui: &UiOptions) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for item in &reviews.items {
        lines.extend(item_lines(item, ui));
        lines.push(Line::default());
    }
    let keys = if reviews.ci {
        "Approval is disabled in CI mode."
    } else {
        "a approve · r reject · Alt+A review all"
    };
    lines.push(hint(keys, ui));
    lines
}

fn item_lines(item: &ReviewItem, ui: &UiOptions) -> Vec<Line<'static>> {
    let review = &item.review;
    let token = match review.state {
        SnapshotState::Recorded => Token::Ok,
        SnapshotState::Missing => Token::Warn,
        _ => Token::Error,
    };
    let mut head = vec![
        Span::styled(
            format!("snapshot {} {}", review.name, review.state.as_str()),
            ui.theme.fg(token).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} {}", ui.glyphs.dot, review.display_path),
            ui.theme.fg(Token::Muted),
        ),
    ];
    if let Some(label) = item.decision.label() {
        head.push(Span::styled(
            format!(" {} {label}", ui.glyphs.dot),
            ui.theme
                .fg(decision_token(&item.decision))
                .add_modifier(Modifier::BOLD),
        ));
    }
    let mut lines = vec![Line::from(head)];
    lines.extend(detail_lines(review, ui));
    lines
}

fn decision_token(decision: &Decision) -> Token {
    match decision {
        Decision::Approved => Token::Ok,
        Decision::Failed(_) => Token::Error,
        _ => Token::Info,
    }
}

/// The diff of a failed snapshot, or the received one of a new snapshot.
fn detail_lines(review: &SnapshotReview, ui: &UiOptions) -> Vec<Line<'static>> {
    match &review.diff {
        Some(diff) if !diff.is_empty() => diff
            .to_string()
            .lines()
            .map(|line| Line::styled(line.to_owned(), ui.theme.fg(diff_token(line))))
            .collect(),
        _ if review.state == SnapshotState::Failed => {
            vec![hint(&review.message, ui)]
        }
        _ => {
            let mut lines = Vec::new();
            if !review.message.is_empty() {
                lines.push(hint(&review.message, ui));
            }
            lines.extend(
                review
                    .received
                    .lines()
                    .map(|line| Line::styled(format!("+ {line}"), ui.theme.fg(Token::Ok))),
            );
            lines
        }
    }
}

/// The colour of one diff line: additions, removals and changes.
fn diff_token(line: &str) -> Token {
    if line.starts_with("added") || (line.starts_with('+') && !line.starts_with("+++")) {
        Token::Ok
    } else if line.starts_with("removed") || (line.starts_with('-') && !line.starts_with("---")) {
        Token::Error
    } else if line.starts_with("@@") || line.starts_with("+++") || line.starts_with("---") {
        Token::Muted
    } else if line.starts_with(' ') {
        Token::Text
    } else {
        Token::Warn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mode_follows_the_ci_variable() {
        assert_eq!(mode_from_env(None), SnapshotMode::Record);
        assert_eq!(mode_from_env(Some("false")), SnapshotMode::Record);
        assert_eq!(mode_from_env(Some("true")), SnapshotMode::Ci);
        assert_eq!(mode_from_env(Some("1")), SnapshotMode::Ci);
    }

    #[test]
    fn diff_lines_are_coloured_by_kind() {
        assert_eq!(diff_token("added $.a: 1"), Token::Ok);
        assert_eq!(diff_token("removed $.a: 1"), Token::Error);
        assert_eq!(diff_token("changed $.a: 1 → 2"), Token::Warn);
        assert_eq!(diff_token("event count changed: 3 → 2"), Token::Warn);
        assert_eq!(diff_token("@@ -1 +1 @@"), Token::Muted);
        assert_eq!(diff_token("--- old"), Token::Muted);
        assert_eq!(diff_token("-a"), Token::Error);
        assert_eq!(diff_token("+b"), Token::Ok);
        assert_eq!(diff_token(" same"), Token::Text);
    }

    #[test]
    fn decisions_have_labels() {
        assert_eq!(Decision::Open.label(), None);
        assert_eq!(Decision::Approved.label(), Some("approved"));
        assert_eq!(Decision::Rejected.label(), Some("rejected"));
        assert_eq!(Decision::Failed("x".into()).label(), Some("x"));
        assert_eq!(decision_token(&Decision::Approving), Token::Info);
    }
}
