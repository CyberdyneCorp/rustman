//! JSONPath filters and incremental search in the JSON tree (spec: JSON
//! filter and search). Filters use `serde_json_path` (RFC 9535); jq syntax
//! is not supported (design D6).

use std::sync::Arc;

use serde_json::Value;
use serde_json_path::JsonPath;

use super::json_tree::{JsonTree, Kind, Label};
use super::received::Source;

/// Parse a filter expression; the error is shown inline.
pub fn parse(expr: &str) -> Result<JsonPath, String> {
    JsonPath::parse(expr.trim()).map_err(|e| e.to_string())
}

/// The nodes `path` selects in the JSON body held by `source`, each named by
/// its location, as a tree of its own. Parses the body, so it runs off the
/// render loop.
pub fn filter(source: &Source, path: &JsonPath) -> Result<Arc<JsonTree>, String> {
    let bytes = source.read_all().map_err(|e| e.to_string())?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    drop(bytes);
    Ok(Arc::new(select(&value, path)))
}

/// The matches of `path` in `value` as top-level nodes.
pub fn select(value: &Value, path: &JsonPath) -> JsonTree {
    let located = path.query_located(value);
    let roots: Vec<(Label, &Value)> = located
        .iter()
        .map(|node| (Label::Name(node.location().to_string().into()), node.node()))
        .collect();
    JsonTree::from_roots(&roots)
}

/// The nodes whose label or scalar value contains `query`, ignoring case,
/// in document order.
pub fn search(tree: &JsonTree, query: &str) -> Vec<usize> {
    search_until(tree, query, &|| false).unwrap_or_default()
}

/// [`search`] that gives up (`None`) once `cancelled` returns true; it is
/// asked every [`CANCEL_CHECK`] nodes.
pub fn search_until(
    tree: &JsonTree,
    query: &str,
    cancelled: &dyn Fn() -> bool,
) -> Option<Vec<usize>> {
    matching(tree, 0..tree.len(), query, cancelled)
}

/// [`search`] for `query` after a search for `previous` found `matches`.
/// Typing extends the query, and a node that contains the longer query also
/// contains the shorter one, so only the earlier matches are scanned again
/// (the per-keystroke cost shrinks as the query grows on large trees).
pub fn refine(tree: &JsonTree, previous: &str, matches: &[usize], query: &str) -> Vec<usize> {
    refine_until(tree, previous, matches, query, &|| false).unwrap_or_default()
}

/// [`refine`] that gives up like [`search_until`].
pub fn refine_until(
    tree: &JsonTree,
    previous: &str,
    matches: &[usize],
    query: &str,
    cancelled: &dyn Fn() -> bool,
) -> Option<Vec<usize>> {
    let narrows = !previous.is_empty() && query.to_lowercase().contains(&previous.to_lowercase());
    if narrows {
        matching(tree, matches.iter().copied(), query, cancelled)
    } else {
        search_until(tree, query, cancelled)
    }
}

/// How many nodes a search scans between checks for cancellation.
pub const CANCEL_CHECK: usize = 4096;

fn matching(
    tree: &JsonTree,
    nodes: impl Iterator<Item = usize>,
    query: &str,
    cancelled: &dyn Fn() -> bool,
) -> Option<Vec<usize>> {
    if query.is_empty() {
        return Some(Vec::new());
    }
    let needle = query.to_lowercase();
    let hit = |text: &str| text.to_lowercase().contains(&needle);
    let mut found = Vec::new();
    for (scanned, i) in nodes.enumerate() {
        if scanned % CANCEL_CHECK == 0 && cancelled() {
            return None;
        }
        if i >= tree.len() {
            continue;
        }
        let node = tree.node(i);
        let label = match &node.label {
            Label::Name(name) => hit(name),
            Label::None | Label::Index(_) => false,
        };
        if label || matches!(&node.kind, Kind::Scalar(_, text) if hit(text)) {
            found.push(i);
        }
    }
    Some(found)
}

/// The match after (or before) `node` in `matches`, wrapping around.
pub fn step(matches: &[usize], node: Option<usize>, forward: bool) -> Option<usize> {
    let at = node.unwrap_or(0);
    let next = if forward {
        matches
            .iter()
            .find(|&&m| m > at)
            .or_else(|| matches.first())
    } else {
        matches
            .iter()
            .rev()
            .find(|&&m| m < at)
            .or_else(|| matches.last())
    };
    next.copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn items() -> Value {
        json!({"items": [{"id": 1}, {"id": 2}, {"id": 3}], "name": "Ana"})
    }

    #[test]
    fn a_filter_shows_only_the_matches() {
        // Scenario "Filter with JSONPath".
        let path = parse("$.items[*].id").unwrap();
        let tree = select(&items(), &path);
        let values: Vec<String> = tree.nodes().iter().map(|n| n.summary()).collect();
        assert_eq!(values, ["1", "2", "3"]);
        assert_eq!(
            tree.node(0).label_text().as_deref(),
            Some("$['items'][0]['id']")
        );
        let source = Source::Memory(items().to_string().into_bytes().into());
        assert_eq!(filter(&source, &path).unwrap().len(), 3);
    }

    #[test]
    fn invalid_and_empty_filters() {
        // Scenario "Invalid filter".
        assert!(parse("$.items[").is_err());
        let none = select(&items(), &parse("$.nothing").unwrap());
        assert!(none.is_empty());
        let source = Source::Memory(b"not json".to_vec().into());
        assert!(filter(&source, &parse("$").unwrap()).is_err());
    }

    #[test]
    fn search_finds_keys_and_values_and_steps_around() {
        let tree = JsonTree::from_value(&items());
        let hits = search(&tree, "ID");
        assert_eq!(hits.len(), 3);
        assert_eq!(search(&tree, "ana"), [8]);
        assert!(search(&tree, "").is_empty());
        assert_eq!(step(&hits, Some(hits[0]), true), Some(hits[1]));
        assert_eq!(step(&hits, Some(hits[2]), true), Some(hits[0]));
        assert_eq!(step(&hits, Some(hits[0]), false), Some(hits[2]));
        assert_eq!(step(&[], None, true), None);
    }

    #[test]
    fn refining_a_search_scans_only_the_earlier_matches() {
        let tree = JsonTree::from_value(&json!({"user": "Ana", "users": [1], "us": "x"}));
        let us = search(&tree, "us");
        assert_eq!(refine(&tree, "us", &us, "USE"), search(&tree, "use"));
        // Only earlier matches are looked at: a stale list proves it.
        assert!(refine(&tree, "us", &[], "use").is_empty());
        // A query that does not extend the previous one searches again.
        assert_eq!(refine(&tree, "use", &[], "an"), search(&tree, "an"));
        assert_eq!(refine(&tree, "", &[], "x"), search(&tree, "x"));
    }
}
