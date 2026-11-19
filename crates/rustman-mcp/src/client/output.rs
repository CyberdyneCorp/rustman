//! Human-readable views of MCP results, shared by `rustman mcp-client` and
//! the TUI's result view: one block per tool, resource, prompt or content
//! item. Values arrive redacted from the session. [`printable`] makes text
//! a server controls safe to write to a terminal.

use std::borrow::Cow;

use rmcp::model::{Prompt, Resource, Tool};

use super::prompts::PromptResult;
use super::resources::ResourceContent;
use super::tools::ToolResult;

/// `text` with every control character other than newline and tab written
/// escaped (`\u{1b}`), so text a server controls cannot send escape
/// sequences to the terminal.
pub fn printable(text: &str) -> Cow<'_, str> {
    if !text.chars().any(is_unsafe) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if is_unsafe(c) {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

fn is_unsafe(c: char) -> bool {
    c.is_control() && c != '\n' && c != '\t'
}

/// `search  Search files [read-only]` and the description below it.
pub fn tools(tools: &[Tool]) -> String {
    if tools.is_empty() {
        return "no tools\n".to_owned();
    }
    let mut out = String::new();
    for tool in tools {
        out.push_str(&tool.name);
        if let Some(title) = tool.title.as_deref().filter(|t| *t != tool.name) {
            out.push_str("  ");
            out.push_str(title);
        }
        let hints = hints(tool);
        if !hints.is_empty() {
            out.push_str(&format!(" [{}]", hints.join(", ")));
        }
        out.push('\n');
        if let Some(description) = tool.description.as_deref() {
            push_indented(&mut out, description);
        }
    }
    out
}

/// The annotations a tool declares, as short words.
pub fn hints(tool: &Tool) -> Vec<&'static str> {
    let Some(a) = &tool.annotations else {
        return Vec::new();
    };
    [
        (a.read_only_hint, "read-only"),
        (a.destructive_hint, "destructive"),
        (a.idempotent_hint, "idempotent"),
        (a.open_world_hint, "open-world"),
    ]
    .into_iter()
    .filter(|(flag, _)| *flag == Some(true))
    .map(|(_, word)| word)
    .collect()
}

/// Each content item, `structuredContent` as JSON, `tool error` first for
/// an `isError` result, and output-schema violations as warnings.
pub fn tool_result(result: &ToolResult) -> String {
    let mut out = String::new();
    if result.is_error {
        out.push_str("tool error\n");
    }
    for item in &result.content {
        out.push_str(&item.summary());
        out.push('\n');
    }
    if let Some(structured) = &result.structured {
        let json = serde_json::to_string_pretty(structured).unwrap_or_default();
        out.push_str(&format!("structuredContent:\n{json}\n"));
    }
    for violation in &result.output_violations {
        out.push_str(&format!("warning: outputSchema: {violation}\n"));
    }
    out
}

/// `file:///README.md  README  text/markdown`.
pub fn resources(resources: &[Resource]) -> String {
    if resources.is_empty() {
        return "no resources\n".to_owned();
    }
    let mut out = String::new();
    for resource in resources {
        out.push_str(&resource.uri);
        if resource.name != resource.uri {
            out.push_str("  ");
            out.push_str(&resource.name);
        }
        if let Some(mime) = &resource.mime_type {
            out.push_str("  ");
            out.push_str(mime);
        }
        out.push('\n');
    }
    out
}

/// The text of each content, or its blob's type and size.
pub fn contents(contents: &[ResourceContent]) -> String {
    let mut out = String::new();
    for content in contents {
        let mime = content.mime_type.as_deref().unwrap_or("unknown type");
        match (&content.text, &content.blob) {
            (Some(text), _) => {
                out.push_str(text);
                if !text.ends_with('\n') {
                    out.push('\n');
                }
            }
            (None, Some(blob)) => {
                let bytes = blob.len() * 3 / 4;
                out.push_str(&format!("[blob {} {mime}, ~{bytes} bytes]\n", content.uri));
            }
            (None, None) => out.push_str(&format!("[empty {}]\n", content.uri)),
        }
    }
    out
}

/// `review  Review code  (code*, style)`: `*` marks required arguments.
pub fn prompts(prompts: &[Prompt]) -> String {
    if prompts.is_empty() {
        return "no prompts\n".to_owned();
    }
    let mut out = String::new();
    for prompt in prompts {
        out.push_str(&prompt.name);
        let arguments: Vec<String> = prompt
            .arguments
            .iter()
            .flatten()
            .map(|a| {
                let star = if a.required == Some(true) { "*" } else { "" };
                format!("{}{star}", a.name)
            })
            .collect();
        if !arguments.is_empty() {
            out.push_str(&format!("  ({})", arguments.join(", ")));
        }
        out.push('\n');
        if let Some(description) = prompt.description.as_deref() {
            push_indented(&mut out, description);
        }
    }
    out
}

/// `user: <content>`, one message per line.
pub fn prompt(result: &PromptResult) -> String {
    let mut out = String::new();
    if let Some(description) = &result.description {
        out.push_str(&format!("{description}\n"));
    }
    for line in &result.messages {
        out.push_str(&format!("{}: {}\n", line.role, line.content.summary()));
    }
    out
}

fn push_indented(out: &mut String, text: &str) {
    for line in text.lines() {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::prompts::PromptLine;
    use super::super::tools::ContentItem;
    use super::*;

    #[test]
    fn tools_show_their_hints_and_descriptions() {
        let tool: Tool = serde_json::from_value(json!({
            "name": "search",
            "description": "Find files",
            "inputSchema": {"type": "object"},
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        }))
        .unwrap();
        assert_eq!(tools(&[tool]), "search [read-only]\n    Find files\n");
        assert_eq!(tools(&[]), "no tools\n");
    }

    #[test]
    fn tool_errors_and_violations_are_shown() {
        let result = ToolResult {
            tool: "read".into(),
            content: vec![ContentItem::Text {
                text: "no such file".into(),
            }],
            structured: Some(json!({"count": "3"})),
            is_error: true,
            output_violations: vec![super::super::schema::Violation::new(
                "/count",
                "expected integer",
            )],
            overridden: Vec::new(),
        };
        let text = tool_result(&result);
        assert!(
            text.starts_with("tool error\nno such file\nstructuredContent:\n"),
            "{text}"
        );
        assert!(
            text.ends_with("warning: outputSchema: /count: expected integer\n"),
            "{text}"
        );
    }

    #[test]
    fn prompts_mark_required_arguments() {
        let prompt_list: Vec<Prompt> = serde_json::from_value(json!([{
            "name": "review",
            "arguments": [{"name": "code", "required": true}, {"name": "style"}]
        }]))
        .unwrap();
        assert_eq!(prompts(&prompt_list), "review  (code*, style)\n");
        let got = PromptResult {
            description: None,
            messages: vec![PromptLine {
                role: "user".into(),
                content: ContentItem::Text { text: "hi".into() },
            }],
        };
        assert_eq!(prompt(&got), "user: hi\n");
    }

    #[test]
    fn blobs_show_their_size() {
        let blob = ResourceContent {
            uri: "file:///logo.png".into(),
            mime_type: Some("image/png".into()),
            text: None,
            blob: Some("AAAA".into()),
        };
        assert_eq!(
            contents(&[blob]),
            "[blob file:///logo.png image/png, ~3 bytes]\n"
        );
    }
}
