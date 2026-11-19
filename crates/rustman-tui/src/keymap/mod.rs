//! Keymaps as data (design D4): layered bindings from contexts and key
//! sequences to action ids.
//!
//! Layers apply in order — built-in defaults, then the optional `vim`
//! preset, then the user's `keybindings.toml` — and a later layer replaces an
//! earlier one's binding for the same context and keys. Lookups try the
//! focused context first, then `global`.

pub mod actions;
pub mod chord;
pub mod docs;
mod user;

use std::collections::BTreeMap;
use std::fmt;

pub use actions::ActionId;
pub use chord::{Chord, Keys};
pub use user::{FILE_NAME, UserLayer};

use crate::focus::Focus;

/// Where a binding applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Context {
    Global,
    Sidebar,
    Editor,
    Response,
}

impl Context {
    pub const ALL: [Self; 4] = [Self::Global, Self::Sidebar, Self::Editor, Self::Response];

    /// The table name in `keybindings.toml`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Sidebar => "sidebar",
            Self::Editor => "editor",
            Self::Response => "response",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }

    /// The context of the focused element.
    pub fn of(focus: Focus) -> Self {
        match focus {
            Focus::Sidebar => Self::Sidebar,
            Focus::Response => Self::Response,
            Focus::Method | Focus::Url | Focus::EditorTabs | Focus::EditorBody => Self::Editor,
        }
    }

    /// The heading in the help overlay and the docs.
    pub fn title(self) -> &'static str {
        match self {
            Self::Global => "Global",
            Self::Sidebar => "Sidebar",
            Self::Editor => "Request editor",
            Self::Response => "Response viewer",
        }
    }
}

impl fmt::Display for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Which layer a binding comes from; later layers win.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    Default,
    Vim,
    User,
}

