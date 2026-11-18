//! Reference resolution: one self-contained document out of a root document
//! and the files (or, with `--allow-remote-refs`, URLs) it references.
//!
//! References into the root's `components` stay as they are. Every other
//! target (a referenced file, a URL, or a part of the root outside
//! `components`) is copied once into `components/x-rustman-external/<name>`
//! and the reference is rewritten to point there. A circular reference
//! therefore stays a reference and is never expanded. The 3.0 schema rewrite
//! runs in the same pass, so copied schemas are rewritten too.

use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::{Map, Value};

use super::document::{self, SpecVersion};
use super::error::ImportError;
use super::normalize;
use super::pointer;
use super::source::{self, FetchOptions, Location};
use super::walk::{Mode, walk};

/// Where copied reference targets are kept, under `components`.
pub const EXTERNAL: &str = "x-rustman-external";

/// The most documents one import reads, the root included.
pub const MAX_DOCUMENTS: usize = 64;

#[derive(Clone, Debug)]
pub struct BundleOptions {
    pub allow_remote_refs: bool,
    pub version: SpecVersion,
    pub fetch: FetchOptions,
}

/// The self-contained, 3.1-shaped form of `doc`, read from `root`.
pub async fn bundle(
    root: &Location,
    mut doc: Value,
    options: &BundleOptions,
) -> Result<Value, ImportError> {
    let root = root.canonical();
    let mut bundler = Bundler::new(root.clone(), doc.clone(), options);
    bundler.rewrite(&mut doc, &root, Mode::Doc, "")?;
    bundler.drain().await?;
    if !bundler.hoisted.is_empty() {
        insert_external(&mut doc, bundler.hoisted)?;
    }
    Ok(doc)
}

fn insert_external(doc: &mut Value, hoisted: Map<String, Value>) -> Result<(), ImportError> {
    let not_object = || ImportError::Reference("the document root is not an object".into());
    let root = doc.as_object_mut().ok_or_else(not_object)?;
    let components = root
        .entry("components")
        .or_insert_with(|| Value::Object(Map::new()));
    let components = components
        .as_object_mut()
        .ok_or_else(|| ImportError::Reference("'components' is not an object".into()))?;
    components.insert(EXTERNAL.to_owned(), Value::Object(hoisted));
    Ok(())
}

/// A reference target waiting to be copied.
struct Pending {
    doc: Location,
    pointer: String,
    name: String,
    mode: Mode,
    /// The reference as written and where, for errors.
    reference: String,
    at: String,
}

struct Bundler<'a> {
    root: Location,
    options: &'a BundleOptions,
    /// Every document read, as read.
    docs: HashMap<Location, Value>,
    names: HashMap<(Location, String), String>,
    used: HashSet<String>,
    queue: VecDeque<Pending>,
    hoisted: Map<String, Value>,
}

impl<'a> Bundler<'a> {
    fn new(root: Location, doc: Value, options: &'a BundleOptions) -> Self {
        Self {
            docs: HashMap::from([(root.clone(), doc)]),
            root,
            options,
            names: HashMap::new(),
            used: HashSet::new(),
            queue: VecDeque::new(),
            hoisted: Map::new(),
        }
    }

    /// Rewrite the references in `value`, a part of the document `base`.
    fn rewrite(
        &mut self,
        value: &mut Value,
        base: &Location,
        mode: Mode,
        at: &str,
    ) -> Result<(), ImportError> {
        walk(value, mode, at, &mut |node, mode, at| {
            self.visit(node, mode, at, base)
        })
    }

    fn visit(
        &mut self,
        node: &mut Value,
        mode: Mode,
        at: &str,
        base: &Location,
    ) -> Result<(), ImportError> {
        if let Some(Value::String(reference)) = node.get("$ref") {
            let rewritten = self.rewrite_ref(&reference.clone(), base, mode, at)?;
            node["$ref"] = Value::String(rewritten);
        }
        if mode == Mode::Schema && self.options.version == SpecVersion::V3_0 {
            normalize::schema_3_0(node);
        }
        Ok(())
    }

