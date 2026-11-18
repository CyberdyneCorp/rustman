//! Bruno collections (task 3.2): a folder with `bruno.json`, `.bru` request
//! files in nested folders ordered by `meta.seq`, optional `collection.bru`
//! and `folder.bru` settings, and `environments/*.bru`.

mod bru;
mod request;

use std::path::Path;

use serde_json::Value;

pub use bru::{Block, Content, Entry};

use crate::error::ImportError;
use crate::ir::{Folder, ImportedCollection, ImportedEnvironment, Item, Location, Note, NoteKind};
use crate::postman::string;
use crate::source::Reader;
use request::Bru;

/// The file that marks a Bruno collection folder.
pub const MARKER: &str = "bruno.json";

/// The deepest folder nesting read below a collection, a guard against
/// filesystem loops that symlink detection cannot see (such as junctions).
pub const MAX_DEPTH: usize = 32;

/// Parse the `.bru` text of `file` into its blocks.
pub fn parse_blocks(text: &str, file: &str) -> Result<Vec<Block>, ImportError> {
    bru::parse(text, file)
}

/// Whether `text` looks like a `.bru` request file.
pub fn is_bru(text: &str) -> bool {
    let mut lines = text.lines().map(str::trim_end);
    let has_meta = lines.clone().any(|l| l == "meta {");
    has_meta
        && lines.any(|l| l.ends_with(" {") && !l.starts_with(char::is_whitespace))
        && bru::parse(text, "").is_ok()
}

/// A single `.bru` request file as a one-request collection.
pub fn parse_file(text: &str, file: &str) -> Result<ImportedCollection, ImportError> {
    let blocks = bru::parse(text, file)?;
    let mut bru = Bru {
        blocks: &blocks,
        file,
        notes: Vec::new(),
    };
    let stem = file_stem(file);
    let name = bru.name(&stem);
    let request = bru
        .request(name.clone())
        .ok_or_else(|| ImportError::invalid(file, "no request block (get, post, ...)"))?;
    Ok(ImportedCollection {
        name: name.clone(),
        root: Folder {
            name,
            items: vec![Item::Request(Box::new(request))],
            location: Location::item(file, ""),
            ..Folder::default()
        },
        environments: Vec::new(),
        notes: bru.notes,
    })
}

fn file_stem(file: &str) -> String {
    Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Parse the Bruno collection folder `dir`, shown as `shown`.
pub fn parse_dir(
    dir: &Path,
    shown: &str,
    reader: &mut Reader,
) -> Result<ImportedCollection, ImportError> {
    let config_path = dir.join(MARKER);
    let config: Value = serde_json::from_str(&reader.read(&config_path)?)
        .map_err(|e| ImportError::invalid(&format!("{shown}/{MARKER}"), e.to_string()))?;
    let name = string(&config["name"]).unwrap_or_else(|| file_stem(shown));
    let mut walker = Walker {
        reader,
        shown,
        notes: Vec::new(),
    };
    let (mut root, _) = walker.folder(dir, "", "collection.bru")?;
    root.name.clone_from(&name);
    let environments = walker.environments(&dir.join("environments"))?;
    Ok(ImportedCollection {
        name,
        root,
        environments,
        notes: walker.notes,
    })
}

struct Walker<'r> {
    reader: &'r mut Reader,
    shown: &'r str,
    notes: Vec<Note>,
}

/// An item with its `meta.seq` and file name, for ordering.
struct Ordered {
    seq: Option<u32>,
    file_name: String,
    item: Item,
}

