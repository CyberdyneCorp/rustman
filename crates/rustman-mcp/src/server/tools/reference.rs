//! The tool reference in `docs/agent.md`, generated from the catalog's
//! names, descriptions, annotations and input schemas. A test fails when
//! the doc drifts; run it with `RUSTMAN_BLESS_DOCS=1` to rewrite the block.

use std::fmt::Write;

use rmcp::model::Tool;
use serde_json::Value;

use super::catalog;

/// The line that opens the generated block in `docs/agent.md`.
pub const START: &str =
    "<!-- tool-reference:start: generated from the tool schemas, do not edit by hand -->";
/// The line that closes it.
pub const END: &str = "<!-- tool-reference:end -->";

/// The arguments of `tool`: required ones in bold, then optional ones, in
/// schema order.
fn arguments(tool: &Tool) -> String {
    let Some(Value::Object(properties)) = tool.input_schema.get("properties") else {
        return "none".to_owned();
    };
    if properties.is_empty() {
        return "none".to_owned();
    }
    let required: Vec<&str> = tool
        .input_schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let (mut first, mut rest) = (Vec::new(), Vec::new());
    for name in properties.keys() {
        if required.contains(&name.as_str()) {
            first.push(format!("**`{name}`**"));
        } else {
            rest.push(format!("`{name}`"));
        }
    }
    first.extend(rest);
    first.join(", ")
}

fn read_only(tool: &Tool) -> &'static str {
    let read_only = tool
        .annotations
        .as_ref()
        .and_then(|a| a.read_only_hint)
        .unwrap_or(false);
    if read_only { "yes" } else { "no" }
}

/// The generated Markdown between [`START`] and [`END`] (both excluded).
pub fn markdown() -> String {
    let mut out = String::from(
        "| Tool | Arguments (**required**) | Read-only | Description |\n|------|------|------|------|\n",
    );
    for tool in catalog() {
        let description = tool.description.as_deref().unwrap_or_default();
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} |",
            tool.name,
            arguments(&tool),
            read_only(&tool),
            description.replace('|', "\\|")
        );
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

    #[test]
    fn arguments_list_required_ones_first_in_bold() {
        let tool = catalog()
            .into_iter()
            .find(|t| t.name == "import_openapi")
            .unwrap();
        assert_eq!(arguments(&tool), "**`spec`**, `into`, `dry_run`");
        let none = catalog()
            .into_iter()
            .find(|t| t.name == "list_environments")
            .unwrap();
        assert_eq!(arguments(&none), "none");
        assert_eq!(read_only(&none), "yes");
    }

    #[test]
    fn splicing_replaces_only_the_block() {
        let doc = format!("a\n{START}\n\nold\n{END}\nb\n");
        assert_eq!(
            splice(&doc, "new\n").unwrap(),
            format!("a\n{START}\n\nnew\n\n{END}\nb\n")
        );
        assert!(splice("no markers", "x").is_none());
    }
}
