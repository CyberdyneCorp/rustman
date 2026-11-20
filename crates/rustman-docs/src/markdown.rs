//! The Markdown renderer (api-docs-generation "Markdown output", design
//! D8): GitHub-flavored Markdown with tables and fenced code, one file per
//! section, endpoint and reference page, and `README.md` as the index.
//!
//! It reads the same page model as the HTML renderer, so redaction,
//! ordering and determinism are the same. User text is escaped: Markdown
//! punctuation with backslashes and `<`, `>`, `&` as entities, so no text
//! becomes a link, an image or raw HTML. Descriptions are inserted as the
//! sanitized HTML the HTML site shows. Long bodies go in `<details>`.

use crate::commonmark::Html;
use crate::html::escape_html;
use crate::model::{
    DocSite, Endpoint, Example, ReferencePage, ReferenceSet, Row, Schema, Section, Table,
};
use crate::output::Files;
use crate::rendered::Rendered;

/// Bodies with more lines than this are folded in a `<details>` block.
pub const LONG_BODY_LINES: usize = 20;

/// Every Markdown file of `site`.
pub fn render(site: &DocSite) -> Files {
    let mut files = Files::new();
    files.insert("README.md".to_owned(), index(site).finish());
    for section in site.all_sections() {
        let name = format!("sections/{}.md", section.slug);
        files.insert(name, section_page(section).finish());
    }
    for endpoint in site.endpoints() {
        let name = format!("endpoints/{}.md", endpoint.slug);
        files.insert(name, endpoint_page(endpoint).finish());
    }
    for set in &site.references {
        for page in &set.pages {
            let name = format!("reference/{}.md", page.slug);
            files.insert(name, reference_page(set, page).finish());
        }
    }
    files
}

fn index(site: &DocSite) -> Doc {
    let mut doc = Doc::default();
    doc.heading(1, &inline(&site.title));
    if !site.variables.is_empty() {
        doc.heading(2, "Variables");
        doc.rows(&["Name", "Value"], &site.variables);
    }
    if !site.sections.is_empty() {
        doc.heading(2, "Sections");
        for section in site.all_sections() {
            let indent = "  ".repeat(section.depth.saturating_sub(1));
            let link = link(&section.title, &format!("sections/{}.md", section.slug));
            doc.line(&format!("{indent}- {link}"));
        }
        doc.blank();
    }
    for set in &site.references {
        doc.heading(2, &inline(&set.title));
        doc.para(&code(&set.endpoint));
        for page in &set.pages {
            let link = link(&page.name, &format!("reference/{}.md", page.slug));
            doc.line(&format!("- {link} *{}*", inline(&page.label)));
        }
        doc.blank();
    }
    doc
}

fn section_page(section: &Section) -> Doc {
    let mut doc = Doc::default();
    doc.heading(1, &inline(&section.title));
    doc.para("[Index](../README.md)");
    doc.html(section.description.as_ref());
    if !section.endpoints.is_empty() {
        doc.heading(2, "Endpoints");
        for e in &section.endpoints {
            doc.line(&format!("- {}", endpoint_item(e)));
        }
        doc.blank();
    }
    if !section.sections.is_empty() {
        doc.heading(2, "Subsections");
        for sub in &section.sections {
            doc.line(&format!(
                "- {}",
                link(&sub.title, &format!("{}.md", sub.slug))
            ));
        }
        doc.blank();
    }
    doc
}

/// `[POST Create user](../endpoints/users-create.md) `{{base_url}}/users``.
fn endpoint_item(e: &Endpoint) -> String {
    let label = match &e.method {
        Some(method) => format!("{} {}", inline(method), inline(&e.name)),
        None => inline(&e.name),
    };
    let mut item = format!("[{label}](../endpoints/{}.md)", e.slug);
    if let Some(url) = &e.url {
        item.push_str(&format!(" {}", code(url)));
    }
    if e.deprecated {
        item.push_str(" **deprecated**");
    }
    item
}

