//! `rustman docs` code samples end to end (api-docs-generation "Code
//! samples"): they match `rustman export snippet --placeholders` byte for
//! byte (runner-parity task 4.5), the curl samples run against the local
//! echo server, and unknown request kinds still get a page.

mod common;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use common::{Server, rustman, write};
use serde_json::Value;

/// Requests with JSON, form, multipart and file bodies, all sent to the
/// echo server through `{{base_url}}`, with collection and workspace
/// variables kept as placeholders.
const REQUESTS: [(&str, &str); 4] = [
    (
        "api/json",
        "version: 1\nkind: http\nname: Json\nmethod: POST\nurl: '{{base_url}}/echo'\nquery:\n  - name: q\n    value: a b&c\nheaders:\n  - name: X-Request-Id\n    value: docs-42\nbody:\n  type: json\n  content:\n    name: Ada\n    tags: [x, 1]\n",
    ),
    (
        "api/form",
        "version: 1\nkind: http\nname: Form\nmethod: POST\nurl: '{{base_url}}/echo'\nbody:\n  type: form\n  fields:\n    - name: q\n      value: a b&c\n    - name: tenant\n      value: '{{tenant}}'\n",
    ),
    (
        "api/upload",
        "version: 1\nkind: http\nname: Upload\nmethod: POST\nurl: '{{base_url}}/echo'\nbody:\n  type: multipart\n  parts:\n    - name: title\n      value: Report\n    - name: file\n      file: ../data/report.csv\n      content_type: text/csv\n",
    ),
    (
        "api/raw",
        "version: 1\nkind: http\nname: Raw\nmethod: PUT\nurl: '{{base_url}}/echo'\nheaders:\n  - name: Content-Type\n    value: text/csv\nbody:\n  type: file\n  path: ../data/report.csv\n",
    ),
];

const LANGS: [&str; 5] = ["curl", "httpie", "fetch", "python", "rust"];

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"Shop\"\nversion = 1\n\n[variables]\ntenant = \"acme\"\n",
    );
    write(
        root,
        "environments/dev.yaml",
        "variables:\n  base_url: http://localhost:9\n",
    );
    write(root, ".gitignore", ".rustman/\n");
    write(root, "data/report.csv", "a,b\n1,2\n");
    for (id, text) in REQUESTS {
        write(root, &format!("{id}.rman.yaml"), text);
    }
    dir
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn build_markdown(root: &Path) {
    let output = rustman(root)
        .args(["docs", "build", "--format", "markdown"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
}

/// The code of sample `lang` on the Markdown page `page`.
fn sample(page: &str, lang: &str) -> String {
    let heading = format!("### {lang}\n\n");
    let start = page.find(&heading).unwrap_or_else(|| panic!("{page}")) + heading.len();
    let block = &page[start..];
    let fence_end = block.find('\n').unwrap();
    let fence: String = block.chars().take_while(|c| *c == '`').collect();
    let body = &block[fence_end + 1..];
    let end = body.find(&format!("\n{fence}\n")).unwrap();
    body[..end].to_owned()
}

fn page(root: &Path, id: &str) -> String {
    let slug = id.replace('/', "-");
    fs::read_to_string(root.join(format!("site/endpoints/{slug}.md"))).unwrap()
}

#[test]
fn docs_samples_match_export_snippet_placeholders_byte_for_byte() {
    let ws = workspace();
    build_markdown(ws.path());
    for (id, _) in REQUESTS {
        let page = page(ws.path(), id);
        for lang in LANGS {
            let output = rustman(ws.path())
                .args(["export", "snippet", id, "--lang", lang, "--placeholders"])
                .output()
                .unwrap();
            assert!(output.status.success(), "{}", stderr(&output));
            let exported = String::from_utf8(output.stdout).unwrap();
            let exported = exported.strip_suffix('\n').unwrap_or(&exported);
            assert_eq!(sample(&page, lang), exported, "{id} {lang}");
        }
    }
}

fn curl_available() -> bool {
    Command::new("curl")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The curl sample of `page` as arguments, with its placeholders filled in
/// as a reader would: `{{base_url}}` pointing at `base`, `{{tenant}}` as
/// `acme`. Comment lines (auth notes) are dropped.
fn curl_args(page: &str, base: &str) -> Vec<String> {
    let code = sample(page, "curl");
    let command: Vec<&str> = code.lines().filter(|l| !l.starts_with('#')).collect();
    let words = shell_words::split(&command.join("\n")).unwrap();
    assert_eq!(words[0], "curl");
    words[1..]
        .iter()
        .map(|w| {
            w.replace("{{base_url}}", base)
                .replace("{{tenant}}", "acme")
        })
        .collect()
}

#[test]
fn curl_samples_run_against_the_echo_server() {
    if !curl_available() {
        eprintln!("skipped: curl is not installed");
        return;
    }
    let server = Server::http();
    let ws = workspace();
    build_markdown(ws.path());
    let base = server.url("").trim_end_matches('/').to_owned();
    let mut echoes = Vec::new();
    for (id, _) in REQUESTS {
        let output = Command::new("curl")
            .args(["--silent", "--show-error", "--max-time", "30"])
            .args(curl_args(&page(ws.path(), id), &base))
            .current_dir(ws.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{id}: {}", stderr(&output));
        let echo: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|e| panic!("{id}: {e}: {}", String::from_utf8_lossy(&output.stdout)));
        echoes.push(echo);
    }
    let text = |v: &Value, key: &str| v[key].as_str().unwrap_or_default().to_owned();
    let [json, form, upload, raw] = echoes.as_slice() else {
        unreachable!()
    };
    assert_eq!(text(json, "method"), "POST");
    assert_eq!(text(json, "path"), "/echo?q=a+b%26c");
    let body: Value = serde_json::from_str(&text(json, "body")).unwrap();
    assert_eq!(body, serde_json::json!({"name": "Ada", "tags": ["x", 1]}));
    assert_eq!(text(form, "body"), "q=a+b%26c&tenant=acme");
    assert!(
        text(upload, "body").contains("name=\"file\"; filename=\"report.csv\""),
        "{upload}"
    );
    assert!(text(upload, "body").contains("a,b\n1,2\n"), "{upload}");
    assert_eq!(text(raw, "method"), "PUT");
    assert_eq!(text(raw, "body"), "a,b\n1,2\n");
}

#[test]
fn unknown_kinds_get_generic_pages() {
    let ws = workspace();
    write(
        ws.path(),
        "events/orders.rman.yaml",
        "version: 1\nkind: kafka\nname: Orders\ntopic: orders\nbrokers: localhost:9092\n",
    );
    let output = rustman(ws.path())
        .args(["docs", "build", "--format", "both"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let md = fs::read_to_string(ws.path().join("site/endpoints/events-orders.md")).unwrap();
    assert!(md.contains("Kind: `kafka`"), "{md}");
    assert!(md.contains("| `topic` | `orders` |"), "{md}");
    assert!(
        ws.path()
            .join("site/endpoints/events-orders.html")
            .is_file()
    );
}
