//! The HTML site: page snapshots, the search index and script, and the
//! theme tokens (api-docs-generation "Client-side search", "Dark and light
//! themes", "Self-contained offline site", "Other request kinds").

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{build_site, read, request, workspace, write};

const CSS: &str = include_str!("../src/assets/app.css");
const SCRIPT: &str = include_str!("../src/assets/app.js");

/// `text` with the build version replaced, so snapshots survive releases.
fn stable(text: &str) -> String {
    text.replace(rustman_core::VERSION, "<version>")
}

#[test]
fn page_snapshots() {
    let ws = workspace("");
    build_site(ws.path());
    let site = ws.path().join("site");
    insta::assert_snapshot!("index_html", stable(&read(&site, "index.html")));
    insta::assert_snapshot!(
        "endpoint_html",
        stable(&read(&site, "endpoints/users-create.html"))
    );
}

#[test]
fn search_index_snapshot() {
    let ws = workspace("");
    build_site(ws.path());
    insta::assert_snapshot!(
        "search_index",
        read(&ws.path().join("site"), "search-index.json")
    );
}

#[test]
fn a_grpc_request_gets_a_generic_page() {
    let ws = workspace("");
    write(
        ws.path(),
        "rpc/get-product.rman.yaml",
        "version: 1\nkind: grpc\nname: GetProduct\ntarget: localhost:50051\nmethod: shop.Catalog/GetProduct\nmetadata:\n  - name: authorization\n    value: Bearer literal-credential-1234\nmessage:\n  id: 7\n",
    );
    build_site(ws.path());
    let page = common::visible_text(&read(
        &ws.path().join("site"),
        "endpoints/rpc-get-product.html",
    ));
    assert!(page.contains("kind: grpc"), "{page}");
    assert!(page.contains("target localhost:50051"), "{page}");
    assert!(page.contains("method shop.Catalog/GetProduct"), "{page}");
    assert!(
        page.contains("authorization: «redacted:Authorization»"),
        "{page}"
    );
    assert!(!page.contains("literal-credential"), "{page}");
}

#[test]
fn a_graphql_page_lists_its_operation_variables() {
    let ws = workspace("");
    write(
        ws.path(),
        "gql/get-user.rman.yaml",
        &format!(
            "version: 1\nkind: graphql\nname: Get user\nendpoint: '{{{{base_url}}}}/graphql'\ndocument: 'query($id: ID!) {{ user(id: $id) {{ email }} }}'\nvariables:\n  id: '{{{{user_id}}}}'\n  password: gql-literal-pass\n  note: '{}'\n",
            common::TOKEN
        ),
    );
    build_site(ws.path());
    let html = read(&ws.path().join("site"), "endpoints/gql-get-user.html");
    let page = common::visible_text(&html);
    assert!(
        page.contains("variables {&quot;id&quot;:&quot;{{user_id}}&quot;,"),
        "{page}"
    );
    // The operation inputs stay redacted: a store secret and a literal
    // credential-named variable.
    assert!(!html.contains(common::TOKEN), "{html}");
    assert!(!html.contains("gql-literal-pass"), "{html}");
    assert!(
        page.contains("&quot;password&quot;:&quot;«redacted:password»&quot;"),
        "{page}"
    );
    assert!(
        page.contains("&quot;note&quot;:&quot;«redacted:token»&quot;"),
        "{page}"
    );
}

// Theme tokens ------------------------------------------------------------

/// The `--name: #rrggbb` tokens of the CSS block that starts with `selector`.
fn tokens(selector: &str) -> Vec<(String, String)> {
    let start = CSS
        .find(selector)
        .unwrap_or_else(|| panic!("no {selector}"));
    let body = &CSS[start..];
    let body = &body[body.find('{').unwrap() + 1..body.find('}').unwrap()];
    body.lines()
        .filter_map(|l| {
            let (name, value) = l.trim().strip_prefix("--")?.split_once(':')?;
            Some((
                name.trim().to_owned(),
                value.trim().trim_end_matches(';').to_owned(),
            ))
        })
        .collect()
}

fn luminance(hex: &str) -> f64 {
    let channel = |i: usize| {
        let c = f64::from(u8::from_str_radix(&hex[i..i + 2], 16).unwrap()) / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5)
}

