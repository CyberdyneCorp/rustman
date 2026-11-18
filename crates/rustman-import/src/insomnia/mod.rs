//! Insomnia v4 JSON and v5 YAML exports (task 3.1).
//!
//! A v4 export is a flat list of resources linked by `parentId`; a v5
//! export is already a tree. The v4 list is first rebuilt into the v5 shape
//! (`children` lists, sorted by sort key), so one walker reads both.

mod env;
mod request;

use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::error::ImportError;
use crate::ir::{
    Folder, ImportedCollection, Item, Location, Note, NoteKind, Variable, VisitStrings,
};
use crate::postman::{description, string};

/// The `type` of a v5 export.
const V5_TYPE: &str = "collection.insomnia.rest/5";

/// Whether `doc` is an Insomnia v4 JSON export.
pub fn is_v4(doc: &Value) -> bool {
    doc["_type"] == "export" && doc["__export_format"] == 4
}

/// Whether `text` looks like an Insomnia v5 YAML export (a top-level
/// `type: collection.insomnia.rest/5.x` line).
pub fn is_v5(text: &str) -> bool {
    text.lines().any(|line| {
        line.strip_prefix("type:")
            .is_some_and(|t| t.trim().trim_matches(['"', '\'']).starts_with(V5_TYPE))
    })
}

/// Parse an Insomnia v4 (JSON) or v5 (YAML) export `text`, read from `file`.
pub fn parse(text: &str, file: &str) -> Result<ImportedCollection, ImportError> {
    let doc = match serde_json::from_str::<Value>(text) {
        Ok(doc) if is_v4(&doc) => return Ok(parse_v4(&doc, file)),
        Ok(_) => return Err(ImportError::invalid(file, "not an Insomnia v4 export")),
        Err(_) => rustman_core::format::from_yaml::<Value>(Path::new(file), text)
            .map_err(|e| ImportError::invalid(file, e.to_string()))?,
    };
    let is_v5_doc = doc["type"].as_str().is_some_and(|t| t.starts_with(V5_TYPE));
    if !is_v5_doc {
        return Err(ImportError::invalid(
            file,
            "not an Insomnia v4 JSON or v5 YAML export",
        ));
    }
    Ok(parse_v5(&doc, file))
}

fn parse_v4(doc: &Value, file: &str) -> ImportedCollection {
    let resources: Vec<&Value> = doc["resources"].as_array().into_iter().flatten().collect();
    let workspace = resources.iter().find(|r| r["_type"] == "workspace");
    let root_id = workspace
        .and_then(|w| string(&w["_id"]))
        .unwrap_or_default();
    let name = workspace
        .and_then(|w| string(&w["name"]))
        .unwrap_or_else(|| "Insomnia".to_owned());
    let tree = V4Tree::new(&resources);
    let mut root = tree.node(&root_id);
    root["name"] = Value::String(name.clone());
    if let Some(w) = workspace {
        root["description"] = w["description"].clone();
    }
    let envs = env::from_v4(&resources, &root_id);
    finish(&root, envs, name, file)
}

fn parse_v5(doc: &Value, file: &str) -> ImportedCollection {
    let name = string(&doc["name"]).unwrap_or_else(|| "Insomnia".to_owned());
    let mut root = serde_json::json!({ "name": name, "children": doc["collection"] });
    root["description"] = doc["meta"]["description"].clone();
    let envs = env::from_v5(&doc["environments"]);
    finish(&root, envs, name, file)
}

fn finish(root: &Value, envs: env::EnvSet, name: String, file: &str) -> ImportedCollection {
    let mut ctx = Context {
        file: file.to_owned(),
        notes: Vec::new(),
    };
    let mut folder = ctx.folder(root, "");
    folder.name.clone_from(&name);
    let environments = envs.flatten(&ctx.location(""));
    let mut collection = ImportedCollection {
        name,
        root: folder,
        environments,
        notes: ctx.notes,
    };
    note_template_tags(&mut collection);
    collection
}

/// The resources of a v4 export by parent, rebuilt as a v5-shaped tree.
struct V4Tree<'a> {
    children: HashMap<String, Vec<&'a Value>>,
}

