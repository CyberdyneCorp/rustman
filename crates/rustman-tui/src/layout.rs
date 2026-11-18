//! The responsive shell layout (design D5, spec: Three-region layout and
//! Responsive collapse). One pure function computes every region, so drawing
//! and mouse hit-testing always agree.

use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};

use crate::focus::{Focus, Pane};
use crate::shell::ShellState;

pub const SIDEBAR_DEFAULT: u16 = 28;
pub const SIDEBAR_MIN: u16 = 20;
pub const SIDEBAR_MAX: u16 = 50;

/// Columns of the method field in the editor.
const METHOD_WIDTH: u16 = 9;

/// The layout regime for a terminal size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Breakpoint {
    /// ≥120 columns: sidebar | editor | response.
    Wide,
    /// 80–119 columns: editor over response; the sidebar is an overlay.
    Medium,
    /// <80 columns or <24 rows: one pane at a time.
    Narrow,
}

impl Breakpoint {
    pub fn of(width: u16, height: u16) -> Self {
        if width < 80 || height < 24 {
            Self::Narrow
        } else if width < 120 {
            Self::Medium
        } else {
            Self::Wide
        }
    }
}

/// Where everything goes on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Regions {
    pub breakpoint: Breakpoint,
    pub sidebar: Option<Rect>,
    /// The sidebar is drawn over the other panes.
    pub sidebar_overlay: bool,
    pub editor: Option<Rect>,
    pub response: Option<Rect>,
    /// The pane switcher bar (narrow only).
    pub switcher: Option<Rect>,
    /// The "below the recommended size" notice (narrow only).
    pub notice: Option<Rect>,
    pub status: Rect,
}

