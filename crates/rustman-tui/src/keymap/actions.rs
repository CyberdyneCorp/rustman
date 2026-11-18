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
    PaletteOpen,
    HelpOpen,
    SidebarToggle,
    SidebarGrow,
    SidebarShrink,
    FocusNext,
    FocusPrev,
    PaneLeft,
    PaneRight,
    Escape,
    EnvSwitch,
    SecretsReveal,
    EditorExternal,
    VizOpen,
    NotificationsShow,
    ListUp,
    ListDown,
    ListTop,
    ListBottom,
    ListPageUp,
    ListPageDown,
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
pub const REGISTRY: [ActionDef; 27] = [
    def(ActionId::RequestSend, "request.send", "Send request"),
    def(ActionId::RequestCancel, "request.cancel", "Cancel request"),
    def(ActionId::RequestSave, "request.save", "Save request"),
    def(ActionId::TabNew, "tab.new", "New tab"),
    def(ActionId::TabClose, "tab.close", "Close tab"),
    def(ActionId::PaletteOpen, "palette.open", "Command palette"),
    def(ActionId::HelpOpen, "help.open", "Help"),
    def(ActionId::SidebarToggle, "sidebar.toggle", "Toggle sidebar"),
    def(ActionId::SidebarGrow, "sidebar.grow", "Widen sidebar"),
    def(ActionId::SidebarShrink, "sidebar.shrink", "Narrow sidebar"),
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
                    .all(|c| c.is_ascii_lowercase() || "._".contains(c))
            );
        }
    }
}