fn contrast(a: &str, b: &str) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[test]
fn both_themes_meet_wcag_aa_contrast() {
    let pairs = [
        ("fg", "bg"),
        ("muted", "bg"),
        ("link", "bg"),
        ("fg", "panel"),
        ("link", "panel"),
        ("badge-fg", "badge-bg"),
        ("method-fg", "method-bg"),
    ];
    let light = tokens(":root {");
    let dark = tokens(":root[data-theme=\"dark\"] {");
    assert_eq!(
        tokens(":root:not([data-theme=\"light\"]) {"),
        dark,
        "system dark = toggled dark"
    );
    for theme in [&light, &dark] {
        let get = |name: &str| theme.iter().find(|(n, _)| n == name).unwrap().1.clone();
        for (fg, bg) in pairs {
            let ratio = contrast(&get(fg), &get(bg));
            assert!(ratio >= 4.5, "{fg} on {bg}: {ratio:.2} in {theme:?}");
        }
    }
}

#[test]
fn the_system_preference_is_followed_and_a_stored_choice_wins() {
    assert!(
        CSS.contains(
            "@media (prefers-color-scheme: dark) {\n  :root:not([data-theme=\"light\"]) {"
        )
    );
    let ws = workspace("");
    build_site(ws.path());
    let index = read(&ws.path().join("site"), "index.html");
    assert!(
        index.contains("try{var t=window.localStorage.getItem(\"rustman-docs-theme\")"),
        "{index}"
    );
    assert!(
        index.contains("}catch(e){}"),
        "storage errors fall back to the system preference"
    );
    assert!(SCRIPT.contains("try { window.localStorage.setItem(THEME_KEY, next); } catch (e)"));
}

// Search ------------------------------------------------------------------

/// Skip a test whose tool is missing, unless `RUSTMAN_REQUIRE_BROWSER` is
/// set (as CI sets it on Linux), where a missing tool fails the test.
fn skip(reason: &str) {
    assert!(
        std::env::var_os("RUSTMAN_REQUIRE_BROWSER").is_none(),
        "{reason}, and RUSTMAN_REQUIRE_BROWSER is set"
    );
    eprintln!("skipped: {reason}");
}

fn node() -> Option<PathBuf> {
    let ok = Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    ok.then(|| PathBuf::from("node"))
}

/// A workspace with `n` endpoints in 20 folders.
fn big_workspace(n: usize) -> tempfile::TempDir {
    let ws = workspace("");
    for i in 0..n {
        let rel = format!("bulk{:02}/op{i:04}.rman.yaml", i % 20);
        let url = format!("{{{{base_url}}}}/things/{i}/items");
        let extra =
            "query:\n  - name: page\n    value: '1'\ndescription: Lists the items of a thing.\n";
        write(
            ws.path(),
            &rel,
            &request(&format!("Thing {i}"), "GET", &url, extra),
        );
    }
    ws
}

