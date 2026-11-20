//! A fixture workspace for docs builds: two collections (`users`, `auth`)
//! and five visible requests, plus a hidden one.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rustman_core::history::{Entry, HistoryStore};
use rustman_core::settings::HistorySettings;
use rustman_core::{Secret, Workspace};
use rustman_docs::{BuildOptions, BuildReport, DocsError, build};
use serde_json::{Value, json};
use tempfile::TempDir;

/// The secret value of `token` in environment `local`.
pub const TOKEN: &str = "CANARY-tok-9f2c71";

pub fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

pub fn request(name: &str, method: &str, url: &str, extra: &str) -> String {
    format!("version: 1\nkind: http\nname: {name}\nmethod: {method}\nurl: '{url}'\n{extra}")
}

/// A workspace with `rustman.toml` holding `docs_settings` under `[docs]`.
pub fn workspace(docs_settings: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        &format!("[workspace]\nname = \"Shop\"\nversion = 1\n\n[docs]\n{docs_settings}"),
    );
    write(
        root,
        "environments/local.yaml",
        "variables:\n  base_url: http://localhost:8080\nsecrets:\n  - token\n",
    );
    write(
        root,
        "users/_collection.yaml",
        "name: Users\ndescription: User **accounts**. See [the guide](https://guide.example/users).\n",
    );
    write(
        root,
        "users/create.rman.yaml",
        &request(
            "Create user",
            "POST",
            "{{base_url}}/users",
            "description: Creates a user.\nheaders:\n  - name: Content-Type\n    value: application/json\n  - name: Authorization\n    value: Bearer {{token}}\nbody:\n  type: json\n  content:\n    name: Ada\n    role: admin\nassertions:\n  - status: 201\ncaptures:\n  - var: user_id\n    from: json\n    path: $.id\n",
        ),
    );
    write(
        root,
        "users/get.rman.yaml",
        &request(
            "Get user",
            "GET",
            "{{base_url}}/users/{{user_id}}",
            "query:\n  - name: expand\n    value: roles\n",
        ),
    );
    write(
        root,
        "users/delete.rman.yaml",
        &request(
            "Delete user",
            "DELETE",
            "{{base_url}}/users/{{user_id}}",
            &format!("headers:\n  - name: X-Api-Key\n    value: {TOKEN}-literal\n"),
        ),
    );
    write(
        root,
        "users/debug.rman.yaml",
        &request(
            "Debug dump",
            "GET",
            "{{base_url}}/debug",
            "docs:\n  hidden: true\n",
        ),
    );
    write(root, "auth/_collection.yaml", "order: [login, list]\n");
    write(
        root,
        "auth/list.rman.yaml",
        &request("List sessions", "GET", "{{base_url}}/sessions", ""),
    );
    write(
        root,
        "auth/login.rman.yaml",
        &request(
            "Login",
            "POST",
            "{{base_url}}/login",
            "auth:\n  type: basic\n  username: ada\n  password: '{{token}}'\n",
        ),
    );
    dir
}

pub fn load(root: &Path) -> Workspace {
    Workspace::load(root).unwrap()
}

pub fn secrets() -> Vec<Secret<String>> {
    vec![Secret::new("token", TOKEN.to_owned())]
}

/// Build `root` into `root/site` with `options` adjusted by `adjust`.
pub fn build_with(
    root: &Path,
    adjust: impl FnOnce(&mut BuildOptions),
) -> Result<BuildReport, DocsError> {
    let mut options = BuildOptions::new(root.join("site"));
    options.shown_out = "site/".into();
    adjust(&mut options);
    build(&load(root), &secrets(), &options)
}

pub fn build_site(root: &Path) -> BuildReport {
    build_with(root, |_| {}).unwrap()
}

/// Every file under `dir`, by `/`-separated relative path.
pub fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    collect(dir, dir, &mut out);
    out
}

fn collect(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path: PathBuf = entry.unwrap().path();
        if path.is_dir() {
            collect(base, &path, out);
        } else {
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel, fs::read(&path).unwrap());
        }
    }
}

pub fn read(root: &Path, rel: &str) -> String {
    fs::read_to_string(root.join(rel)).unwrap()
}

/// `html` without tags (each one a space), whitespace runs collapsed:
/// roughly what a reader sees.
pub fn visible_text(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Record an exchange of `request` (a request file path) in the history.
pub fn record(root: &Path, id: &str, request: &str, status: u16, body: &str, headers: Value) {
    let entry: Entry = serde_json::from_value(json!({
        "schema": "rustman.history/v1",
        "id": id,
        "timestamp": "2026-11-17T09:30:00.123Z",
        "request": request,
        "environment": "local",
        "sent": {"method": "POST", "url": "http://localhost:8080/users", "headers": [], "body": null},
        "status": status,
        "total_ms": 4.0,
        "error": null,
        "spill": null,
        "record": {"exchange": {"response": {"status": status, "headers": headers, "body": body}}},
        "replay": null,
    }))
    .unwrap();
    let store = HistoryStore::for_workspace(root, &HistorySettings::default());
    store.record(&entry).unwrap();
}