/// Lay out the shell in `area`.
pub fn regions(area: Rect, shell: &ShellState) -> Regions {
    let breakpoint = Breakpoint::of(area.width, area.height);
    let [main, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let mut regions = Regions {
        breakpoint,
        sidebar: None,
        sidebar_overlay: false,
        editor: None,
        response: None,
        switcher: None,
        notice: None,
        status,
    };
    match breakpoint {
        Breakpoint::Wide => wide(&mut regions, main, shell),
        Breakpoint::Medium => medium(&mut regions, main, shell),
        Breakpoint::Narrow => narrow(&mut regions, main, shell),
    }
    regions
}

fn wide(regions: &mut Regions, main: Rect, shell: &ShellState) {
    let sidebar = if shell.sidebar_hidden {
        0
    } else {
        shell.sidebar_width.min(main.width)
    };
    let [side, rest] =
        Layout::horizontal([Constraint::Length(sidebar), Constraint::Min(0)]).areas(main);
    let [editor, response] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(rest);
    regions.sidebar = (sidebar > 0).then_some(side);
    regions.editor = Some(editor);
    regions.response = Some(response);
}

fn medium(regions: &mut Regions, main: Rect, shell: &ShellState) {
    let [editor, response] =
        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(main);
    regions.editor = Some(editor);
    regions.response = Some(response);
    if shell.overlay_open {
        let width = shell.sidebar_width.min(main.width);
        regions.sidebar = Some(Rect { width, ..main });
        regions.sidebar_overlay = true;
    }
}

fn narrow(regions: &mut Regions, main: Rect, shell: &ShellState) {
    let [switcher, pane, notice] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(main);
    regions.switcher = Some(switcher);
    regions.notice = Some(notice);
    match shell.focus.pane() {
        Pane::Sidebar => regions.sidebar = Some(pane),
        Pane::Editor => regions.editor = Some(pane),
        Pane::Response => regions.response = Some(pane),
    }
}

impl Regions {
    pub fn pane(&self, pane: Pane) -> Option<Rect> {
        match pane {
            Pane::Sidebar => self.sidebar,
            Pane::Editor => self.editor,
            Pane::Response => self.response,
        }
    }

    /// The pane under a cell; the sidebar wins because it may overlay.
    pub fn pane_at(&self, column: u16, row: u16) -> Option<Pane> {
        let at = Position::new(column, row);
        Pane::ALL
            .into_iter()
            .find(|&p| self.pane(p).is_some_and(|r| r.contains(at)))
    }

    /// The column of the draggable sidebar border (wide layout only).
    pub fn sidebar_border(&self) -> Option<u16> {
        let sidebar = self
            .sidebar
            .filter(|_| self.breakpoint == Breakpoint::Wide)?;
        Some(sidebar.right().saturating_sub(1))
    }
}

/// The labels of the pane switcher bar and where each one is.
pub fn switcher_tabs(bar: Rect) -> Vec<(Pane, Rect)> {
    let mut x = bar.x;
    let mut tabs = Vec::new();
    for pane in Pane::ALL {
        let width = u16::try_from(pane.label().len() + 2).unwrap_or(u16::MAX);
        let rect = Rect::new(x, bar.y, width, 1).intersection(bar);
        tabs.push((pane, rect));
        x = x.saturating_add(width + 1);
    }
    tabs
}

/// Where each of `labels` sits when drawn left to right from `area`'s
/// start as ` label ` (one space either side), clipped to `area`.
pub fn label_rects(area: Rect, labels: &[String]) -> Vec<Rect> {
    let mut x = area.x;
    labels
        .iter()
        .map(|label| {
            let width = u16::try_from(label.chars().count() + 2).unwrap_or(u16::MAX);
            let rect = Rect::new(x, area.y, width, 1).intersection(area);
            x = x.saturating_add(width);
            rect
        })
        .collect()
}

/// The row of the request tab strip: the editor's top border, inside its
/// corners.
pub fn tab_strip(editor: Rect) -> Rect {
    Rect::new(
        editor.x.saturating_add(1),
        editor.y,
        editor.width.saturating_sub(2),
        1.min(editor.height),
    )
}

/// The focus targets inside the editor pane: method and URL on the first
/// row, the editor tab bar on the second, the content below.
pub fn editor_targets(editor: Rect) -> [(Focus, Rect); 4] {
    let inner = editor.inner(Margin::new(1, 1));
    let row = |offset: u16, height: u16| {
        Rect::new(inner.x, inner.y.saturating_add(offset), inner.width, height).intersection(inner)
    };
    let first = row(0, 1);
    let method_width = METHOD_WIDTH.min(first.width);
    let method = Rect {
        width: method_width,
        ..first
    };
    let url = Rect {
        x: first.x + method_width,
        width: first.width - method_width,
        ..first
    };
    [
        (Focus::Method, method),
        (Focus::Url, url),
        (Focus::EditorTabs, row(1, 1)),
        (Focus::EditorBody, row(2, inner.height.saturating_sub(2))),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(width: u16, height: u16, shell: &ShellState) -> Regions {
        regions(Rect::new(0, 0, width, height), shell)
    }

    #[test]
    fn breakpoints() {
        assert_eq!(Breakpoint::of(160, 45), Breakpoint::Wide);
        assert_eq!(Breakpoint::of(120, 24), Breakpoint::Wide);
        assert_eq!(Breakpoint::of(119, 40), Breakpoint::Medium);
        assert_eq!(Breakpoint::of(80, 24), Breakpoint::Medium);
        assert_eq!(Breakpoint::of(79, 40), Breakpoint::Narrow);
        assert_eq!(Breakpoint::of(200, 23), Breakpoint::Narrow);
    }

    #[test]
    fn wide_shows_three_panes_side_by_side() {
        let r = layout(160, 45, &ShellState::default());
        let (side, editor, response) = (r.sidebar.unwrap(), r.editor.unwrap(), r.response.unwrap());
        assert_eq!(side, Rect::new(0, 0, 28, 44));
        assert_eq!(editor.x, 28);
        assert_eq!(response.x, editor.right());
        assert_eq!(response.right(), 160);
        assert_eq!(r.status, Rect::new(0, 44, 160, 1));
        assert_eq!(r.sidebar_border(), Some(27));
        assert!(r.switcher.is_none() && r.notice.is_none());
    }

    #[test]
    fn medium_stacks_and_overlays_the_sidebar() {
        let mut shell = ShellState::default();
        let r = layout(80, 24, &shell);
        assert!(r.sidebar.is_none());
        assert_eq!(r.editor.unwrap().bottom(), r.response.unwrap().y);
        shell.overlay_open = true;
        let r = layout(80, 24, &shell);
        assert!(r.sidebar_overlay);
        assert_eq!(r.sidebar, Some(Rect::new(0, 0, 28, 23)));
        assert_eq!(r.pane_at(5, 5), Some(Pane::Sidebar));
        assert_eq!(r.pane_at(40, 5), Some(Pane::Editor));
        assert_eq!(r.sidebar_border(), None);
    }

    #[test]
    fn narrow_shows_the_focused_pane_only() {
        let shell = ShellState {
            focus: Focus::Response,
            ..ShellState::default()
        };
        let r = layout(60, 20, &shell);
        assert!(r.sidebar.is_none() && r.editor.is_none());
        assert_eq!(r.response, Some(Rect::new(0, 1, 60, 17)));
        assert_eq!(r.switcher, Some(Rect::new(0, 0, 60, 1)));
        assert_eq!(r.notice, Some(Rect::new(0, 18, 60, 1)));
    }

    #[test]
    fn switcher_tabs_sit_side_by_side() {
        let tabs = switcher_tabs(Rect::new(0, 0, 60, 1));
        assert_eq!(tabs[0], (Pane::Sidebar, Rect::new(0, 0, 9, 1)));
        assert_eq!(tabs[1], (Pane::Editor, Rect::new(10, 0, 9, 1)));
        assert_eq!(tabs[2], (Pane::Response, Rect::new(20, 0, 10, 1)));
    }

    #[test]
    fn labels_sit_side_by_side_and_are_clipped() {
        let labels = vec!["Params".to_owned(), "Headers".to_owned()];
        let rects = label_rects(Rect::new(2, 3, 14, 1), &labels);
        assert_eq!(rects, [Rect::new(2, 3, 8, 1), Rect::new(10, 3, 6, 1)]);
        assert_eq!(tab_strip(Rect::new(0, 0, 40, 10)), Rect::new(1, 0, 38, 1));
    }

    #[test]
    fn editor_targets_split_the_editor() {
        let [method, url, tabs, body] = editor_targets(Rect::new(0, 0, 40, 10));
        assert_eq!(method, (Focus::Method, Rect::new(1, 1, 9, 1)));
        assert_eq!(url, (Focus::Url, Rect::new(10, 1, 29, 1)));
        assert_eq!(tabs, (Focus::EditorTabs, Rect::new(1, 2, 38, 1)));
        assert_eq!(body, (Focus::EditorBody, Rect::new(1, 3, 38, 6)));
        for (_, rect) in editor_targets(Rect::new(0, 0, 3, 2)) {
            assert!(rect.area() <= 1);
        }
    }
}