fn endpoint_page(e: &Endpoint) -> Doc {
    let mut doc = Doc::default();
    let badge = if e.deprecated { " **deprecated**" } else { "" };
    doc.heading(1, &format!("{}{badge}", inline(&e.name)));
    doc.para(&format!(
        "[Index](../README.md) / [Section](../sections/{}.md)",
        e.section
    ));
    request_line(&mut doc, e);
    if let Some(summary) = &e.summary {
        doc.para(&format!("**{}**", inline(summary)));
    }
    doc.html(e.description.as_ref());
    if let Some(auth) = &e.auth {
        doc.para(&format!("Auth: {}", code(auth)));
    }
    doc.table_section("Query parameters", "Name", &e.query);
    doc.table_section("Headers", "Header", &e.headers);
    body(&mut doc, e);
    doc.table_section("Fields", "Field", &e.fields);
    doc.list_section("Expected behavior", &e.expected);
    doc.list_section("Produces", &e.produces);
    schemas(&mut doc, &e.schemas);
    if let Some(example) = &e.example {
        example_section(&mut doc, example);
    }
    samples(&mut doc, e);
    doc
}

fn request_line(doc: &mut Doc, e: &Endpoint) {
    match (&e.method, &e.url) {
        (Some(method), Some(url)) => {
            doc.para(&code_text(&format!("{method} {url}")));
        }
        _ => doc.para(&format!("Kind: {}", code(&e.kind))),
    }
}

fn body(doc: &mut Doc, e: &Endpoint) {
    let Some(kind) = &e.body_type else {
        return;
    };
    doc.heading(2, "Body");
    doc.para(&format!("Body type: {}", code(kind)));
    if !e.body_fields.is_empty() {
        doc.rows(&["Field", "Value"], &e.body_fields);
    }
    if let Some(text) = &e.body_text {
        doc.code_block("Body", "", text.as_str());
    }
}

fn schemas(doc: &mut Doc, schemas: &[Schema]) {
    if schemas.is_empty() {
        return;
    }
    doc.heading(2, "Schemas");
    for schema in schemas {
        doc.heading(3, &inline(&schema.label));
        doc.code_block(schema.label.as_str(), "json", schema.json.as_str());
    }
}

fn example_section(doc: &mut Doc, example: &Example) {
    doc.heading(2, "Example response");
    let status = example
        .status
        .map(|s| format!("Status `{s}`"))
        .unwrap_or_default();
    let origin = match &example.recorded {
        Some(date) => format!("recorded {}", inline(date)),
        None => "from the stored snapshot".to_owned(),
    };
    doc.para(format!("{status} *{origin}*").trim());
    if !example.headers.is_empty() {
        doc.rows(&["Header", "Value"], &example.headers);
    }
    if let Some(body) = &example.body {
        let info = if example.body_json { "json" } else { "" };
        doc.code_block("Body", info, body.as_str());
    }
    if example.truncated {
        doc.para("*Body truncated at 64 KB.*");
    }
}

fn samples(doc: &mut Doc, e: &Endpoint) {
    if e.samples.is_empty() {
        return;
    }
    doc.heading(2, "Code samples");
    for sample in &e.samples {
        doc.heading(3, &inline(&sample.id));
        doc.fenced(sample.fence, sample.code.as_str());
    }
}

fn reference_page(set: &ReferenceSet, page: &ReferencePage) -> Doc {
    let mut doc = Doc::default();
    doc.heading(1, &inline(&page.name));
    doc.para(&format!(
        "[Index](../README.md) / {} {}",
        inline(&set.title),
        code(&set.endpoint)
    ));
    doc.para(&format!("*{}*", inline(&page.label)));
    if let Some(reason) = &page.deprecated {
        doc.para(&format!("**deprecated**: {}", inline(reason)));
    }
    doc.html(page.description.as_ref());
    for table in &page.tables {
        reference_table(&mut doc, table);
    }
    schemas(&mut doc, &page.schemas);
    doc
}