/// A binding in a built-in layer.
pub type Builtin = (Context, &'static str, ActionId);

/// The default keymap (spec: Default keymap).
pub const DEFAULT: &[Builtin] = &[
    (Context::Global, "Ctrl+Enter", ActionId::RequestSend),
    (Context::Global, "Ctrl+R", ActionId::RequestSend),
    (Context::Global, "Ctrl+C", ActionId::RequestCancel),
    (Context::Global, "Ctrl+S", ActionId::RequestSave),
    (Context::Global, "Ctrl+T", ActionId::TabNew),
    (Context::Global, "Ctrl+W", ActionId::TabClose),
    (Context::Global, "Ctrl+PageDown", ActionId::TabNext),
    (Context::Global, "Ctrl+PageUp", ActionId::TabPrev),
    (Context::Global, "Alt+1", ActionId::TabGoto1),
    (Context::Global, "Alt+2", ActionId::TabGoto2),
    (Context::Global, "Alt+3", ActionId::TabGoto3),
    (Context::Global, "Alt+4", ActionId::TabGoto4),
    (Context::Global, "Alt+5", ActionId::TabGoto5),
    (Context::Global, "Alt+6", ActionId::TabGoto6),
    (Context::Global, "Alt+7", ActionId::TabGoto7),
    (Context::Global, "Alt+8", ActionId::TabGoto8),
    (Context::Global, "Alt+9", ActionId::TabGoto9),
    (Context::Global, "Ctrl+P", ActionId::PaletteOpen),
    (Context::Global, "?", ActionId::HelpOpen),
    (Context::Global, "F1", ActionId::HelpOpen),
    (Context::Global, "Ctrl+B", ActionId::SidebarToggle),
    (Context::Global, "Alt+Right", ActionId::SidebarGrow),
    (Context::Global, "Alt+Left", ActionId::SidebarShrink),
    (Context::Global, "Tab", ActionId::FocusNext),
    (Context::Global, "Shift+Tab", ActionId::FocusPrev),
    (Context::Global, "Esc", ActionId::Escape),
    (Context::Global, "Alt+E", ActionId::EnvSwitch),
    (Context::Global, "Alt+R", ActionId::SecretsReveal),
    (Context::Global, "Alt+W", ActionId::WebhookOpen),
    (Context::Global, "Alt+O", ActionId::MockOpen),
    (Context::Global, "Alt+I", ActionId::McpOpen),
    (Context::Global, "Alt+C", ActionId::CopyAs),
    (Context::Global, "Ctrl+Q", ActionId::Quit),
    (Context::Global, "Ctrl+G", ActionId::GraphqlRefreshSchema),
    (Context::Global, "Alt+S", ActionId::GraphqlSubscriptionStart),
    (Context::Global, "Alt+X", ActionId::GraphqlSubscriptionStop),
    (Context::Global, "Alt+A", ActionId::SnapshotReview),
    (Context::Editor, "Ctrl+E", ActionId::EditorExternal),
    (Context::Editor, "Alt+M", ActionId::EditorMode),
    (Context::Editor, "Ctrl+Space", ActionId::EditorComplete),
    (Context::Sidebar, "Enter", ActionId::SidebarOpen),
    (Context::Sidebar, "/", ActionId::SidebarFilter),
    (Context::Sidebar, "n", ActionId::SidebarNew),
    (Context::Sidebar, "r", ActionId::SidebarRename),
    (Context::Sidebar, "d", ActionId::SidebarDelete),
    (Context::Sidebar, "Left", ActionId::TreeCollapse),
    (Context::Sidebar, "Right", ActionId::TreeExpand),
    (Context::Sidebar, "Up", ActionId::ListUp),
    (Context::Sidebar, "Down", ActionId::ListDown),
    (Context::Sidebar, "Home", ActionId::ListTop),
    (Context::Sidebar, "End", ActionId::ListBottom),
    (Context::Sidebar, "PageUp", ActionId::ListPageUp),
    (Context::Sidebar, "PageDown", ActionId::ListPageDown),
    (Context::Response, "v", ActionId::VizOpen),
    (Context::Response, "Up", ActionId::ListUp),
    (Context::Response, "Down", ActionId::ListDown),
    (Context::Response, "Home", ActionId::ListTop),
    (Context::Response, "End", ActionId::ListBottom),
    (Context::Response, "PageUp", ActionId::ListPageUp),
    (Context::Response, "PageDown", ActionId::ListPageDown),
    (Context::Response, "]", ActionId::ResponseNextTab),
    (Context::Response, "[", ActionId::ResponsePrevTab),
    (Context::Response, "Enter", ActionId::JsonToggle),
    (Context::Response, "Space", ActionId::JsonToggle),
    (Context::Response, "Left", ActionId::JsonCollapse),
    (Context::Response, "zc", ActionId::JsonCollapse),
    (Context::Response, "Right", ActionId::JsonExpand),
    (Context::Response, "zo", ActionId::JsonExpand),
    (Context::Response, "zM", ActionId::JsonCollapseAll),
    (Context::Response, "zR", ActionId::JsonExpandAll),
    (Context::Response, "y", ActionId::JsonCopy),
    (Context::Response, "r", ActionId::ResponseRaw),
    (Context::Response, "f", ActionId::ResponseFilter),
    (Context::Response, "/", ActionId::ResponseSearch),
    (Context::Response, "n", ActionId::SearchNext),
    (Context::Response, "N", ActionId::SearchPrev),
    (Context::Response, "D", ActionId::ResponseDiff),
    (Context::Response, "s", ActionId::ResponseSaveBody),
    (Context::Response, "H", ActionId::ReportHtml),
];

/// The `vim` preset for lists and panes (spec: Vim mode). Modal editing in
/// text areas is in [`crate::vim`].
pub const VIM: &[Builtin] = &[
    (Context::Sidebar, "j", ActionId::ListDown),
    (Context::Sidebar, "k", ActionId::ListUp),
    (Context::Sidebar, "gg", ActionId::ListTop),
    (Context::Sidebar, "G", ActionId::ListBottom),
    (Context::Sidebar, "Ctrl+D", ActionId::ListPageDown),
    (Context::Sidebar, "Ctrl+U", ActionId::ListPageUp),
    (Context::Sidebar, "l", ActionId::PaneRight),
    (Context::Response, "j", ActionId::ListDown),
    (Context::Response, "k", ActionId::ListUp),
    (Context::Response, "gg", ActionId::ListTop),
    (Context::Response, "G", ActionId::ListBottom),
    (Context::Response, "Ctrl+D", ActionId::ListPageDown),
    (Context::Response, "Ctrl+U", ActionId::ListPageUp),
    (Context::Response, "h", ActionId::PaneLeft),
];

/// The key that quits when the user's bindings leave `app.quit` without one.
const QUIT_FALLBACK: &str = "Ctrl+Q";

/// A bound key sequence. `action` is `None` when the user unbound it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bound {
    action: Option<ActionId>,
    layer: Layer,
    /// Definition order, so keys are listed as the layer lists them.
    order: usize,
}