    fn rewrite_ref(
        &mut self,
        reference: &str,
        base: &Location,
        mode: Mode,
        at: &str,
    ) -> Result<String, ImportError> {
        let (doc_part, fragment) = reference.split_once('#').unwrap_or((reference, ""));
        let target = pointer::from_fragment(fragment).ok_or_else(|| {
            ImportError::Reference(format!(
                "unsupported reference '{reference}' at {at}: only JSON pointer fragments are supported"
            ))
        })?;
        let doc = if doc_part.is_empty() {
            base.clone()
        } else {
            base.join(doc_part)?.canonical()
        };
        if doc.is_remote() && doc != self.root && !self.options.allow_remote_refs {
            return Err(ImportError::RemoteRef {
                reference: reference.to_owned(),
                at: pointer::display(at).to_owned(),
            });
        }
        if !self.may_read(&doc) {
            return Err(ImportError::Reference(format!(
                "reference '{reference}' at {} leaves the document's directory",
                pointer::display(at)
            )));
        }
        if doc == self.root && target.starts_with("/components/") {
            return Ok(if *base == self.root {
                reference.to_owned()
            } else {
                format!("#{target}")
            });
        }
        let name = self.hoist(doc, target, mode, reference, at);
        Ok(format!(
            "#/components/{EXTERNAL}/{}",
            pointer::escape(&name)
        ))
    }

    /// Whether a reference may read `doc`: a URL (checked above), or a file
    /// beside a local root, in its directory or below. An absolute path, or
    /// one that climbs out with `..` (or a symlink), could read any file.
    fn may_read(&self, doc: &Location) -> bool {
        let Location::File(path) = doc else {
            return true;
        };
        match &self.root {
            Location::File(root) => root.parent().is_some_and(|dir| path.starts_with(dir)),
            Location::Url(_) => false,
        }
    }

    /// The name the target is copied under, queueing the copy the first time.
    fn hoist(
        &mut self,
        doc: Location,
        target: String,
        mode: Mode,
        reference: &str,
        at: &str,
    ) -> String {
        let key = (doc, target);
        if let Some(name) = self.names.get(&key) {
            return name.clone();
        }
        let name = self.unique_name(&key.0, &key.1);
        self.names.insert(key.clone(), name.clone());
        let (doc, pointer) = key;
        self.queue.push_back(Pending {
            doc,
            pointer,
            name: name.clone(),
            mode,
            reference: reference.to_owned(),
            at: pointer::display(at).to_owned(),
        });
        name
    }

    fn unique_name(&mut self, doc: &Location, target: &str) -> String {
        let raw = format!("{}{target}", doc.file_name());
        let base: String = raw
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let mut name = base.clone();
        let mut n = 1;
        while !self.used.insert(name.clone()) {
            n += 1;
            name = format!("{base}_{n}");
        }
        name
    }

    /// Copy every queued target, rewriting its own references in turn.
    async fn drain(&mut self) -> Result<(), ImportError> {
        while let Some(pending) = self.queue.pop_front() {
            self.load(&pending.doc).await?;
            let mut target = self.docs[&pending.doc]
                .pointer(&pending.pointer)
                .cloned()
                .ok_or_else(|| {
                    ImportError::Reference(format!(
                        "unresolved reference '{}' at {}",
                        pending.reference, pending.at
                    ))
                })?;
            let at = format!("{}#{}", pending.doc, pending.pointer);
            self.rewrite(&mut target, &pending.doc, pending.mode, &at)?;
            self.hoisted.insert(pending.name, target);
        }
        Ok(())
    }

    async fn load(&mut self, doc: &Location) -> Result<(), ImportError> {
        if self.docs.contains_key(doc) {
            return Ok(());
        }
        if self.docs.len() >= MAX_DOCUMENTS {
            return Err(ImportError::Reference(format!(
                "too many referenced documents (at most {MAX_DOCUMENTS})"
            )));
        }
        let text = source::read(doc, &self.options.fetch).await?;
        let (_, value) = document::parse(&text, &doc.to_string())?;
        self.docs.insert(doc.clone(), value);
        Ok(())
    }
}