fn reference_table(doc: &mut Doc, table: &Table) {
    doc.heading(2, &inline(&table.title));
    let head: Vec<String> = table.columns.iter().map(cell).collect();
    doc.table_head(&head);
    for row in &table.rows {
        let cells: Vec<String> = row.iter().map(cell).collect();
        doc.line(&format!("| {} |", cells.join(" | ")));
    }
    doc.blank();
}

/// A Markdown document being written; every line ends with `\n`.
#[derive(Default)]
struct Doc {
    out: String,
}

impl Doc {
    fn finish(mut self) -> Vec<u8> {
        while self.out.ends_with("\n\n") {
            self.out.pop();
        }
        self.out.into_bytes()
    }

    fn line(&mut self, text: &str) {
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn blank(&mut self) {
        self.out.push('\n');
    }

    fn para(&mut self, text: &str) {
        self.line(text);
        self.blank();
    }

    fn heading(&mut self, level: usize, text: &str) {
        self.para(&format!("{} {text}", "#".repeat(level)));
    }

    /// A sanitized HTML description, as an HTML block.
    fn html(&mut self, html: Option<&Html>) {
        if let Some(html) = html {
            self.para(&without_blank_lines(html.as_str().trim_end()));
        }
    }

    fn table_head(&mut self, columns: &[String]) {
        self.line(&format!("| {} |", columns.join(" | ")));
        let rule = vec!["---"; columns.len()].join(" | ");
        self.line(&format!("| {rule} |"));
    }

    /// Name/value rows, with a description column when a row has one.
    fn rows(&mut self, columns: &[&str], rows: &[Row]) {
        let described = rows.iter().any(|r| r.description.is_some());
        let mut head: Vec<String> = columns.iter().map(|c| (*c).to_owned()).collect();
        if described {
            head.push("Description".to_owned());
        }
        self.table_head(&head);
        for row in rows {
            let mut cells = vec![cell_code(&row.name), cell_code(&row.value)];
            if described {
                cells.push(row.description.as_ref().map(cell).unwrap_or_default());
            }
            self.line(&format!("| {} |", cells.join(" | ")));
        }
        self.blank();
    }

    fn table_section(&mut self, title: &str, first: &str, rows: &[Row]) {
        if rows.is_empty() {
            return;
        }
        self.heading(2, title);
        self.rows(&[first, "Value"], rows);
    }

    fn list_section(&mut self, title: &str, items: &[Rendered]) {
        if items.is_empty() {
            return;
        }
        self.heading(2, title);
        for item in items {
            self.line(&format!("- {}", inline(item)));
        }
        self.blank();
    }

    fn fenced(&mut self, info: &str, body: &str) {
        let fence = "`".repeat(longest_run(body, '`').max(2) + 1);
        self.line(&format!("{fence}{info}"));
        self.line(body.trim_end_matches('\n'));
        self.para(&fence);
    }

    /// A fenced block, folded in `<details>` when it is long.
    fn code_block(&mut self, summary: &str, info: &str, body: &str) {
        if body.lines().count() <= LONG_BODY_LINES {
            self.fenced(info, body);
            return;
        }
        self.para(&format!(
            "<details><summary>{}</summary>",
            escape_html(summary)
        ));
        self.fenced(info, body);
        self.para("</details>");
    }
}

/// The length of the longest run of `c` in `text`.
fn longest_run(text: &str, c: char) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for ch in text.chars() {
        run = if ch == c { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

/// `[text](target)` with the text escaped.
fn link(text: &Rendered, target: &str) -> String {
    format!("[{}]({target})", inline(text))
}

/// Redacted text as inline Markdown.
fn inline(text: &Rendered) -> String {
    escape(text.as_str())
}

/// Inline Markdown that shows `text` as written: punctuation that would
/// start emphasis, a link, an image, code or a table cell is escaped with
/// a backslash (`!` and `#` need none: `[` is escaped and user text never
/// starts a line), `<`, `>` and `&` become entities, line breaks `<br>`.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '\n' => out.push_str("<br>"),
            '\r' => {}
            '\\' | '`' | '*' | '_' | '[' | ']' | '|' | '~' => {
                out.push('\\');
                out.push(c);
            }
            other => out.push(other),
        }
    }
    out
}

