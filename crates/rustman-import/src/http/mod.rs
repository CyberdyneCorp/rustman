//! `.http` and `.rest` files (task 4.2): a file, or a folder of them, with
//! the environment files found next to them.

mod env;
mod parse;

use std::path::Path;

use crate::error::ImportError;
use crate::ir::{Folder, ImportedCollection, ImportedEnvironment, Item, Location, Note};
use crate::source::Reader;

/// Whether `text` looks like an HTTP file: not JSON, with at least one
/// request line such as `GET https://...`.
pub fn is_http(text: &str) -> bool {
    let trimmed = text.trim_start();
    !trimmed.starts_with('{')
        && !trimmed.starts_with('[')
        && text.lines().any(|l| parse::is_request_line(l.trim()))
}

/// Whether `name` is an HTTP file name.
fn is_http_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".http") || lower.ends_with(".rest")
}

/// Whether the folder `dir` holds `.http` or `.rest` files.
pub fn has_http_files(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| is_http_name(&e.file_name().to_string_lossy()) && e.path().is_file())
    })
}

fn stem(shown: &str) -> String {
    Path::new(shown)
        .file_stem()
        .map_or_else(|| "http".to_owned(), |s| s.to_string_lossy().into_owned())
}

/// The requests of `text`, read from `file`, as a collection (no
/// environments).
pub fn parse_file(text: &str, file: &str) -> Result<ImportedCollection, ImportError> {
    let (root, notes) = folder(text, file, &stem(file));
    Ok(collection(root, Vec::new(), notes))
}

/// [`parse_file`] for the file at `path`, plus the environment files in its
/// folder.
pub fn parse_file_with_environments(
    text: &str,
    path: &Path,
    shown: &str,
    reader: &mut Reader,
) -> Result<ImportedCollection, ImportError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let (root, mut notes) = folder(text, shown, &stem(shown));
    let envs = env::environments(dir, reader, &mut notes)?;
    Ok(collection(root, envs, notes))
}

/// Every `.http` / `.rest` file of `dir` (one folder each, by file name)
/// and the environment files there.
pub fn parse_dir(
    dir: &Path,
    shown: &str,
    reader: &mut Reader,
) -> Result<ImportedCollection, ImportError> {
    let mut items = Vec::new();
    let mut notes = Vec::new();
    for (name, path) in crate::bruno::sorted_entries(dir)? {
        if !is_http_name(&name) || !path.is_file() {
            continue;
        }
        let file = format!("{shown}/{name}");
        let text = reader.read(&path)?;
        let (folder, mut file_notes) = folder(&text, &file, &stem(&name));
        notes.append(&mut file_notes);
        items.push(Item::Folder(Box::new(folder)));
    }
    let envs = env::environments(dir, reader, &mut notes)?;
    let name = dir
        .canonicalize()
        .ok()
        .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| stem(shown));
    let root = Folder {
        name,
        items,
        location: Location::item(shown, ""),
        ..Folder::default()
    };
    Ok(collection(root, envs, notes))
}

/// The folder of one file's requests (its file variables become the
/// folder's variables), and the notes made reading it.
fn folder(text: &str, file: &str, name: &str) -> (Folder, Vec<Note>) {
    let parsed = parse::parse(text, file);
    let folder = Folder {
        name: name.to_owned(),
        variables: parsed.variables,
        items: parsed
            .requests
            .into_iter()
            .map(|r| Item::Request(Box::new(r)))
            .collect(),
        location: Location::item(file, ""),
        ..Folder::default()
    };
    (folder, parsed.notes)
}

fn collection(
    root: Folder,
    environments: Vec<ImportedEnvironment>,
    notes: Vec<Note>,
) -> ImportedCollection {
    ImportedCollection {
        name: root.name.clone(),
        root,
        environments,
        notes,
    }
}
