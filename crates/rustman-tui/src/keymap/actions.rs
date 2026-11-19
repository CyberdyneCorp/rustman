//! The action registry: every action the keymap, the palette and the help
//! overlay can name, with its stable id (design D4).
//!
//! Ids are part of the user-facing `keybindings.toml` format: never rename
//! one; add a new action instead.

/// An action a key, the palette or the mouse can trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ActionId {
    RequestSend,
    RequestCancel,
    RequestSave,
    TabNew,
    TabClose,
    TabNext,
    TabPrev,
    TabGoto1,
    TabGoto2,
    TabGoto3,
    TabGoto4,
    TabGoto5,
    TabGoto6,
    TabGoto7,
    TabGoto8,
    TabGoto9,
    PaletteOpen,
    HelpOpen,
    SidebarToggle,
    SidebarGrow,
    SidebarShrink,
    SidebarOpen,
    SidebarFilter,
    SidebarNew,
    SidebarRename,
    SidebarDelete,
    TreeCollapse,
    TreeExpand,
    FocusNext,
    FocusPrev,
    PaneLeft,
    PaneRight,
    Escape,
    EnvSwitch,
    SecretsReveal,
    EditorExternal,
    EditorMode,
    EditorComplete,
    VizOpen,
    NotificationsShow,
    ListUp,
    ListDown,
    ListTop,
    ListBottom,
    ListPageUp,
    ListPageDown,
    ResponseNextTab,
    ResponsePrevTab,
    JsonToggle,
    JsonCollapse,
    JsonExpand,
    JsonCollapseAll,
    JsonExpandAll,
    JsonCopy,
    ResponseRaw,
    ResponseFilter,
    ResponseSearch,
    SearchNext,
    SearchPrev,
    ResponseDiff,
    ResponseSaveBody,
    ReportHtml,
    WebhookOpen,
    MockOpen,
    GraphqlRefreshSchema,
    GraphqlSubscriptionStart,
    GraphqlSubscriptionStop,
    CopyAs,
    ImportOpen,
    SnapshotReview,
    Quit,
}

/// One registry entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionDef {
    pub action: ActionId,
    /// The stable id used in `keybindings.toml`.
    pub id: &'static str,
    /// The name shown in the palette and the help overlay.
    pub title: &'static str,
}

const fn def(action: ActionId, id: &'static str, title: &'static str) -> ActionDef {
    ActionDef { action, id, title }
}

/// Every action, in the order the palette and the docs list them.
pub const REGISTRY: [ActionDef; 71] = [
    def(ActionId::RequestSend, "request.send", "Send request"),
    def(ActionId::RequestCancel, "request.cancel", "Cancel request"),
    def(ActionId::RequestSave, "request.save", "Save request"),
    def(ActionId::TabNew, "tab.new", "New tab"),
    def(ActionId::TabClose, "tab.close", "Close tab"),
    def(ActionId::TabNext, "tab.next", "Next tab"),
    def(ActionId::TabPrev, "tab.prev", "Previous tab"),
    def(ActionId::TabGoto1, "tab.goto_1", "Go to tab 1"),
    def(ActionId::TabGoto2, "tab.goto_2", "Go to tab 2"),
    def(ActionId::TabGoto3, "tab.goto_3", "Go to tab 3"),
    def(ActionId::TabGoto4, "tab.goto_4", "Go to tab 4"),
    def(ActionId::TabGoto5, "tab.goto_5", "Go to tab 5"),
    def(ActionId::TabGoto6, "tab.goto_6", "Go to tab 6"),
    def(ActionId::TabGoto7, "tab.goto_7", "Go to tab 7"),
    def(ActionId::TabGoto8, "tab.goto_8", "Go to tab 8"),
    def(ActionId::TabGoto9, "tab.goto_9", "Go to tab 9"),
    def(ActionId::PaletteOpen, "palette.open", "Command palette"),
    def(ActionId::HelpOpen, "help.open", "Help"),
    def(ActionId::SidebarToggle, "sidebar.toggle", "Toggle sidebar"),
    def(ActionId::SidebarGrow, "sidebar.grow", "Widen sidebar"),
    def(ActionId::SidebarShrink, "sidebar.shrink", "Narrow sidebar"),
    def(ActionId::SidebarOpen, "sidebar.open", "Open request"),
    def(ActionId::SidebarFilter, "sidebar.filter", "Filter requests"),
    def(ActionId::SidebarNew, "sidebar.new", "New request file"),
    def(
        ActionId::SidebarRename,
        "sidebar.rename",
        "Rename request or folder",
    ),
    def(
        ActionId::SidebarDelete,
        "sidebar.delete",
        "Delete request or folder",
    ),
    def(ActionId::TreeCollapse, "tree.collapse", "Collapse folder"),
    def(ActionId::TreeExpand, "tree.expand", "Expand folder"),
    def(ActionId::FocusNext, "focus.next", "Focus next"),
    def(ActionId::FocusPrev, "focus.prev", "Focus previous"),
    def(ActionId::PaneLeft, "pane.left", "Focus pane to the left"),
    def(ActionId::PaneRight, "pane.right", "Focus pane to the right"),
    def(ActionId::Escape, "ui.escape", "Close or dismiss"),
    def(ActionId::EnvSwitch, "env.switch", "Switch environment"),
    def(ActionId::SecretsReveal, "secrets.reveal", "Reveal secrets"),
    def(
        ActionId::EditorExternal,
        "editor.external",
        "Edit in external editor",
    ),
    def(
        ActionId::EditorMode,
        "editor.mode",
        "Change body mode or auth type",
    ),
    def(
        ActionId::EditorComplete,
        "editor.complete",
        "Show completions",
    ),
    def(ActionId::VizOpen, "viz.open", "Open in viz companion"),
    def(
        ActionId::NotificationsShow,
        "notifications.show",
        "Show notification log",
    ),
    def(ActionId::ListUp, "list.up", "Move up"),
    def(ActionId::ListDown, "list.down", "Move down"),
    def(ActionId::ListTop, "list.top", "Go to top"),
    def(ActionId::ListBottom, "list.bottom", "Go to bottom"),
    def(ActionId::ListPageUp, "list.page_up", "Page up"),
    def(ActionId::ListPageDown, "list.page_down", "Page down"),
    def(
        ActionId::ResponseNextTab,
        "response.next_tab",
        "Next response tab",
    ),
    def(
        ActionId::ResponsePrevTab,
        "response.prev_tab",
        "Previous response tab",
    ),
    def(ActionId::JsonToggle, "json.toggle", "Fold or unfold node"),
    def(ActionId::JsonCollapse, "json.collapse", "Fold node"),
    def(ActionId::JsonExpand, "json.expand", "Unfold node"),
    def(ActionId::JsonCollapseAll, "json.collapse_all", "Fold all"),
    def(ActionId::JsonExpandAll, "json.expand_all", "Unfold all"),
    def(ActionId::JsonCopy, "json.copy", "Copy value"),
    def(ActionId::ResponseRaw, "response.raw", "Toggle raw body"),
    def(
        ActionId::ResponseFilter,
        "response.filter",
        "Filter body (JSONPath)",
    ),
    def(ActionId::ResponseSearch, "response.search", "Search body"),
    def(ActionId::SearchNext, "search.next", "Next match"),
    def(ActionId::SearchPrev, "search.prev", "Previous match"),
    def(
        ActionId::ResponseDiff,
        "response.diff",
        "Diff with an earlier exchange",
    ),
    def(
        ActionId::ResponseSaveBody,
        "response.save_body",
        "Save body…",
    ),
    def(ActionId::ReportHtml, "report.html", "Export HTML report"),
    def(ActionId::WebhookOpen, "webhook.open", "Webhook inspector"),
    def(ActionId::MockOpen, "mock.open", "Mock server"),
    def(
        ActionId::GraphqlRefreshSchema,
        "graphql.refresh_schema",
        "Refresh GraphQL schema",
    ),
    def(
        ActionId::GraphqlSubscriptionStart,
        "graphql.subscription_start",
        "Start GraphQL subscription",
    ),
    def(
        ActionId::GraphqlSubscriptionStop,
        "graphql.subscription_stop",
        "Stop GraphQL subscription",
    ),
    def(ActionId::CopyAs, "request.copy_as", "Copy as…"),
    def(ActionId::ImportOpen, "import.open", "Import…"),
    def(
        ActionId::SnapshotReview,
        "snapshot.review",
        "Review snapshots",
    ),
    def(ActionId::Quit, "app.quit", "Quit"),
];

