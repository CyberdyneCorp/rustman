//! The JSON tree viewer's model (spec: JSON tree viewer, design D6).
//!
//! A body is parsed once, off the render loop, into a flat [`JsonTree`]:
//! nodes in document order, each knowing where its subtree ends, so folding
//! skips a subtree in one step. [`TreeView`] keeps the per-tab fold flags and
//! the list of visible rows; drawing reads only the rows on screen (a virtual
//! list), so scrolling costs the same for a 50 MiB body as for a small one.

use std::fmt::Write;
use std::sync::Arc;

use serde_json::Value;

/// What a scalar is, for its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scalar {
    String,
    Number,
    Bool,
    Null,
}

/// A node's value: a container with its child count, or a scalar as JSON
/// text (strings keep their quotes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Object(u32),
    Array(u32),
    Scalar(Scalar, Box<str>),
}

/// How a node is named in its parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Label {
    /// The document root.
    None,
    /// An object member, or a filter match named by its path.
    Name(Box<str>),
    Index(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub label: Label,
    pub kind: Kind,
    pub depth: u16,
    /// The parent's index; `None` for a top-level node.
    pub parent: Option<u32>,
    /// One past the last node of this subtree.
    pub end: u32,
}

impl Node {
    pub fn is_container(&self) -> bool {
        !matches!(self.kind, Kind::Scalar(..))
    }

    /// `[1000 items]`, `{3 keys}`, or the scalar's JSON text.
    pub fn summary(&self) -> String {
        let plural = |n: u32, one: &str, many: &str| {
            if n == 1 {
                format!("1 {one}")
            } else {
                format!("{n} {many}")
            }
        };
        match &self.kind {
            Kind::Object(n) => format!("{{{}}}", plural(*n, "key", "keys")),
            Kind::Array(n) => format!("[{}]", plural(*n, "item", "items")),
            Kind::Scalar(_, text) => text.to_string(),
        }
    }

    /// The label as shown before `: `; `None` for the root.
    pub fn label_text(&self) -> Option<String> {
        match &self.label {
            Label::None => None,
            Label::Name(name) => Some(name.to_string()),
            Label::Index(i) => Some(format!("[{i}]")),
        }
    }
}

/// A parsed body, flattened in document order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JsonTree {
    nodes: Vec<Node>,
}

impl JsonTree {
    /// The tree of one document.
    pub fn from_value(value: &Value) -> Self {
        Self::from_roots(&[(Label::None, value)])
    }

    /// Several top-level nodes, such as the matches of a filter.
    pub fn from_roots(roots: &[(Label, &Value)]) -> Self {
        let mut tree = Self::default();
        for (label, value) in roots {
            tree.push(label.clone(), value, 0, None);
        }
        tree
    }

    fn push(&mut self, label: Label, value: &Value, depth: u16, parent: Option<u32>) {
        let index = self.nodes.len();
        let kind = match value {
            Value::Object(map) => Kind::Object(count(map.len())),
            Value::Array(items) => Kind::Array(count(items.len())),
            Value::String(_) => Kind::Scalar(Scalar::String, value.to_string().into()),
            Value::Number(n) => Kind::Scalar(Scalar::Number, n.to_string().into()),
            Value::Bool(b) => Kind::Scalar(Scalar::Bool, b.to_string().into()),
            Value::Null => Kind::Scalar(Scalar::Null, "null".into()),
        };
        self.nodes.push(Node {
            label,
            kind,
            depth,
            parent,
            end: 0,
        });
        let me = Some(count(index));
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    self.push(Label::Name(key.as_str().into()), child, depth + 1, me);
                }
            }
            Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    self.push(Label::Index(count(i)), child, depth + 1, me);
                }
            }
            _ => {}
        }
        self.nodes[index].end = count(self.nodes.len());
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// How many top-level nodes there are (the matches of a filter).
    pub fn len_top(&self) -> usize {
        self.nodes.iter().filter(|n| n.parent.is_none()).count()
    }

    pub fn node(&self, index: usize) -> &Node {
        &self.nodes[index]
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// The node's value as compact JSON.
    pub fn to_json(&self, index: usize) -> String {
        let mut out = String::new();
        self.write_json(index, &mut out);
        out
    }

    fn write_json(&self, index: usize, out: &mut String) {
        let node = &self.nodes[index];
        let (open, close) = match &node.kind {
            Kind::Scalar(_, text) => {
                out.push_str(text);
                return;
            }
            Kind::Object(_) => ('{', '}'),
            Kind::Array(_) => ('[', ']'),
        };
        out.push(open);
        for (n, child) in self.children(index).enumerate() {
            if n > 0 {
                out.push(',');
            }
            if let Label::Name(name) = &self.nodes[child].label {
                out.push_str(&Value::String(name.to_string()).to_string());
                out.push(':');
            }
            self.write_json(child, out);
        }
        out.push(close);
    }

    /// The direct children of `index`.
    pub fn children(&self, index: usize) -> impl Iterator<Item = usize> + '_ {
        let end = self.nodes[index].end as usize;
        let mut next = index + 1;
        std::iter::from_fn(move || {
            if next >= end {
                return None;
            }
            let child = next;
            next = self.nodes[child].end as usize;
            Some(child)
        })
    }

    /// What `y` copies: a string's text without quotes, anything else as
    /// JSON.
    pub fn copy_text(&self, index: usize) -> String {
        match &self.nodes[index].kind {
            Kind::Scalar(Scalar::String, text) => {
                serde_json::from_str::<String>(text).unwrap_or_else(|_| text.to_string())
            }
            _ => self.to_json(index),
        }
    }

    /// The node's JSONPath, e.g. `$.user.id` or `$.items[0]`.
    pub fn path(&self, index: usize) -> String {
        let mut parts = Vec::new();
        let mut at = Some(index);
        while let Some(i) = at {
            let node = &self.nodes[i];
            match &node.label {
                Label::None => {}
                Label::Index(n) => parts.push(format!("[{n}]")),
                Label::Name(name) if node.parent.is_none() => parts.push(name.to_string()),
                Label::Name(name) => parts.push(member(name)),
            }
            at = node.parent.map(|p| p as usize);
        }
        if !parts.last().is_some_and(|p| p.starts_with('$')) {
            parts.push("$".to_owned());
        }
        parts.iter().rev().fold(String::new(), |mut out, p| {
            let _ = write!(out, "{p}");
            out
        })
    }
}