/// What a key sequence means in a context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Action(ActionId),
    /// The keys start a longer binding (`g` of `gg`); wait for more.
    Pending,
    Unbound,
}

/// Whether the `first` binding resolves before `second` is ever reached:
/// a strict prefix in the same context or over a global binding, or the
/// same keys in a pane over a global binding with another action.
fn hides(
    (ctx, short, action): (Context, &Keys, ActionId),
    (long_ctx, long, long_action): (Context, &Keys, ActionId),
) -> bool {
    if !long.starts_with(short) {
        return false;
    }
    if long.len() > short.len() {
        return long_ctx == ctx || long_ctx == Context::Global;
    }
    long.len() == short.len()
        && ctx != Context::Global
        && long_ctx == Context::Global
        && action != long_action
}

/// The effective keymap after all layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    bindings: BTreeMap<(Context, Keys), Bound>,
    pub vim: bool,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::new(false, &UserLayer::default())
    }
}

impl Keymap {
    /// Defaults, then the vim preset when `vim` is set, then `user`.
    pub fn new(vim: bool, user: &UserLayer) -> Self {
        let mut keymap = Self {
            bindings: BTreeMap::new(),
            vim,
        };
        keymap.add_builtin(DEFAULT, Layer::Default);
        if vim {
            keymap.add_builtin(VIM, Layer::Vim);
        }
        for (context, keys, action) in &user.bindings {
            keymap.bind(*context, keys.clone(), *action, Layer::User);
        }
        keymap
    }

    /// [`Self::new`], plus warnings for user bindings that conflict. If no
    /// key is left for `app.quit`, `Ctrl+Q` is bound to it again so the TUI
    /// can always be closed.
    pub fn checked(vim: bool, user: &UserLayer) -> (Self, Vec<String>) {
        let mut keymap = Self::new(vim, user);
        let mut warnings = keymap.shadowed();
        if keymap.label(ActionId::Quit).is_empty() {
            let keys = chord::parse(QUIT_FALLBACK).expect("the fallback parses");
            keymap.bind(Context::Global, keys, Some(ActionId::Quit), Layer::Default);
            warnings.push(format!(
                "{FILE_NAME}: app.quit has no key left; {QUIT_FALLBACK} quits again"
            ));
        }
        (keymap, warnings)
    }

    /// One warning per binding that a user binding makes unreachable: a
    /// shorter sequence that is a prefix of a longer one resolves first, in
    /// the same context or over a global binding, and a pane binding hides
    /// a global binding of the same keys while that pane is focused.
    fn shadowed(&self) -> Vec<String> {
        self.conflicts(true)
    }

    /// The `shadowed` check; `user_only` limits it to pairs where at least
    /// one side comes from the user layer.
    fn conflicts(&self, user_only: bool) -> Vec<String> {
        let active = || {
            self.bindings
                .iter()
                .filter_map(|((ctx, keys), b)| b.action.map(|a| (*ctx, keys, a, b.layer)))
        };
        let mut warnings = Vec::new();
        for (ctx, short, action, layer) in active() {
            for (long_ctx, long, long_action, long_layer) in active() {
                let by_user = !user_only || layer == Layer::User || long_layer == Layer::User;
                if by_user && hides((ctx, short, action), (long_ctx, long, long_action)) {
                    warnings.push(format!(
                        "{FILE_NAME}: `{}` ({}) in [{ctx}] hides `{}` ({}) in [{long_ctx}], which can't be typed there",
                        chord::display(short),
                        action.id(),
                        chord::display(long),
                        long_action.id(),
                    ));
                }
            }
        }
        warnings
    }