#[test]
fn searching_2000_endpoints_takes_under_100_ms() {
    let Some(node) = node() else {
        return skip("node is not installed");
    };
    let ws = big_workspace(2000);
    let started = Instant::now();
    build_site(ws.path());
    eprintln!("built 2000 endpoint pages in {:?}", started.elapsed());
    let site = ws.path().join("site");
    let script = r#"
const fs = require("fs");
const engine = require(process.argv[2]);
const index = JSON.parse(fs.readFileSync(process.argv[3], "utf8"));
// Best of five runs: CI runners are shared, and a single sample also pays for the
// JIT warming up, which a reader's browser does once per page load.
const best = f => {
  let min = Infinity, value;
  for (let i = 0; i < 5; i++) {
    const t = process.hrtime.bigint();
    value = f();
    min = Math.min(min, Number(process.hrtime.bigint() - t) / 1e6);
  }
  return [min, value];
};
const [prepareMs, prepared] = best(() => engine.prepare(index.entries));
let worst = 0, hits = 0;
for (const q of ["users", "thing 1999", "items", "thng", "zzz-nothing", "get /things"]) {
  const [ms, found] = best(() => engine.search(prepared, q, 20));
  hits += found.length;
  worst = Math.max(worst, ms);
}
console.log(JSON.stringify({ entries: index.entries.length, prepareMs, worst, hits }));
"#;
    let js = ws.path().join("bench.js");
    std::fs::write(&js, script).unwrap();
    let out = Command::new(node)
        .arg(&js)
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/assets/app.js"))
        .arg(site.join("search-index.json"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    eprintln!("{result}");
    assert!(result["entries"].as_u64().unwrap() >= 2000);
    assert!(result["prepareMs"].as_f64().unwrap() < 100.0, "{result}");
    assert!(result["worst"].as_f64().unwrap() < 100.0, "{result}");
    assert!(result["hits"].as_u64().unwrap() > 0);
}

#[test]
fn the_search_script_finds_by_path_prefix_and_fuzzy() {
    let Some(node) = node() else {
        return skip("node is not installed");
    };
    let script = r#"
const engine = require(process.argv[2]);
const entries = [
  { t: "Get user", m: "GET", p: "/users/{id}", k: "http", d: "", w: ["id"], u: "endpoints/a.html" },
  { t: "Login", m: "POST", p: "/login", k: "http", d: "Starts a session", w: [], u: "endpoints/b.html" },
];
const p = engine.prepare(entries);
const names = q => engine.search(p, q, 20).map(e => e.t).join("|");
console.log([names("users"), names("sess"), names("lgn"), names("post login"), names("nothing")].join(";"));
"#;
    let dir = tempfile::tempdir().unwrap();
    let js = dir.path().join("t.js");
    std::fs::write(&js, script).unwrap();
    let out = Command::new(node)
        .arg(&js)
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/assets/app.js"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "Get user;Login;Login;Login;"
    );
}

fn chrome() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = std::env::var_os("CHROME")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    for name in [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
    ] {
        candidates.push(PathBuf::from(name));
    }
    candidates.push("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into());
    candidates.push(r"C:\Program Files\Google\Chrome\Application\chrome.exe".into());
    candidates.push(r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe".into());
    candidates.into_iter().find(|c| usable(c))
}

/// On Windows `chrome.exe --version` opens a browser window and never exits, so there
/// only an existing absolute path counts; elsewhere the binary must answer `--version`.
fn usable(candidate: &Path) -> bool {
    if cfg!(windows) {
        return candidate.is_absolute() && candidate.is_file();
    }
    Command::new(candidate)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.trim_start_matches('/');
    format!("file:///{}", text.replace(' ', "%20"))
}

const CHROME_DEADLINE: Duration = Duration::from_secs(60);

/// Runs Chrome with stdout sent to a file, not a pipe: on Windows, Chrome's helper
/// processes inherit the pipe and keep it open, so `Command::output` never returns.
/// Chrome is killed (with its process tree) once the deadline passes. Returns `None`
/// when it timed out without dumping anything.
fn dump_dom(mut command: Command, scratch: &Path) -> Option<String> {
    let path = scratch.join("dom.html");
    let file = std::fs::File::create(&path).unwrap();
    let mut child = command
        .stdout(file)
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let exited = loop {
        if child.try_wait().unwrap().is_some() {
            break true;
        }
        if started.elapsed() > CHROME_DEADLINE {
            break false;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    if !exited {
        kill_tree(&mut child);
    }
    let dom = std::fs::read_to_string(&path).unwrap_or_default();
    (exited || !dom.is_empty()).then_some(dom)
}

fn kill_tree(child: &mut std::process::Child) {
    if cfg!(windows) {
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .output();
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn a_headless_browser_finds_users_by_path() {
    let Some(chrome) = chrome() else {
        return skip("no Chrome or Chromium found");
    };
    let ws = workspace("");
    write(
        ws.path(),
        "users/by-id.rman.yaml",
        &request("Fetch account", "GET", "/users/{id}", ""),
    );
    build_site(ws.path());
    let index = ws.path().join("site").join("index.html");
    let profile = tempfile::tempdir().unwrap();
    let mut command = Command::new(chrome);
    command
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-sandbox",
            "--no-first-run",
        ])
        .arg(format!("--user-data-dir={}", profile.path().display()))
        .args(["--virtual-time-budget=2000", "--dump-dom"])
        .arg(format!("{}?q=users", file_url(&index)));
    let Some(dom) = dump_dom(command, profile.path()) else {
        return skip("Chrome did not exit within the deadline");
    };
    let results = dom
        .split("<ul id=\"search-results\">")
        .nth(1)
        .unwrap_or_default();
    let results = &results[..results.find("</ul>").unwrap_or(0)];
    assert!(
        results.contains("Fetch account /users/{id}"),
        "results: {results}\n{dom}"
    );
    assert!(
        results.contains("href=\"endpoints/users-by-id.html\""),
        "{results}"
    );
    assert!(
        dom.contains("<div id=\"search\">"),
        "the search box is shown with JavaScript"
    );
}