/// `.name`, or `['odd name']` when it is not a plain identifier.
fn member(name: &str) -> String {
    let plain = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_alphanumeric() || c == '_');
    if plain {
        format!(".{name}")
    } else {
        format!("['{}']", name.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The fold state and visible rows of one tree on screen.
#[derive(Debug, Clone)]
pub struct TreeView {
    pub tree: Arc<JsonTree>,
    collapsed: Vec<bool>,
    /// Indexes of the nodes on screen when scrolled through, in order.
    visible: Vec<u32>,
    /// The selected row (an index into the visible rows).
    pub cursor: usize,
    /// The first row on screen.
    pub scroll: usize,
}

impl TreeView {
    /// Every node expanded.
    pub fn new(tree: Arc<JsonTree>) -> Self {
        let collapsed = vec![false; tree.len()];
        let mut view = Self {
            tree,
            collapsed,
            visible: Vec::new(),
            cursor: 0,
            scroll: 0,
        };
        view.rebuild();
        view
    }

    fn rebuild(&mut self) {
        let selected = self.selected();
        self.visible.clear();
        let mut i = 0;
        while i < self.tree.len() {
            self.visible.push(count(i));
            i = if self.collapsed[i] {
                self.tree.node(i).end as usize
            } else {
                i + 1
            };
        }
        self.cursor = selected
            .and_then(|node| self.row_of(node))
            .unwrap_or(0)
            .min(self.visible.len().saturating_sub(1));
    }

    fn row_of(&self, node: usize) -> Option<usize> {
        self.visible.binary_search(&count(node)).ok()
    }

    /// The number of rows when scrolled through.
    pub fn rows(&self) -> usize {
        self.visible.len()
    }

    /// The node of visible row `row`.
    pub fn node_at(&self, row: usize) -> Option<usize> {
        self.visible.get(row).map(|&i| i as usize)
    }

    /// The selected node.
    pub fn selected(&self) -> Option<usize> {
        self.node_at(self.cursor)
    }

    pub fn is_collapsed(&self, node: usize) -> bool {
        self.collapsed[node]
    }

    /// `Enter`/`Space`: fold or unfold the selected container.
    pub fn toggle(&mut self) {
        if let Some(node) = self.selected() {
            let folded = self.collapsed[node];
            self.set(node, !folded);
        }
    }

    /// `zc`/`Left`: fold the selected container, or go to the parent.
    pub fn collapse(&mut self) {
        let Some(node) = self.selected() else { return };
        if self.tree.node(node).is_container() && !self.collapsed[node] {
            self.set(node, true);
        } else if let Some(parent) = self.tree.node(node).parent {
            self.cursor = self.row_of(parent as usize).unwrap_or(self.cursor);
        }
    }

    /// `zo`/`Right`: unfold the selected container.
    pub fn expand(&mut self) {
        if let Some(node) = self.selected() {
            self.set(node, false);
        }
    }

    fn set(&mut self, node: usize, folded: bool) {
        if self.tree.node(node).is_container() && self.collapsed[node] != folded {
            self.collapsed[node] = folded;
            self.rebuild();
        }
    }

    /// `zM`: fold every container below the top level.
    pub fn collapse_all(&mut self) {
        for (i, node) in self.tree.nodes().iter().enumerate() {
            self.collapsed[i] = node.is_container() && node.depth > 0;
        }
        self.cursor = 0;
        self.rebuild();
    }

    /// `zR`: unfold everything.
    pub fn expand_all(&mut self) {
        self.collapsed.fill(false);
        self.rebuild();
    }

    /// Unfold the ancestors of `node` and select it.
    pub fn reveal(&mut self, node: usize) {
        let mut changed = false;
        let mut at = self.tree.node(node).parent;
        while let Some(parent) = at {
            changed |= std::mem::replace(&mut self.collapsed[parent as usize], false);
            at = self.tree.node(parent as usize).parent;
        }
        if changed {
            self.rebuild();
        }
        self.cursor = self.row_of(node).unwrap_or(self.cursor);
    }

    /// Move the selection by `delta` rows, clamped.
    pub fn move_by(&mut self, delta: isize) {
        let last = self.rows().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }

    /// The first row on screen in a window of `height` rows, keeping the
    /// selection on screen.
    pub fn top(&self, height: usize) -> usize {
        let height = height.max(1);
        self.scroll
            .min(self.cursor)
            .max((self.cursor + 1).saturating_sub(height))
    }

    /// Remember where the window of `height` rows starts.
    pub fn follow(&mut self, height: usize) {
        self.scroll = self.top(height);
    }

    /// The nodes on screen in a window of `height` rows.
    pub fn window(&self, height: usize) -> Vec<usize> {
        let start = self.top(height);
        let end = (start + height.max(1)).min(self.visible.len());
        self.visible[start.min(end)..end]
            .iter()
            .map(|&i| i as usize)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn view(value: &Value) -> TreeView {
        TreeView::new(Arc::new(JsonTree::from_value(value)))
    }

    #[test]
    fn nodes_are_flattened_with_their_subtree_ends() {
        let tree = JsonTree::from_value(&json!({"a": [1, {"b": null}], "c": "x"}));
        let summaries: Vec<String> = tree.nodes().iter().map(Node::summary).collect();
        assert_eq!(
            summaries,
            ["{2 keys}", "[2 items]", "1", "{1 key}", "null", "\"x\""]
        );
        assert_eq!(tree.node(1).end, 5);
        assert_eq!(tree.children(0).collect::<Vec<_>>(), [1, 5]);
        assert_eq!(tree.path(4), "$.a[1].b");
        assert_eq!(tree.to_json(1), "[1,{\"b\":null}]");
        assert_eq!(tree.copy_text(5), "x");
        assert_eq!(tree.copy_text(0), r#"{"a":[1,{"b":null}],"c":"x"}"#);
    }

    #[test]
    fn odd_member_names_are_quoted_in_paths() {
        let tree = JsonTree::from_value(&json!({"a b": {"it's": 1}}));
        assert_eq!(tree.path(2), r"$['a b']['it\'s']");
    }

    #[test]
    fn a_collapsed_array_shows_its_count_and_hides_its_items() {
        // Scenario "Collapse a large array".
        let items: Vec<Value> = (0..1000).map(|i| json!({"id": i})).collect();
        let mut view = view(&json!({ "items": items }));
        assert_eq!(view.rows(), 2 + 2000);
        view.move_by(1);
        view.toggle();
        assert_eq!(view.rows(), 2);
        let node = view.tree.node(view.selected().unwrap());
        assert_eq!(
            format!("{}: {}", node.label_text().unwrap(), node.summary()),
            "items: [1000 items]"
        );
        view.expand();
        assert_eq!(view.rows(), 2002);
    }

    #[test]
    fn collapse_all_expand_all_and_reveal() {
        let mut view = view(&json!({"a": {"b": {"c": 1}}, "d": [1, 2]}));
        view.collapse_all();
        assert_eq!(view.rows(), 3, "the root and its two members");
        view.reveal(3);
        assert_eq!(view.tree.path(view.selected().unwrap()), "$.a.b.c");
        view.expand_all();
        assert_eq!(view.rows(), view.tree.len());
        // Left on a leaf goes to its parent; on an open container it folds.
        view.collapse();
        assert_eq!(view.tree.path(view.selected().unwrap()), "$.a.b");
        view.collapse();
        assert!(view.is_collapsed(2));
    }

    #[test]
    fn the_window_follows_the_cursor() {
        let mut view = view(&json!((0..100).collect::<Vec<_>>()));
        view.move_by(50);
        let rows = view.window(10);
        assert_eq!(rows.len(), 10);
        assert_eq!(*rows.last().unwrap(), 50);
        view.follow(10);
        view.move_by(-5);
        assert_eq!(
            view.window(10)[0],
            41,
            "moving up inside the window keeps it"
        );
        view.move_by(-1000);
        assert_eq!(view.window(10)[0], 0);
        view.move_by(1000);
        assert_eq!(view.cursor, 100);
    }
}