    fn add_builtin(&mut self, table: &[Builtin], layer: Layer) {
        for &(context, text, action) in table {
            let keys = chord::parse(text).expect("built-in bindings parse");
            self.bind(context, keys, Some(action), layer);
        }
    }

    fn bind(&mut self, context: Context, keys: Keys, action: Option<ActionId>, layer: Layer) {
        let order = self.bindings.len();
        let bound = Bound {
            action,
            layer,
            order,
        };
        self.bindings.insert((context, keys), bound);
    }

    /// What `keys` mean with `context` focused: its own bindings first, then
    /// the global ones.
    pub fn resolve(&self, context: Context, keys: &[Chord]) -> Resolution {
        let contexts = if context == Context::Global {
            &[Context::Global][..]
        } else {
            &[context, Context::Global][..]
        };
        for &ctx in contexts {
            if let Some(action) = self.exact(ctx, keys) {
                return Resolution::Action(action);
            }
            if self.has_longer(ctx, keys) {
                return Resolution::Pending;
            }
        }
        Resolution::Unbound
    }

    fn exact(&self, context: Context, keys: &[Chord]) -> Option<ActionId> {
        self.bindings
            .get(&(context, keys.to_vec()))
            .and_then(|b| b.action)
    }

    fn has_longer(&self, context: Context, keys: &[Chord]) -> bool {
        self.bindings.iter().any(|((ctx, bound), b)| {
            *ctx == context
                && b.action.is_some()
                && bound.len() > keys.len()
                && bound.starts_with(keys)
        })
    }

    /// The key sequences bound to `action` in `context`, the user's first.
    pub fn keys_for(&self, context: Context, action: ActionId) -> Vec<Keys> {
        let mut found: Vec<(&Bound, &Keys)> = self
            .bindings
            .iter()
            .filter(|((ctx, _), b)| *ctx == context && b.action == Some(action))
            .map(|((_, keys), b)| (b, keys))
            .collect();
        found.sort_by(|a, b| b.0.layer.cmp(&a.0.layer).then(a.0.order.cmp(&b.0.order)));
        found.into_iter().map(|(_, keys)| keys.clone()).collect()
    }

    /// Every binding of `action` in any context, displayed and deduplicated
    /// (`Ctrl+Enter, Ctrl+R`); empty when it has none.
    pub fn label(&self, action: ActionId) -> String {
        let mut labels: Vec<String> = Vec::new();
        for context in Context::ALL {
            for keys in self.keys_for(context, action) {
                let text = chord::display(&keys);
                if !labels.contains(&text) {
                    labels.push(text);
                }
            }
        }
        labels.join(", ")
    }

    /// The first key sequence bound to `action`, displayed (`Ctrl+Enter`),
    /// for hints that name one key; `None` when it has none.
    pub fn key(&self, action: ActionId) -> Option<String> {
        Context::ALL
            .iter()
            .find_map(|&context| self.keys_for(context, action).into_iter().next())
            .map(|keys| chord::display(&keys))
    }

