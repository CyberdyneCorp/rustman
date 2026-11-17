//! Every example in docs/request-files.md loads (task 2.8).

use std::fs;
use std::path::Path;

use rustman_core::Workspace;
use rustman_core::collection::CollectionMeta;
use rustman_core::format::from_yaml;
use rustman_core::format::write::{collection_to_yaml, request_to_yaml};
use rustman_core::model::{Body, KindRegistry, Request};
use rustman_core::settings::Settings;

struct Example {
    lang: String,
    path: String,
    body: String,
}

fn examples() -> Vec<Example> {
    let doc = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/request-files.md");
    let text = fs::read_to_string(doc).unwrap();
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let Some(lang) = line
            .strip_prefix("```")
            .filter(|l| *l == "yaml" || *l == "toml")
        else {
            continue;
        };
        let block: Vec<&str> = lines.by_ref().take_while(|l| *l != "```").collect();
        let path = block[0]
            .strip_prefix("# ")
            .expect("example starts with '# <path>'");
        out.push(Example {
            lang: lang.to_owned(),
            path: path.to_owned(),
            body: block[1..].iter().map(|l| format!("{l}\n")).collect(),
        });
    }
    out
}

#[test]
fn every_example_is_canonical() {
    let registry = KindRegistry::default();
    let mut counts = (0, 0, 0);
    for ex in examples() {
        let file = Path::new(&ex.path);
        if ex.path.ends_with(".rman.yaml") {
            let request = registry
                .load(file, &ex.body)
                .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(
                request_to_yaml(&request),
                ex.body,
                "{} is not canonical",
                ex.path
            );
            counts.0 += 1;
        } else if ex.path.ends_with("_collection.yaml") {
            let meta: CollectionMeta = from_yaml(file, &ex.body).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(
                collection_to_yaml(&meta),
                ex.body,
                "{} is not canonical",
                ex.path
            );
            counts.1 += 1;
        } else {
            assert_eq!(
                (ex.lang.as_str(), ex.path.as_str()),
                ("toml", "rustman.toml")
            );
            let (_, warnings) = Settings::parse(file, &ex.body).unwrap();
            assert!(warnings.is_empty(), "{warnings:?}");
            counts.2 += 1;
        }
    }
    assert!(
        counts.0 >= 10 && counts.1 == 1 && counts.2 == 1,
        "{counts:?}"
    );
}

#[test]
fn there_is_one_example_per_body_type() {
    let registry = KindRegistry::default();
    let mut seen = Vec::new();
    for ex in examples().iter().filter(|e| e.path.ends_with(".rman.yaml")) {
        if let Request::Http(http) = registry.load(Path::new(&ex.path), &ex.body).unwrap() {
            let kind = match http.body {
                Some(Body::Json(_)) => "json",
                Some(Body::Text(_)) => "text",
                Some(Body::Form(_)) => "form",
                Some(Body::Multipart(_)) => "multipart",
                Some(Body::File(_)) => "file",
                Some(Body::None) => "none",
                None => continue,
            };
            seen.push(kind);
        }
    }
    for kind in rustman_core::model::body::BODY_TYPES {
        assert!(seen.contains(kind), "no example for body type '{kind}'");
    }
}

#[test]
fn the_examples_form_a_loadable_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let mut requests = 0;
    for ex in examples() {
        let path = tmp.path().join(&ex.path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, &ex.body).unwrap();
        requests += usize::from(ex.path.ends_with(".rman.yaml"));
    }
    let ws = Workspace::load(tmp.path()).unwrap_or_else(|e| panic!("{e}"));
    assert!(ws.warnings.is_empty(), "{:?}", ws.warnings);
    assert_eq!(ws.requests().len(), requests);
    let users = ws
        .requests()
        .into_iter()
        .filter(|r| r.entry.id().starts_with("users/"));
    let order: Vec<String> = users.map(|r| r.entry.stem()).collect();
    assert_eq!(order[..2], ["login".to_owned(), "get-user".to_owned()]);
    let get_user = &ws.find_by_name("Get user")[0];
    assert!(
        get_user
            .effective_headers()
            .iter()
            .any(|h| h.name == "X-Team")
    );
    assert!(get_user.effective_auth().is_some());
}
