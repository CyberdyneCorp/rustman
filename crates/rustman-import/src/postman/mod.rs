//! Postman Collection v2.0/v2.1 and environment exports (tasks 2.1-2.4).
//!
//! Exports are read as JSON values rather than typed structs: Postman's
//! exporters disagree on details (auth parameters as a list or an object,
//! URLs as a string or an object, descriptions as a string or an object),
//! and every variant is handled in one small helper.

mod auth;
mod env;
mod request;

use serde_json::Value;

pub use env::parse_environment;

use crate::error::ImportError;
use crate::ir::{Folder, ImportedCollection, Item, Location, Note, NoteKind, Script, Variable};

/// The `info.schema` path segments of the supported collection versions.
const SCHEMAS: &[&str] = &["/json/collection/v2.0.", "/json/collection/v2.1."];

/// Whether `doc` is a Postman v2.0 or v2.1 collection.
pub fn is_collection(doc: &Value) -> bool {
    doc["info"]["schema"]
        .as_str()
        .is_some_and(|s| s.contains("postman.com") && SCHEMAS.iter().any(|v| s.contains(v)))
}

/// Whether `doc` is a Postman environment export.
pub fn is_environment(doc: &Value) -> bool {
    doc["_postman_variable_scope"] == "environment"
        || (doc["values"].is_array() && doc["name"].is_string() && doc["info"].is_null())
}

/// Parse the collection export `text`, read from `file`.
pub fn parse_collection(text: &str, file: &str) -> Result<ImportedCollection, ImportError> {
    let doc: Value =
        serde_json::from_str(text).map_err(|e| ImportError::invalid(file, e.to_string()))?;
    if !is_collection(&doc) {
        return Err(ImportError::invalid(
            file,
            "not a Postman v2.0 or v2.1 collection (info.schema)",
        ));
    }
    let mut ctx = Context {
        file: file.to_owned(),
        notes: Vec::new(),
    };
    let name = string(&doc["info"]["name"]).unwrap_or_else(|| "collection".to_owned());
    let mut root = ctx.folder(&doc, "");
    root.name.clone_from(&name);
    root.description = description(&doc["info"]["description"]);
    Ok(ImportedCollection {
        name,
        root,
        environments: Vec::new(),
        notes: ctx.notes,
    })
}

/// The file being read and the notes made so far.
pub(crate) struct Context {
    pub file: String,
    pub notes: Vec<Note>,
}

impl Context {
    pub fn location(&self, item: &str) -> Location {
        Location::item(&self.file, item)
    }

    pub fn note(&mut self, kind: NoteKind, message: impl Into<String>, at: &Location) {
        self.notes.push(Note::new(kind, message, at));
    }

    /// A folder (or the collection itself) at item path `path`.
    fn folder(&mut self, v: &Value, path: &str) -> Folder {
        let at = self.location(path);
        Folder {
            name: string(&v["name"]).unwrap_or_default(),
            description: description(&v["description"]),
            auth: auth::parse(&v["auth"], &at, self),
            headers: Vec::new(),
            variables: if path.is_empty() {
                self.variables(&v["variable"], &at)
            } else {
                self.folder_variables(&v["variable"], &at)
            },
            scripts: scripts(&v["event"], &at),
            items: self.items(&v["item"], path),
            location: at,
        }
    }

    fn items(&mut self, items: &Value, parent: &str) -> Vec<Item> {
        let mut out = Vec::new();
        for item in items.as_array().into_iter().flatten() {
            let name = string(&item["name"]).unwrap_or_else(|| "untitled".to_owned());
            let path = if parent.is_empty() {
                name.clone()
            } else {
                format!("{parent} / {name}")
            };
            if item["item"].is_array() {
                out.push(Item::Folder(Box::new(self.folder(item, &path))));
            } else {
                let mut request = request::parse(item, &path, self);
                request.name = name;
                out.push(Item::Request(Box::new(request)));
            }
        }
        out
    }

    /// Folder `variable` lists: the v2.1 schema allows them, but Postman
    /// never resolves them (requests in the folder see the collection's
    /// value), so they are dropped, each with a note, rather than given a
    /// meaning they do not have in Postman. Values are never shown.
    fn folder_variables(&mut self, vars: &Value, at: &Location) -> Vec<Variable> {
        for var in vars.as_array().into_iter().flatten() {
            let name = string(&var["key"])
                .or_else(|| string(&var["id"]))
                .unwrap_or_default();
            if !name.is_empty() {
                let message = format!(
                    "folder variable {name} dropped: Postman ignores folder-level variables"
                );
                self.note(NoteKind::Dropped, message, at);
            }
        }
        Vec::new()
    }

    /// Collection, folder or request variables; disabled ones are dropped.
    pub fn variables(&mut self, vars: &Value, at: &Location) -> Vec<Variable> {
        let mut out = Vec::new();
        for var in vars.as_array().into_iter().flatten() {
            let name = string(&var["key"])
                .or_else(|| string(&var["id"]))
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            if var["disabled"] == true {
                self.note(
                    NoteKind::Dropped,
                    format!("disabled variable {name} dropped"),
                    at,
                );
                continue;
            }
            out.push(Variable {
                name,
                value: string(&var["value"]).unwrap_or_default(),
                secret: var["type"] == "secret",
                path: false,
            });
        }
        out
    }
}

/// The scripts of an item's `event` list.
fn scripts(events: &Value, at: &Location) -> Vec<Script> {
    let mut out = Vec::new();
    for event in events.as_array().into_iter().flatten() {
        let source = match &event["script"]["exec"] {
            Value::Array(lines) => lines
                .iter()
                .map(|l| l.as_str().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n"),
            other => other.as_str().unwrap_or_default().to_owned(),
        };
        if source.trim().is_empty() {
            continue;
        }
        out.push(Script {
            event: string(&event["listen"]).unwrap_or_else(|| "script".to_owned()),
            source,
            location: at.clone(),
        });
    }
    out
}

/// A string, number or boolean as text; `None` for anything else.
pub(crate) fn string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// A description: a string, or an object with `content`.
pub(crate) fn description(v: &Value) -> Option<String> {
    string(v)
        .or_else(|| string(&v["content"]))
        .filter(|d| !d.trim().is_empty())
}
