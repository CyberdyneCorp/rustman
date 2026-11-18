//! The keymap reference in `docs/tui.md`, generated from the registry and
//! the built-in layers. A test fails when the doc drifts; run it with
//! `RUSTMAN_BLESS_DOCS=1` to rewrite the generated block.

use std::fmt::Write;

use super::actions::REGISTRY;
use super::{Builtin, Context, DEFAULT, VIM, chord};
use crate::vim;

/// The line that opens the generated block in `docs/tui.md`.
pub const START: &str =
    "<!-- keymap:start: generated from the action registry, do not edit by hand -->";
/// The line that closes it.
pub const END: &str = "<!-- keymap:end -->";

/// The keys bound to `id` in `table`, as `` `Ctrl+R` `` (global) or
/// `` `Up` (sidebar, response) ``.
fn keys(table: &[Builtin], id: &str) -> String {
    let mut grouped: Vec<(String, Vec<Context>)> = Vec::new();
    for (context, text, _) in table.iter().filter(|(_, _, a)| a.id() == id) {
        let keys = chord::display(&chord::parse(text).expect("built-in bindings parse"));
        match grouped.iter_mut().find(|(k, _)| *k == keys) {
            Some((_, contexts)) => contexts.push(*context),
            None => grouped.push((keys, vec![*context])),
        }
    }
    let parts: Vec<String> = grouped
        .into_iter()
        .map(|(keys, contexts)| {
            if contexts == [Context::Global] {
                return format!("`{keys}`");
            }
            let names: Vec<&str> = contexts.iter().map(|c| c.name()).collect();
            format!("`{keys}` ({})", names.join(", "))
        })
        .collect();
    parts.join(", ")
}

/// The generated Markdown between [`START`] and [`END`] (both excluded).
pub fn markdown() -> String {
    let mut out = String::new();
    out.push_str("| Action | Id | Default keys | Vim preset adds |\n|---|---|---|---|\n");
    for def in REGISTRY {
        let _ = writeln!(
            out,
            "| {} | `{}` | {} | {} |",
            def.title,
            def.id,
            keys(DEFAULT, def.id),
            keys(VIM, def.id)
        );
    }
    out.push_str("\nIn vim mode, text areas (the request body) are modal:\n\n");
    out.push_str("| Keys | What they do |\n|---|---|\n");
    for (keys, what) in vim::KEYS {
        let keys: Vec<String> = keys.split(' ').map(|k| format!("`{k}`")).collect();
        let _ = writeln!(out, "| {} | {what} |", keys.join(" "));
    }
    out
}

/// Replace the generated block in `doc` with `generated`; `None` when the
/// markers are missing.
pub fn splice(doc: &str, generated: &str) -> Option<String> {
    let start = doc.find(START)? + START.len();
    let end = start + doc[start..].find(END)?;
    Some(format!("{}\n\n{generated}\n{}", &doc[..start], &doc[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn doc_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/tui.md")
    }

    #[test]
    fn docs_tui_md_keymap_matches_the_registry() {
        let path = doc_path();
        let doc = std::fs::read_to_string(&path).expect("docs/tui.md");
        let expected = splice(&doc, &markdown()).expect("keymap markers in docs/tui.md");
        if std::env::var_os("RUSTMAN_BLESS_DOCS").is_some() {
            std::fs::write(&path, &expected).expect("write docs/tui.md");
            return;
        }
        assert!(
            doc == expected,
            "docs/tui.md's keymap table is out of date; run \
             `RUSTMAN_BLESS_DOCS=1 cargo test -p rustman-tui docs_tui_md` and commit the result"
        );
    }

    #[test]
    fn every_action_is_documented() {
        let table = markdown();
        for def in REGISTRY {
            assert!(table.contains(&format!("`{}`", def.id)), "{}", def.id);
        }
        assert!(table.contains("| Send request | `request.send` | `Ctrl+Enter`, `Ctrl+R` |"));
        assert!(table.contains("`v` (response)"));
        assert!(table.contains("`gg` (sidebar, response)"));
    }

    #[test]
    fn splicing_replaces_only_the_block() {
        let doc = format!("before\n{START}\nold\n{END}\nafter\n");
        let spliced = splice(&doc, "new\n").unwrap();
        assert_eq!(spliced, format!("before\n{START}\n\nnew\n\n{END}\nafter\n"));
        assert_eq!(splice(&spliced, "new\n").unwrap(), spliced, "idempotent");
        assert!(splice("no markers", "x").is_none());
    }
}
