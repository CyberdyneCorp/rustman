//! Markdown output (api-docs-generation "Markdown output"): `README.md`
//! as an index linking every section file in workspace order, one file per
//! section and endpoint, GitHub-flavored tables, fenced samples, long
//! bodies in `<details>`, and the same redaction as HTML.

mod common;

use common::{TOKEN, build_with, files, read, record, request, workspace, write};
use rustman_docs::Format;
use serde_json::json;

fn build_markdown(root: &std::path::Path) -> rustman_docs::BuildReport {
    build_with(root, |o| {
        o.out = root.join("docs/api");
        o.shown_out = "docs/api/".into();
        o.format = Format::Markdown;
    })
    .unwrap()
}

#[test]
fn readme_links_every_section_file_in_workspace_order() {
    let ws = workspace("");
    write(
        ws.path(),
        "users/admin/promote.rman.yaml",
        &request("Promote", "POST", "{{base_url}}/admin/promote", ""),
    );
    let report = build_markdown(ws.path());
    let out = ws.path().join("docs/api");
    let readme = read(&out, "README.md");
    let links: Vec<&str> = readme
        .lines()
        .filter(|l| l.trim_start().starts_with("- ["))
        .collect();
    assert_eq!(
        links,
        [
            "- [auth](sections/auth.md)",
            "- [Users](sections/users.md)",
            "  - [admin](sections/users-admin.md)",
        ]
    );
    for section in ["auth", "users", "users-admin"] {
        assert!(out.join(format!("sections/{section}.md")).is_file());
    }
    let html_only = report.files.iter().any(|f| f.ends_with(".html"));
    assert!(!html_only, "markdown only: {:?}", report.files);
    assert!(!report.files.iter().any(|f| f.starts_with("assets/")));
    let pages: Vec<&String> = report
        .files
        .iter()
        .filter(|f| f.starts_with("endpoints/"))
        .collect();
    assert_eq!(pages.len(), 6, "{pages:?}");
}

#[test]
fn page_snapshots() {
    let ws = workspace("");
    build_markdown(ws.path());
    let out = ws.path().join("docs/api");
    insta::assert_snapshot!("readme_md", read(&out, "README.md"));
    insta::assert_snapshot!("section_md", read(&out, "sections/users.md"));
    insta::assert_snapshot!("endpoint_md", read(&out, "endpoints/users-create.md"));
}

#[test]
fn both_formats_sit_side_by_side() {
    let ws = workspace("");
    let report = build_with(ws.path(), |o| o.format = Format::Both).unwrap();
    for name in [
        "index.html",
        "README.md",
        "endpoints/users-get.html",
        "endpoints/users-get.md",
    ] {
        assert!(report.files.iter().any(|f| f == name), "{name}");
    }
}

#[test]
fn user_content_is_escaped_and_secrets_redacted() {
    let ws = workspace("");
    write(
        ws.path(),
        "users/evil.rman.yaml",
        &request(
            "<img src=x onerror=alert(1)> [click](javascript:alert(1)) | pipe",
            "POST",
            "{{base_url}}/evil",
            &format!(
                "description: '<script>alert(1)</script> token {TOKEN}'\nheaders:\n  - name: X-Note\n    value: 'a|b `c`'\n"
            ),
        ),
    );
    build_markdown(ws.path());
    let page = read(&ws.path().join("docs/api"), "endpoints/users-evil.md");
    assert!(
        !page.contains("<img") && !page.contains("<script"),
        "{page}"
    );
    assert!(
        page.starts_with(
            "# &lt;img src=x onerror=alert(1)&gt; \\[click\\](javascript:alert(1)) \\| pipe\n"
        ),
        "{page}"
    );
    assert!(page.contains("| `X-Note` | `` a\\|b `c` `` |"), "{page}");
    assert!(page.contains("«redacted:token»"), "{page}");
    for (name, bytes) in files(&ws.path().join("docs/api")) {
        assert!(!String::from_utf8_lossy(&bytes).contains(TOKEN), "{name}");
    }
}

#[test]
fn long_bodies_are_folded_in_details() {
    let ws = workspace("examples = [\"users/create\"]\n");
    let items: Vec<_> = (0..30).map(|i| json!({"id": i})).collect();
    record(
        ws.path(),
        "01JD0000000000000000000001",
        "users/create.rman.yaml",
        201,
        &json!({"items": items}).to_string(),
        json!([["content-type", "application/json"]]),
    );
    build_markdown(ws.path());
    let page = read(&ws.path().join("docs/api"), "endpoints/users-create.md");
    let at = page.find("## Example response").unwrap();
    let example = &page[at..];
    assert!(
        example.contains("<details><summary>Body</summary>\n\n```json\n{\n  \"items\": ["),
        "{example}"
    );
    assert!(example.contains("```\n\n</details>\n"), "{example}");
    let short = read(&ws.path().join("docs/api"), "endpoints/users-get.md");
    assert!(!short.contains("<details>"), "{short}");
}

#[test]
fn a_description_starting_with_html_stays_inert_markdown() {
    let ws = workspace("");
    write(
        ws.path(),
        "users/_collection.yaml",
        "name: Users\ndescription: \"<script>alert(3)</script> [x](javascript:alert(4)) ![i](http://evil.example/p.png)\\n\\n```\\na\\n\\n[y](javascript:alert(5))\\n```\\n\"\n",
    );
    build_markdown(ws.path());
    let page = read(&ws.path().join("docs/api"), "sections/users.md");
    assert!(
        page.contains("<p>&lt;script&gt;alert(3)&lt;/script&gt; [x](javascript:alert(4))"),
        "{page}"
    );
    // The blank line inside the code block would end the HTML block and
    // have the link after it parsed as Markdown.
    assert!(page.contains("<pre><code>a&#10;\n[y]"), "{page}");
    let description: Vec<&str> = page
        .lines()
        .skip_while(|l| !l.starts_with("<p>"))
        .take_while(|l| !l.is_empty())
        .collect();
    assert_eq!(description.len(), 5, "{page}");
}
