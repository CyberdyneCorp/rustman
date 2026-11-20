//! The object a schema describes, `allOf` included. An `allOf` is a
//! conjunction: its members' properties, `required` lists and types add up
//! to one object. Flattening it this way means moving fields between the
//! members, adding or removing a member, or wrapping a schema in an
//! equivalent `allOf` is compared field by field like any other object.

use serde_json::Value;

use crate::spec::{Node, inline};

/// The most parts collected from one schema (nested `allOf`s included).
const MAX_PARTS: usize = 256;

/// Keywords the flattened object accounts for; the rest of an `allOf`
/// member is compared as a composition (see [`residual`]).
const OBJECT_KEYS: [&str; 4] = ["type", "properties", "required", "allOf"];

/// `node` (resolved) followed by the resolved members of its `allOf`, and of
/// theirs, depth first. A member already collected is not collected again.
pub fn parts<'a>(node: &Node<'a>, doc: &'a Value) -> Vec<Node<'a>> {
    let mut out = Vec::new();
    collect(node.clone().resolved(doc), doc, &mut out);
    out
}

fn collect<'a>(node: Node<'a>, doc: &'a Value, out: &mut Vec<Node<'a>>) {
    if out.len() >= MAX_PARTS || out.iter().any(|part| part.pointer == node.pointer) {
        return;
    }
    let all_of = node.child("allOf");
    out.push(node);
    let Some(all_of) = all_of else {
        return;
    };
    let count = all_of.value.as_array().map_or(0, Vec::len);
    for member in (0..count).filter_map(|i| all_of.item(i)) {
        collect(member.resolved(doc), doc, out);
    }
}

/// The schema of property `name`, from the first part that declares it.
pub fn property<'a>(parts: &[Node<'a>], name: &str) -> Option<Node<'a>> {
    parts
        .iter()
        .find_map(|part| part.child("properties")?.child(name))
}

/// What the `allOf` members say besides the flattened object: each
/// member inlined without documentation and without [`OBJECT_KEYS`] (and a
/// boolean `additionalProperties`), the empty ones dropped.
pub fn residual(parts: &[Node<'_>], doc: &Value) -> Vec<Value> {
    parts
        .iter()
        .skip(1)
        .filter_map(|part| {
            let mut value = inline(doc, part.value, true);
            let map = value.as_object_mut()?;
            for key in OBJECT_KEYS {
                map.remove(key);
            }
            if map
                .get("additionalProperties")
                .is_some_and(Value::is_boolean)
            {
                map.remove("additionalProperties");
            }
            (!map.is_empty()).then_some(value)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parts_follow_nested_all_ofs_and_references() {
        let doc = json!({
            "s": {"allOf": [{"$ref": "#/b"}, {"allOf": [{"properties": {"c": {}}}]}]},
            "b": {"properties": {"b": {}}, "allOf": [{"$ref": "#/s"}]}
        });
        let root = Node::at(&doc, "/s").unwrap();
        let pointers: Vec<String> = parts(&root, &doc).into_iter().map(|p| p.pointer).collect();
        assert_eq!(pointers, ["/s", "/b", "/s/allOf/1", "/s/allOf/1/allOf/0"]);
        let parts = parts(&root, &doc);
        assert_eq!(
            property(&parts, "c").unwrap().pointer,
            "/s/allOf/1/allOf/0/properties/c"
        );
        assert!(property(&parts, "z").is_none());
    }

    #[test]
    fn the_residual_keeps_what_the_object_does_not_cover() {
        let doc = json!({"s": {"allOf": [
            {"type": "object", "properties": {"a": {}}, "additionalProperties": false},
            {"maxProperties": 3, "description": "d"}
        ]}});
        let root = Node::at(&doc, "/s").unwrap();
        assert_eq!(
            residual(&parts(&root, &doc), &doc),
            [json!({"maxProperties": 3})]
        );
    }
}
