//! Variable syntax translation (task 1.3): every source's references become
//! Rustman `{{name}}` placeholders and dynamic variables.

use rustman_core::vars::dynamic::Dynamic;

use crate::ir::{Folder, ImportedCollection, Item, Location, Note, NoteKind, VisitStrings};

/// Source dynamic variables with a Rustman equivalent.
const DYNAMIC: &[(&str, &str)] = &[
    ("$guid", "$uuid"),
    ("$randomUUID", "$uuid"),
    ("$timestamp", "$timestamp"),
    ("$randomInt", "$randomInt"),
];

/// Translate every placeholder of `collection` in place, noting each
/// dynamic variable that has no Rustman equivalent.
pub fn translate(collection: &mut ImportedCollection) {
    let mut notes = Vec::new();
    translate_folder(&mut collection.root, &mut notes);
    for env in &mut collection.environments {
        let mut unmapped = Vec::new();
        env.variables
            .visit_strings(&mut |s| translate_in_place(s, &mut unmapped));
        push_unmapped(&mut notes, unmapped, &env.location);
    }
    collection.notes.extend(notes);
}

fn translate_folder(folder: &mut Folder, notes: &mut Vec<Note>) {
    let mut unmapped = Vec::new();
    folder
        .auth
        .visit_strings(&mut |s| translate_in_place(s, &mut unmapped));
    folder
        .headers
        .visit_strings(&mut |s| translate_in_place(s, &mut unmapped));
    folder
        .variables
        .visit_strings(&mut |s| translate_in_place(s, &mut unmapped));
    push_unmapped(notes, unmapped, &folder.location);
    for item in &mut folder.items {
        match item {
            Item::Folder(child) => translate_folder(child, notes),
            Item::Request(request) => {
                let mut unmapped = Vec::new();
                request.visit_strings(&mut |s| translate_in_place(s, &mut unmapped));
                push_unmapped(notes, unmapped, &request.location);
            }
        }
    }
}

fn push_unmapped(notes: &mut Vec<Note>, mut names: Vec<String>, at: &Location) {
    names.dedup();
    for name in names {
        let message = format!("unmapped dynamic variable {name}");
        notes.push(Note::new(NoteKind::Unmapped, message, at));
    }
}

fn translate_in_place(text: &mut String, unmapped: &mut Vec<String>) {
    if text.contains("{{") {
        *text = translate_text(text, unmapped);
    }
}

/// `text` with each `{{ ... }}` translated; unmappable dynamic variables are
/// kept as written and their names added to `unmapped` (once each).
pub fn translate_text(text: &str, unmapped: &mut Vec<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let Some(len) = rest[start + 2..].find("}}") else {
            break;
        };
        out.push_str(&rest[..start]);
        let inner = &rest[start + 2..start + 2 + len];
        out.push_str(&placeholder(inner, unmapped));
        rest = &rest[start + 4 + len..];
    }
    out.push_str(rest);
    out
}

/// The Rustman form of the placeholder whose inside is `inner`.
fn placeholder(inner: &str, unmapped: &mut Vec<String>) -> String {
    let trimmed = inner.trim();
    if let Some(name) = trimmed.strip_prefix("_.") {
        return format!("{{{{{}}}}}", name.trim());
    }
    if !trimmed.starts_with('$') {
        return format!("{{{{{inner}}}}}");
    }
    let head = trimmed.split_whitespace().next().unwrap_or_default();
    if let Some((_, to)) = DYNAMIC.iter().find(|(from, _)| *from == head) {
        return format!("{{{{{to}}}}}");
    }
    if Dynamic::parse(trimmed).is_err() && !unmapped.iter().any(|n| n == head) {
        unmapped.push(head.to_owned());
    }
    format!("{{{{{inner}}}}}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::ImportedRequest;

    fn tr(text: &str) -> (String, Vec<String>) {
        let mut unmapped = Vec::new();
        (translate_text(text, &mut unmapped), unmapped)
    }

    #[test]
    fn plain_variables_stay_as_they_are() {
        assert_eq!(tr("{{base_url}}/users").0, "{{base_url}}/users");
        assert_eq!(tr("{{ spaced }}").0, "{{ spaced }}");
    }

    #[test]
    fn insomnia_underscore_variables_lose_the_prefix() {
        assert_eq!(tr("{{ _.base_url }}/users").0, "{{base_url}}/users");
        assert_eq!(tr("{{_.a}}-{{_.b}}").0, "{{a}}-{{b}}");
    }

    #[test]
    fn every_mapping_row_of_the_spec() {
        assert_eq!(tr("{{$guid}}").0, "{{$uuid}}");
        assert_eq!(tr("{{$randomUUID}}").0, "{{$uuid}}");
        assert_eq!(tr("{{$timestamp}}").0, "{{$timestamp}}");
        assert_eq!(tr("{{$randomInt}}").0, "{{$randomInt}}");
        // Rustman's own dynamic variables need no mapping.
        assert_eq!(
            tr("{{$isoTimestamp}}"),
            ("{{$isoTimestamp}}".into(), vec![])
        );
    }

    #[test]
    fn unmappable_dynamic_variables_are_kept_and_listed_once() {
        let (text, unmapped) = tr("{\"city\": \"{{$randomCity}}\", \"b\": \"{{$randomCity}}\"}");
        assert_eq!(
            text,
            "{\"city\": \"{{$randomCity}}\", \"b\": \"{{$randomCity}}\"}"
        );
        assert_eq!(unmapped, ["$randomCity"]);
    }

    #[test]
    fn unclosed_braces_are_left_alone() {
        assert_eq!(tr("a {{b").0, "a {{b");
    }

    #[test]
    fn the_stage_notes_unmapped_variables_with_the_item_path() {
        let request = ImportedRequest {
            name: "Create".into(),
            body: Some(crate::ir::ImportedBody::Json {
                text: "{\"c\": \"{{$randomCity}}\"}".into(),
            }),
            location: Location::item("c.json", "Users / Create"),
            ..ImportedRequest::default()
        };
        let mut collection = ImportedCollection {
            root: Folder {
                items: vec![Item::Request(Box::new(request))],
                ..Folder::default()
            },
            ..ImportedCollection::default()
        };
        translate(&mut collection);
        assert_eq!(collection.notes.len(), 1);
        let note = &collection.notes[0];
        assert_eq!(note.message, "unmapped dynamic variable $randomCity");
        assert_eq!(note.location.item.as_deref(), Some("Users / Create"));
    }
}
