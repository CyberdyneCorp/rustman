//! The focus ring (spec: Default keymap, scenario "Focus cycling").

/// The three panes of the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pane {
    Sidebar,
    Editor,
    Response,
}

impl Pane {
    pub const ALL: [Self; 3] = [Self::Sidebar, Self::Editor, Self::Response];

    /// The name on the pane switcher bar.
    pub fn label(self) -> &'static str {
        match self {
            Self::Sidebar => "Sidebar",
            Self::Editor => "Request",
            Self::Response => "Response",
        }
    }

    /// Where focus lands when the pane is clicked or switched to.
    pub fn entry(self) -> Focus {
        match self {
            Self::Sidebar => Focus::Sidebar,
            Self::Editor => Focus::EditorBody,
            Self::Response => Focus::Response,
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Sidebar => 0,
            Self::Editor => 1,
            Self::Response => 2,
        }
    }
}

/// Everything that can hold the keyboard focus, in ring order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Focus {
    Sidebar,
    Method,
    Url,
    EditorTabs,
    EditorBody,
    Response,
}

impl Focus {
    pub const RING: [Self; 6] = [
        Self::Sidebar,
        Self::Method,
        Self::Url,
        Self::EditorTabs,
        Self::EditorBody,
        Self::Response,
    ];

    pub fn pane(self) -> Pane {
        match self {
            Self::Sidebar => Pane::Sidebar,
            Self::Response => Pane::Response,
            _ => Pane::Editor,
        }
    }

    /// The next stop for `Tab`; the sidebar is skipped while it is hidden.
    pub fn next(self, sidebar_shown: bool) -> Self {
        self.step(1, sidebar_shown)
    }

    /// The next stop for `Shift+Tab`.
    pub fn prev(self, sidebar_shown: bool) -> Self {
        self.step(Self::RING.len() - 1, sidebar_shown)
    }

    fn step(self, by: usize, sidebar_shown: bool) -> Self {
        let len = Self::RING.len();
        let mut i = Self::RING.iter().position(|&f| f == self).unwrap_or(0);
        loop {
            i = (i + by) % len;
            let next = Self::RING[i];
            if sidebar_shown || next != Self::Sidebar {
                return next;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_walks_the_whole_ring_and_back_to_the_sidebar() {
        let mut focus = Focus::Sidebar;
        let mut seen = vec![focus];
        for _ in 0..6 {
            focus = focus.next(true);
            seen.push(focus);
        }
        assert_eq!(&seen[..6], &Focus::RING);
        assert_eq!(seen[6], Focus::Sidebar);
    }

    #[test]
    fn shift_tab_goes_backwards() {
        assert_eq!(Focus::Sidebar.prev(true), Focus::Response);
        assert_eq!(Focus::Url.prev(true), Focus::Method);
    }

    #[test]
    fn a_hidden_sidebar_is_skipped() {
        assert_eq!(Focus::Response.next(false), Focus::Method);
        assert_eq!(Focus::Method.prev(false), Focus::Response);
    }

    #[test]
    fn panes_group_the_focus_targets() {
        assert_eq!(Focus::Url.pane(), Pane::Editor);
        assert_eq!(Pane::Editor.entry(), Focus::EditorBody);
        for pane in Pane::ALL {
            assert_eq!(pane.entry().pane(), pane);
            assert_eq!(Pane::ALL[pane.index()], pane);
        }
    }
}
