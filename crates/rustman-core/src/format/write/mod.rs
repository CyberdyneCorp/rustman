//! Deterministic YAML emitter (design D4, "Stable serialization").
//!
//! Writers never use a generic serializer. The typed model is first turned
//! into a [`Node`] tree in schema order ([`request`]), and [`render`] prints it
//! with 2-space indentation, block style, a trailing newline and the quoting
//! rules in [`scalar`].

mod realtime;
mod request;
mod scalar;

pub use realtime::{sse_to_yaml, ws_to_yaml};
pub use request::{
    assertions_to_yaml, auth_node, collection_to_yaml, environment_to_yaml, graphql_to_yaml,
    json_node, mcp_to_yaml, name_values_node, request_to_yaml,
};

/// A YAML node ready to print.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// A string, quoted only when YAML requires it.
    Str(String),
    /// A scalar printed verbatim: numbers, `true`, `false`, `null`.
    Plain(String),
    Map(Vec<(String, Node)>),
    Seq(Vec<Node>),
}

impl Node {
    pub fn str(value: impl Into<String>) -> Self {
        Self::Str(value.into())
    }

    pub fn plain(value: impl ToString) -> Self {
        Self::Plain(value.to_string())
    }
}

/// Builds a mapping in schema order, skipping absent optional values.
#[derive(Default)]
pub struct MapBuilder {
    entries: Vec<(String, Node)>,
}

impl MapBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn node(mut self, key: &str, node: Node) -> Self {
        self.entries.push((key.to_owned(), node));
        self
    }

    pub fn opt(self, key: &str, node: Option<Node>) -> Self {
        match node {
            Some(node) => self.node(key, node),
            None => self,
        }
    }

    pub fn str(self, key: &str, value: &str) -> Self {
        self.node(key, Node::str(value))
    }

    pub fn opt_str(self, key: &str, value: Option<&str>) -> Self {
        self.opt(key, value.map(Node::str))
    }

    pub fn opt_plain<T: ToString>(self, key: &str, value: Option<T>) -> Self {
        self.opt(key, value.map(|v| Node::plain(v)))
    }

    pub fn build(self) -> Node {
        Node::Map(self.entries)
    }
}

/// Render a document. The result always ends with a newline.
pub fn render(node: &Node) -> String {
    let mut out = String::new();
    match node {
        Node::Map(entries) if !entries.is_empty() => write_map(&mut out, entries, 0, false),
        Node::Seq(items) if !items.is_empty() => write_seq(&mut out, items, 0, false),
        other => {
            write_inline(&mut out, other, 0);
            out.push('\n');
        }
    }
    out
}

fn indent(out: &mut String, width: usize) {
    out.extend(std::iter::repeat_n(' ', width));
}

fn write_map(out: &mut String, entries: &[(String, Node)], width: usize, first_inline: bool) {
    for (i, (key, value)) in entries.iter().enumerate() {
        if i > 0 || !first_inline {
            indent(out, width);
        }
        scalar::write_key(out, key);
        out.push(':');
        write_after_key(out, value, width);
    }
}

fn write_seq(out: &mut String, items: &[Node], width: usize, first_inline: bool) {
    for (i, item) in items.iter().enumerate() {
        if i > 0 || !first_inline {
            indent(out, width);
        }
        out.push_str("- ");
        write_item(out, item, width + 2);
    }
}

/// Write a mapping value; the cursor is right after `key:`.
fn write_after_key(out: &mut String, value: &Node, width: usize) {
    match value {
        Node::Map(entries) if !entries.is_empty() => {
            out.push('\n');
            write_map(out, entries, width + 2, false);
        }
        Node::Seq(items) if !items.is_empty() => {
            out.push('\n');
            write_seq(out, items, width + 2, false);
        }
        other => {
            out.push(' ');
            write_inline(out, other, width + 2);
            out.push('\n');
        }
    }
}

/// Write a sequence item; the cursor is right after `- `.
fn write_item(out: &mut String, item: &Node, width: usize) {
    match item {
        Node::Map(entries) if !entries.is_empty() => write_map(out, entries, width, true),
        Node::Seq(items) if !items.is_empty() => write_seq(out, items, width, true),
        other => {
            write_inline(out, other, width);
            out.push('\n');
        }
    }
}

/// Write a scalar or an empty collection. Block scalars continue on the
/// following lines at `content_width`; the final newline is left to the caller.
fn write_inline(out: &mut String, node: &Node, content_width: usize) {
    match node {
        Node::Str(value) => scalar::write_value(out, value, content_width),
        Node::Plain(value) => out.push_str(value),
        Node::Map(_) => out.push_str("{}"),
        Node::Seq(_) => out.push_str("[]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: Vec<(&str, Node)>) -> Node {
        Node::Map(
            entries
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
        )
    }

    #[test]
    fn renders_nested_block_structures() {
        let doc = map(vec![
            ("a", Node::plain(1)),
            ("b", map(vec![("c", Node::str("x"))])),
            (
                "d",
                Node::Seq(vec![
                    map(vec![("name", Node::str("p")), ("v", Node::str("q"))]),
                    Node::Seq(vec![Node::plain(1), Node::plain(2)]),
                    Node::str("s"),
                ]),
            ),
            ("e", Node::Map(vec![])),
            ("f", Node::Seq(vec![])),
        ]);
        let expected = "\
a: 1
b:
  c: x
d:
  - name: p
    v: q
  - - 1
    - 2
  - s
e: {}
f: []
";
        assert_eq!(render(&doc), expected);
    }

    #[test]
    fn renders_block_literals_at_the_content_indent() {
        let doc = map(vec![
            ("a", map(vec![("t", Node::str("one\ntwo\n"))])),
            ("b", Node::Seq(vec![Node::str("x\ny")])),
        ]);
        assert_eq!(
            render(&doc),
            "a:\n  t: |\n    one\n    two\nb:\n  - |-\n    x\n    y\n"
        );
    }
}
