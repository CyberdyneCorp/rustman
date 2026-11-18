//! `docs/agent.md` matches the code: the documented `[agent]` defaults are
//! the defaults, the error code table lists the catalog, and the tool
//! reference is the one generated from `tools/list`.

use std::path::Path;

use rustman_core::agent::AgentSettings;
use rustman_core::settings::Settings;
use rustman_mcp::server::ErrorCode;
use rustman_mcp::server::tools::reference;

fn doc_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/agent.md")
}

fn doc() -> String {
    std::fs::read_to_string(doc_path()).unwrap()
}

/// The ```toml block of the doc that starts with `[agent]`.
fn agent_block(doc: &str) -> String {
    let start = doc.find("```toml\n[agent]").expect("an [agent] block");
    let body = &doc[start + "```toml\n".len()..];
    body[..body.find("```").unwrap()].to_owned()
}

#[test]
fn the_documented_agent_table_is_the_default() {
    let text = format!(
        "[workspace]\nname = \"t\"\nversion = 1\n{}",
        agent_block(&doc())
    );
    let (settings, warnings) = Settings::parse(Path::new("rustman.toml"), &text).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(settings.agent, AgentSettings::default());
}

#[test]
fn every_error_code_is_documented() {
    let doc = doc();
    for code in ErrorCode::ALL {
        let row = format!("| `{}` |", code.as_str());
        assert!(doc.contains(&row), "missing {row}");
    }
}

#[test]
fn every_tool_is_documented() {
    let doc = doc();
    for tool in rustman_mcp::server::tools::catalog() {
        let row = format!("| `{}` |", tool.name);
        assert!(doc.contains(&row), "missing {row}");
    }
}

#[test]
fn the_tool_reference_matches_the_catalog() {
    let doc = doc();
    let expected = reference::splice(&doc, &reference::markdown()).expect("tool-reference markers");
    if std::env::var_os("RUSTMAN_BLESS_DOCS").is_some() {
        std::fs::write(doc_path(), &expected).unwrap();
        return;
    }
    assert!(
        doc == expected,
        "docs/agent.md's tool reference is out of date; run \
         `RUSTMAN_BLESS_DOCS=1 cargo test -p rustman-mcp --test docs_agent` and commit the result"
    );
}