    /// The actions bound in `context` with their keys, in registry order.
    pub fn bindings_in(&self, context: Context) -> Vec<(ActionId, Vec<Keys>)> {
        ActionId::all()
            .map(|action| (action, self.keys_for(context, action)))
            .filter(|(_, keys)| !keys.is_empty())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(text: &str) -> Keys {
        chord::parse(text).unwrap()
    }

    fn user(text: &str) -> UserLayer {
        let (layer, warnings) = UserLayer::parse(text);
        assert!(warnings.is_empty(), "{warnings:?}");
        layer
    }

    #[test]
    fn builtin_tables_parse_and_have_no_duplicates() {
        for table in [DEFAULT, VIM] {
            let mut seen = std::collections::HashSet::new();
            for (context, text, _) in table {
                assert!(seen.insert((context, keys(text))), "{context} {text}");
            }
        }
    }

    #[test]
    fn defaults_cover_the_spec_keymap() {
        let keymap = Keymap::default();
        let global = |text: &str| keymap.resolve(Context::Global, &keys(text));
        for (text, action) in [
            ("Ctrl+Enter", ActionId::RequestSend),
            ("Ctrl+R", ActionId::RequestSend),
            ("Esc", ActionId::Escape),
            ("Ctrl+S", ActionId::RequestSave),
            ("Ctrl+T", ActionId::TabNew),
            ("Ctrl+W", ActionId::TabClose),
            ("Ctrl+P", ActionId::PaletteOpen),
            ("Ctrl+B", ActionId::SidebarToggle),
            ("Alt+E", ActionId::EnvSwitch),
            ("Alt+R", ActionId::SecretsReveal),
            ("Tab", ActionId::FocusNext),
            ("Shift+Tab", ActionId::FocusPrev),
            ("?", ActionId::HelpOpen),
            ("Ctrl+Q", ActionId::Quit),
        ] {
            assert_eq!(global(text), Resolution::Action(action), "{text}");
        }
        let editor = keymap.resolve(Context::Editor, &keys("Ctrl+E"));
        assert_eq!(editor, Resolution::Action(ActionId::EditorExternal));
        let response = keymap.resolve(Context::Response, &keys("v"));
        assert_eq!(response, Resolution::Action(ActionId::VizOpen));
    }

    #[test]
    fn the_focused_context_falls_back_to_global() {
        let keymap = Keymap::default();
        let in_editor = keymap.resolve(Context::Editor, &keys("Ctrl+P"));
        assert_eq!(in_editor, Resolution::Action(ActionId::PaletteOpen));
        let v_in_sidebar = keymap.resolve(Context::Sidebar, &keys("v"));
        assert_eq!(v_in_sidebar, Resolution::Unbound);
    }

    #[test]
    fn vim_adds_sequences_that_wait_for_the_next_key() {
        let keymap = Keymap::new(true, &UserLayer::default());
        let sidebar = |text: &str| keymap.resolve(Context::Sidebar, &keys(text));
        assert_eq!(sidebar("g"), Resolution::Pending);
        assert_eq!(sidebar("gg"), Resolution::Action(ActionId::ListTop));
        assert_eq!(sidebar("j"), Resolution::Action(ActionId::ListDown));
        assert_eq!(sidebar("Down"), Resolution::Action(ActionId::ListDown));
        let plain = Keymap::default();
        assert_eq!(
            plain.resolve(Context::Sidebar, &keys("j")),
            Resolution::Unbound
        );
    }

    #[test]
    fn the_user_beats_vim_which_beats_the_defaults() {
        let user = user(
            "[global]\n\"F5\" = \"request.send\"\n\"Ctrl+P\" = \"help.open\"\n\
             [sidebar]\n\"j\" = \"list.up\"\n\"Down\" = \"none\"",
        );
        let keymap = Keymap::new(true, &user);
        let r = |ctx, text: &str| keymap.resolve(ctx, &keys(text));
        assert_eq!(
            r(Context::Editor, "F5"),
            Resolution::Action(ActionId::RequestSend)
        );
        assert_eq!(
            r(Context::Global, "Ctrl+P"),
            Resolution::Action(ActionId::HelpOpen)
        );
        assert_eq!(
            r(Context::Sidebar, "j"),
            Resolution::Action(ActionId::ListUp)
        );
        assert_eq!(r(Context::Sidebar, "Down"), Resolution::Unbound);
        assert_eq!(
            r(Context::Global, "Ctrl+Enter"),
            Resolution::Action(ActionId::RequestSend),
            "adding a key keeps the defaults"
        );
        assert_eq!(
            keymap.label(ActionId::RequestSend),
            "F5, Ctrl+Enter, Ctrl+R"
        );
        assert_eq!(keymap.label(ActionId::PaletteOpen), "");
        // Hints name the first key only: the user's.
        assert_eq!(keymap.key(ActionId::RequestSend).as_deref(), Some("F5"));
        assert_eq!(keymap.key(ActionId::PaletteOpen), None);
        let default = Keymap::default();
        assert_eq!(
            default.key(ActionId::RequestSend).as_deref(),
            Some("Ctrl+Enter")
        );
        assert_eq!(default.key(ActionId::SidebarNew).as_deref(), Some("n"));
    }

    fn checked(vim: bool, text: &str) -> (Keymap, Vec<String>) {
        Keymap::checked(vim, &user(text))
    }

    #[test]
    fn unbinding_every_quit_key_brings_ctrl_q_back() {
        let (keymap, warnings) = checked(false, "[global]\n\"Ctrl+Q\" = \"none\"");
        assert_eq!(keymap.label(ActionId::Quit), "Ctrl+Q");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("app.quit has no key"), "{warnings:?}");
        let (keymap, warnings) = checked(
            false,
            "[global]\n\"Ctrl+Q\" = \"help.open\"\n\"F10\" = \"none\"",
        );
        assert_eq!(keymap.label(ActionId::Quit), "Ctrl+Q");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let (keymap, warnings) = checked(
            false,
            "[global]\n\"F10\" = \"app.quit\"\n\"Ctrl+Q\" = \"none\"",
        );
        assert_eq!(keymap.label(ActionId::Quit), "F10");
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn a_user_prefix_that_hides_a_longer_binding_is_reported() {
        let (_, warnings) = checked(true, "[sidebar]\n\"g\" = \"help.open\"");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("`g` (help.open) in [sidebar] hides `gg` (list.top)"),
            "{warnings:?}"
        );
        let (_, warnings) = checked(
            false,
            "[global]\n\"F5\" = \"app.quit\"\n\"F5 F6\" = \"help.open\"",
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("hides `F5 F6`"), "{warnings:?}");
        let (_, warnings) = checked(
            false,
            "[sidebar]\n\"F5\" = \"help.open\"\n[global]\n\"F5 F6\" = \"app.quit\"",
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("in [global]"), "{warnings:?}");
    }