impl Walker<'_> {
    /// The path of `rel` as shown in locations: `<src>/<rel>`.
    fn shown(&self, rel: &str) -> String {
        if rel.is_empty() {
            self.shown.to_owned()
        } else {
            format!("{}/{rel}", self.shown)
        }
    }

    /// The folder `dir` (`rel` below the collection), with its settings
    /// file `settings` (`collection.bru` or `folder.bru`) when present, and
    /// the settings' `meta.seq`.
    fn folder(
        &mut self,
        dir: &Path,
        rel: &str,
        settings: &str,
    ) -> Result<(Folder, Option<u32>), ImportError> {
        if rel.split('/').count() > MAX_DEPTH {
            let message = format!("folders nested deeper than {MAX_DEPTH} levels");
            return Err(ImportError::invalid(&self.shown(rel), message));
        }
        let mut folder = Folder {
            name: file_stem(rel),
            location: Location::item(&self.shown(rel), ""),
            ..Folder::default()
        };
        let mut seq = None;
        let settings_path = dir.join(settings);
        if settings_path.is_file() {
            let file = self.shown(&join(rel, settings));
            let blocks = bru::parse(&self.reader.read(&settings_path)?, &file)?;
            let mut bru = Bru {
                blocks: &blocks,
                file: &file,
                notes: Vec::new(),
            };
            apply_settings(&mut folder, &mut bru);
            seq = bru.seq();
            self.notes.append(&mut bru.notes);
        }
        let mut items = self.items(dir, rel)?;
        items.sort_by(|a, b| {
            (a.seq.unwrap_or(u32::MAX), &a.file_name)
                .cmp(&(b.seq.unwrap_or(u32::MAX), &b.file_name))
        });
        folder.items = items.into_iter().map(|o| o.item).collect();
        Ok((folder, seq))
    }

    fn items(&mut self, dir: &Path, rel: &str) -> Result<Vec<Ordered>, ImportError> {
        let mut out = Vec::new();
        for (file_name, path) in sorted_entries(dir)? {
            let child_rel = join(rel, &file_name);
            if is_symlinked_dir(&path) {
                let file = self.shown(&child_rel);
                let message = format!("symlinked folder {file} skipped");
                self.notes.push(Note::new(
                    NoteKind::Dropped,
                    message,
                    &Location::item(&file, ""),
                ));
            } else if path.is_dir() {
                if skip_dir(rel, &file_name) {
                    continue;
                }
                let (folder, seq) = self.folder(&path, &child_rel, "folder.bru")?;
                if !folder.items.is_empty() {
                    out.push(Ordered {
                        seq,
                        file_name,
                        item: Item::Folder(Box::new(folder)),
                    });
                }
            } else if file_name.ends_with(".bru") && !is_settings(&file_name) {
                out.extend(self.request(&path, &child_rel, file_name)?);
            }
        }
        Ok(out)
    }

    fn request(
        &mut self,
        path: &Path,
        rel: &str,
        file_name: String,
    ) -> Result<Option<Ordered>, ImportError> {
        let file = self.shown(rel);
        let blocks = bru::parse(&self.reader.read(path)?, &file)?;
        let mut bru = Bru {
            blocks: &blocks,
            file: &file,
            notes: Vec::new(),
        };
        let name = bru.name(&file_stem(&file_name));
        let seq = bru.seq();
        let request = bru.request(name);
        self.notes.append(&mut bru.notes);
        Ok(request.map(|r| Ordered {
            seq,
            file_name,
            item: Item::Request(Box::new(r)),
        }))
    }

    /// `environments/*.bru`, in file-name order.
    fn environments(&mut self, dir: &Path) -> Result<Vec<ImportedEnvironment>, ImportError> {
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for (file_name, path) in sorted_entries(dir)? {
            if !file_name.ends_with(".bru") || !path.is_file() {
                continue;
            }
            let file = self.shown(&format!("environments/{file_name}"));
            let blocks = bru::parse(&self.reader.read(&path)?, &file)?;
            let mut bru = Bru {
                blocks: &blocks,
                file: &file,
                notes: Vec::new(),
            };
            let variables = request::environment_variables(&mut bru);
            self.notes.append(&mut bru.notes);
            out.push(ImportedEnvironment {
                name: file_stem(&file_name),
                variables,
                location: Location::item(&file, ""),
            });
        }
        Ok(out)
    }
}

/// `collection.bru` / `folder.bru` settings shared with the folder's requests.
fn apply_settings(folder: &mut Folder, bru: &mut Bru<'_>) {
    folder.name = bru.name(&folder.name);
    folder.description = bru.docs();
    folder.headers = bru.headers();
    folder.variables = bru.variables("vars:pre-request");
    folder.scripts = bru.scripts();
    let mode = bru
        .blocks
        .iter()
        .find(|b| b.name == "auth")
        .and_then(|b| b.get("mode"))
        .unwrap_or("inherit")
        .to_owned();
    folder.auth = bru.auth(&mode);
}

/// A symbolic link to a folder: never followed, so a link back up the tree
/// cannot make the walk loop.
fn is_symlinked_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) && path.is_dir()
}

fn is_settings(file_name: &str) -> bool {
    matches!(file_name, "collection.bru" | "folder.bru")
}

/// Folders that are not part of the request tree.
fn skip_dir(rel: &str, name: &str) -> bool {
    (rel.is_empty() && name == "environments") || name.starts_with('.') || name == "node_modules"
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

/// The entries of `dir` by file name, sorted, so the walk is deterministic.
pub(crate) fn sorted_entries(dir: &Path) -> Result<Vec<(String, std::path::PathBuf)>, ImportError> {
    let io = |e| ImportError::io(dir, e);
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(io)? {
        let entry = entry.map_err(io)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        out.push((name, entry.path()));
    }
    out.sort();
    Ok(out)
}