impl<'a> V4Tree<'a> {
    fn new(resources: &[&'a Value]) -> Self {
        let mut children: HashMap<String, Vec<&'a Value>> = HashMap::new();
        for r in resources {
            if matches!(r["_type"].as_str(), Some("request_group" | "request"))
                || is_other_request(r)
            {
                let parent = string(&r["parentId"]).unwrap_or_default();
                children.entry(parent).or_default().push(r);
            }
        }
        Self { children }
    }

    /// The node with id `id`, its children nested under `children`.
    fn node(&self, id: &str) -> Value {
        let mut kids: Vec<&Value> = self.children.get(id).cloned().unwrap_or_default();
        kids.sort_by(|a, b| sort_key(a).total_cmp(&sort_key(b)));
        let list = kids
            .into_iter()
            .map(|kid| {
                let mut node = kid.clone();
                if kid["_type"] == "request_group" {
                    let kid_id = string(&kid["_id"]).unwrap_or_default();
                    node["children"] = self.node(&kid_id)["children"].take();
                }
                node
            })
            .collect();
        serde_json::json!({ "children": Value::Array(list) })
    }
}

/// The sort key of a v4 resource or v5 node (lower first).
fn sort_key(v: &Value) -> f64 {
    v["metaSortKey"]
        .as_f64()
        .or_else(|| v["meta"]["sortKey"].as_f64())
        .unwrap_or(0.0)
}

/// A gRPC or WebSocket request, which has no Rustman equivalent here.
fn is_other_request(v: &Value) -> bool {
    let id = string(&v["_id"])
        .or_else(|| string(&v["meta"]["id"]))
        .unwrap_or_default();
    matches!(
        v["_type"].as_str(),
        Some("grpc_request" | "websocket_request")
    ) || id.starts_with("greq_")
        || id.starts_with("ws-req_")
}

pub(crate) struct Context {
    pub file: String,
    pub notes: Vec<Note>,
}

impl Context {
    fn location(&self, item: &str) -> Location {
        Location::item(&self.file, item)
    }

    fn note(&mut self, kind: NoteKind, message: impl Into<String>, at: &Location) {
        self.notes.push(Note::new(kind, message, at));
    }

    /// A request group (or the workspace) at item path `path`.
    fn folder(&mut self, v: &Value, path: &str) -> Folder {
        let at = self.location(path);
        let headers = request::pairs(&v["headers"], "header", &at, &mut self.notes);
        Folder {
            name: string(&v["name"]).unwrap_or_default(),
            description: description(&v["description"]),
            auth: request::auth(&v["authentication"], &at, &mut self.notes),
            headers,
            variables: object_variables(&v["environment"]),
            scripts: request::scripts(v, &at),
            items: self.items(&v["children"], path),
            location: at,
        }
    }

    fn items(&mut self, children: &Value, parent: &str) -> Vec<Item> {
        let mut out = Vec::new();
        for child in children.as_array().into_iter().flatten() {
            let name = string(&child["name"]).unwrap_or_else(|| "untitled".to_owned());
            let path = if parent.is_empty() {
                name.clone()
            } else {
                format!("{parent} / {name}")
            };
            if child["children"].is_array() || child["_type"] == "request_group" {
                out.push(Item::Folder(Box::new(self.folder(child, &path))));
            } else if is_other_request(child) {
                let at = self.location(&path);
                self.note(
                    NoteKind::Unsupported,
                    "gRPC/WebSocket request unsupported",
                    &at,
                );
            } else {
                let mut req = request::parse(child, &path, self);
                req.name = name;
                out.push(Item::Request(Box::new(req)));
            }
        }
        out
    }
}

/// The variables of an object such as `{"host": "x", "port": 80}`.
pub(crate) fn object_variables(v: &Value) -> Vec<Variable> {
    v.as_object()
        .into_iter()
        .flatten()
        .map(|(name, value)| Variable {
            name: name.clone(),
            value: crate::common::value_text(value),
            secret: false,
        })
        .collect()
}

static TEMPLATE_TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{%-?\s*([A-Za-z_][\w.]*)").expect("the template-tag pattern is valid")
});

/// Note each Insomnia template tag (`{% response ... %}`) once per place:
/// only plain variables have a Rustman equivalent.
fn note_template_tags(collection: &mut ImportedCollection) {
    let mut notes = Vec::new();
    folder_tags(&mut collection.root, &mut notes);
    for env in &mut collection.environments {
        let at = env.location.clone();
        tags_in(&mut env.variables, &at, &mut notes);
    }
    collection.notes.extend(notes);
}

fn folder_tags(folder: &mut Folder, notes: &mut Vec<Note>) {
    let at = folder.location.clone();
    let mut own = Folder {
        items: Vec::new(),
        ..folder.clone()
    };
    tags_in(&mut own, &at, notes);
    for item in &mut folder.items {
        match item {
            Item::Folder(child) => folder_tags(child, notes),
            Item::Request(request) => {
                let at = request.location.clone();
                tags_in(request.as_mut(), &at, notes);
            }
        }
    }
}

fn tags_in(v: &mut dyn VisitStrings, at: &Location, notes: &mut Vec<Note>) {
    let mut names: Vec<String> = Vec::new();
    v.visit_strings(&mut |s| {
        for m in TEMPLATE_TAG.captures_iter(s) {
            if !names.iter().any(|n| n == &m[1]) {
                names.push(m[1].to_owned());
            }
        }
    });
    for name in names {
        let message = format!("unsupported template tag: {name}");
        notes.push(Note::new(NoteKind::Unsupported, message, at));
    }
}