    #[test]
    fn a_pane_binding_that_hides_a_global_one_is_reported() {
        let (keymap, warnings) = checked(false, "[editor]\n\"Ctrl+Q\" = \"help.open\"");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0]
                .contains("`Ctrl+Q` (help.open) in [editor] hides `Ctrl+Q` (app.quit) in [global]"),
            "{warnings:?}"
        );
        let quit = keymap.resolve(Context::Sidebar, &keys("Ctrl+Q"));
        assert_eq!(quit, Resolution::Action(ActionId::Quit));
        // The same action in the pane hides nothing.
        let (_, warnings) = checked(false, "[editor]\n\"Ctrl+Q\" = \"app.quit\"");
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn the_builtin_layers_hide_nothing() {
        for vim in [true, false] {
            let keymap = Keymap::new(vim, &user(""));
            assert_eq!(keymap.conflicts(false), Vec::<String>::new());
        }
    }

    #[test]
    fn the_unfiltered_check_catches_a_builtin_conflict() {
        let mut keymap = Keymap::default();
        let keys = chord::parse("Ctrl+P").unwrap();
        keymap.bind(
            Context::Response,
            keys,
            Some(ActionId::Quit),
            Layer::Default,
        );
        assert!(keymap.shadowed().is_empty());
        assert!(!keymap.conflicts(false).is_empty());
    }

    #[test]
    fn bindings_in_a_context_follow_registry_order() {
        let keymap = Keymap::default();
        let response = keymap.bindings_in(Context::Response);
        assert_eq!(response[0].0, ActionId::VizOpen);
        assert!(response.iter().all(|(_, keys)| !keys.is_empty()));
        assert_eq!(keymap.label(ActionId::HelpOpen), "?, F1");
        assert_eq!(keymap.label(ActionId::ListDown), "Down");
        let vim = Keymap::new(true, &UserLayer::default());
        assert_eq!(vim.label(ActionId::ListDown), "j, Down");
    }

    #[test]
    fn contexts_follow_focus() {
        assert_eq!(Context::of(Focus::Url), Context::Editor);
        assert_eq!(Context::of(Focus::Sidebar), Context::Sidebar);
        assert_eq!(Context::of(Focus::Response), Context::Response);
        for context in Context::ALL {
            assert_eq!(Context::from_name(context.name()), Some(context));
        }
    }
}
