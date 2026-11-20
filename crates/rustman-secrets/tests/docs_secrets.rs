//! Every TOML snippet of `docs/secrets.md` parses and builds providers
//! (add-secret-providers task 2.3): workspace snippets as `rustman.toml`,
//! snippets headed `# ~/.config/rustman/config.toml` as the user config.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::context;
use rustman_core::secrets::provider::config::USER_CONFIG_FILE;
use rustman_core::settings::Settings;
use rustman_secrets::build_from;

const USER_HEADER: &str = "# ~/.config/rustman/config.toml";

fn docs() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/secrets.md");
    std::fs::read_to_string(path).expect("docs/secrets.md")
}

/// The contents of every ```toml block, in order.
fn toml_blocks(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in markdown.lines() {
        match (&mut current, line.trim_end()) {
            (None, "```toml") => current = Some(String::new()),
            (Some(block), "```") => {
                blocks.push(std::mem::take(block));
                current = None;
            }
            (Some(block), text) => {
                block.push_str(text);
                block.push('\n');
            }
            (None, _) => {}
        }
    }
    blocks
}

fn user_tables(text: &str) -> BTreeMap<String, toml::Table> {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("rustman");
    std::fs::create_dir(&config).unwrap();
    std::fs::write(config.join(USER_CONFIG_FILE), text).unwrap();
    rustman_core::secrets::provider::config::load_user(Some(&config)).unwrap()
}

#[test]
fn every_documented_provider_snippet_parses_and_builds() {
    let blocks = toml_blocks(&docs());
    assert!(blocks.len() >= 12, "found {} snippets", blocks.len());
    let ws = tempfile::tempdir().unwrap();
    let ctx = context(ws.path(), &[("RUSTMAN_SECRET_STORE", "file")]);
    // A workspace `op` provider for the user snippet that overrides it.
    let shared_op = Settings::parse(
        Path::new("rustman.toml"),
        "[workspace]\nname = \"d\"\nversion = 1\n[secret_providers.op]\ntype = \"onepassword\"\n",
    )
    .unwrap()
    .0
    .secret_providers;
    let mut types = Vec::new();
    for block in &blocks {
        let providers = if block.starts_with(USER_HEADER) {
            build_from(&shared_op, &user_tables(block), &ctx)
        } else {
            let text = format!("[workspace]\nname = \"d\"\nversion = 1\n\n{block}");
            let (settings, warnings) = Settings::parse(Path::new("rustman.toml"), &text)
                .unwrap_or_else(|e| panic!("{e}\n{block}"));
            assert!(warnings.is_empty(), "{warnings:?}\n{block}");
            build_from(&settings.secret_providers, &BTreeMap::new(), &ctx)
        };
        let providers = providers.unwrap_or_else(|e| panic!("{e}\n{block}"));
        types.extend(providers.infos().into_iter().map(|i| i.kind.clone()));
    }
    for kind in [
        "keychain",
        "env",
        "sops",
        "onepassword",
        "vault",
        "cmd",
        "aws-secrets-manager",
        "aws-ssm",
        "gcp-secret-manager",
        "azure-key-vault",
    ] {
        assert!(types.iter().any(|t| t == kind), "no snippet for {kind}");
    }
}
