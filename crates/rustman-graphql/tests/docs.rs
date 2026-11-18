//! The examples in docs/graphql.md load.

use std::path::Path;

use rustman_core::model::{KindRegistry, Request};
use rustman_core::settings::Settings;

/// Every fenced block of `lang` in docs/graphql.md, without its first
/// (`# <path>`) line.
fn blocks(lang: &str) -> Vec<String> {
    let doc = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/graphql.md");
    let text = std::fs::read_to_string(doc).unwrap();
    let fence = format!("```{lang}");
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line == fence {
            let block: Vec<&str> = lines.by_ref().take_while(|l| *l != "```").collect();
            out.push(block[1..].iter().map(|l| format!("{l}\n")).collect());
        }
    }
    out
}

#[test]
fn the_request_examples_load() {
    let yaml = blocks("yaml");
    assert_eq!(yaml.len(), 4);
    let load = |path: &str, text: &str| {
        let request = KindRegistry::default()
            .load(Path::new(path), text)
            .unwrap_or_else(|e| panic!("{e}"));
        let Request::Graphql(graphql) = request else {
            panic!("a graphql request");
        };
        graphql
    };
    let get_user = load("users/get-user.rman.yaml", &yaml[0]);
    assert_eq!(get_user.operation_name.as_deref(), Some("GetUser"));
    let upload = load("media/upload-avatar.rman.yaml", &yaml[1]);
    assert_eq!(upload.variables.unwrap()["file"]["$file"], "./avatar.png");
    let live = load("live/on-message.rman.yaml", &yaml[2]);
    assert_eq!(live.effective_max_events(), 5);
    assert_eq!(live.assertions.unwrap().len(), 2);
    let login = load("auth/login.rman.yaml", &yaml[3]);
    assert_eq!(login.captures.unwrap()[0].var, "token");
}

#[test]
fn the_settings_example_parses_without_warnings() {
    for toml in blocks("toml") {
        let (settings, warnings) = Settings::parse(Path::new("rustman.toml"), &toml).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(settings.graphql.endpoints.len(), 1);
    }
}