impl ActionId {
    fn def(self) -> &'static ActionDef {
        REGISTRY
            .iter()
            .find(|d| d.action == self)
            .expect("every action is registered")
    }

    /// The stable id, e.g. `request.send`.
    pub fn id(self) -> &'static str {
        self.def().id
    }

    pub fn title(self) -> &'static str {
        self.def().title
    }

    /// The action with this stable id.
    pub fn from_id(id: &str) -> Option<Self> {
        REGISTRY.iter().find(|d| d.id == id).map(|d| d.action)
    }

    /// The 0-based tab index of the `tab.goto_N` actions.
    pub fn tab_index(self) -> Option<usize> {
        const GOTO: [ActionId; 9] = [
            ActionId::TabGoto1,
            ActionId::TabGoto2,
            ActionId::TabGoto3,
            ActionId::TabGoto4,
            ActionId::TabGoto5,
            ActionId::TabGoto6,
            ActionId::TabGoto7,
            ActionId::TabGoto8,
            ActionId::TabGoto9,
        ];
        GOTO.iter().position(|a| *a == self)
    }

    /// Every action, in registry order.
    pub fn all() -> impl Iterator<Item = Self> {
        REGISTRY.iter().map(|d| d.action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_and_titles_are_unique_and_round_trip() {
        let ids: HashSet<_> = REGISTRY.iter().map(|d| d.id).collect();
        let titles: HashSet<_> = REGISTRY.iter().map(|d| d.title).collect();
        let actions: HashSet<_> = ActionId::all().collect();
        assert_eq!(ids.len(), REGISTRY.len());
        assert_eq!(titles.len(), REGISTRY.len());
        assert_eq!(actions.len(), REGISTRY.len());
        for action in ActionId::all() {
            assert_eq!(ActionId::from_id(action.id()), Some(action));
        }
        assert_eq!(ActionId::from_id("request.sned"), None);
        assert_eq!(ActionId::TabGoto3.tab_index(), Some(2));
        assert_eq!(ActionId::TabNext.tab_index(), None);
    }

    #[test]
    fn ids_are_stable() {
        // Ids are a user-facing format; changing one breaks keybindings.toml.
        assert_eq!(ActionId::RequestSend.id(), "request.send");
        assert_eq!(ActionId::PaletteOpen.id(), "palette.open");
        assert_eq!(ActionId::EnvSwitch.title(), "Switch environment");
        for def in REGISTRY {
            let (group, name) = def.id.split_once('.').expect("group.name");
            assert!(!group.is_empty() && !name.is_empty(), "{}", def.id);
            assert!(
                def.id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._".contains(c))
            );
        }
    }
}