/// A code span of `text` (not in a table).
fn code(text: &Rendered) -> String {
    code_text(text.as_str())
}

fn code_text(text: &str) -> String {
    let flat = text.replace(['\r', '\n'], " ");
    let ticks = "`".repeat(longest_run(&flat, '`') + 1);
    let pad = if flat.starts_with('`') || flat.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{ticks}{pad}{flat}{pad}{ticks}")
}

/// A table cell showing `text` as code, or as text with line breaks when
/// it spans lines. `|` is escaped, as GFM requires even in code.
fn cell_code(text: &Rendered) -> String {
    if text.as_str().contains('\n') {
        return cell(text);
    }
    if text.is_empty() {
        return String::new();
    }
    code(text).replace('|', "\\|")
}

/// A table cell showing `text`.
fn cell(text: &Rendered) -> String {
    inline(text)
}

/// `html` with no blank line, so a Markdown renderer reads it as one HTML
/// block: a blank line would end the block and have what follows (inside
/// a `<pre>`, say) parsed as Markdown. The line break before a blank line
/// is written as `&#10;`, which renders the same inside `<pre>` and as
/// whitespace elsewhere.
fn without_blank_lines(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    for (i, line) in html.split('\n').enumerate() {
        if i > 0 {
            out.push_str(if line.trim().is_empty() {
                "&#10;"
            } else {
                "\n"
            });
        }
        out.push_str(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_text_cannot_become_markup() {
        assert_eq!(
            escape("<script>x</script> [a](javascript:y) ![i](z) *b* `c` a|b"),
            "&lt;script&gt;x&lt;/script&gt; \\[a\\](javascript:y) !\\[i\\](z) \\*b\\* \\`c\\` a\\|b"
        );
        assert_eq!(escape("a\nb &amp;"), "a<br>b &amp;amp;");
        assert_eq!(escape("{{base_url}}/users"), "{{base\\_url}}/users");
    }

    #[test]
    fn descriptions_stay_one_html_block() {
        assert_eq!(
            without_blank_lines("<pre><code>a\n\n  \nb\n</code></pre>"),
            "<pre><code>a&#10;&#10;  \nb\n</code></pre>"
        );
        assert_eq!(
            without_blank_lines("<p>a</p>\n<p>b</p>"),
            "<p>a</p>\n<p>b</p>"
        );
    }

    #[test]
    fn code_spans_and_fences_outgrow_backticks() {
        assert_eq!(code_text("a`b"), "``a`b``");
        assert_eq!(code_text("`x`"), "`` `x` ``");
        assert_eq!(code_text("a\nb"), "`a b`");
        let mut doc = Doc::default();
        doc.fenced("sh", "echo ```x```\n");
        assert_eq!(doc.out, "````sh\necho ```x```\n````\n\n");
    }

    #[test]
    fn long_bodies_are_folded() {
        let mut short = Doc::default();
        short.code_block("Body", "json", "{}");
        assert!(!short.out.contains("<details>"), "{}", short.out);
        let mut long = Doc::default();
        let body = "x\n".repeat(LONG_BODY_LINES + 1);
        long.code_block("Body <1>", "json", &body);
        assert!(
            long.out
                .starts_with("<details><summary>Body &lt;1&gt;</summary>\n\n```json\n"),
            "{}",
            long.out
        );
        assert!(long.out.ends_with("```\n\n</details>\n\n"), "{}", long.out);
    }
}
