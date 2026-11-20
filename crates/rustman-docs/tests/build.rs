//! `rustman docs build` through the library: structure, page content,
//! self-contained output and the output folder rules
//! (api-docs-generation "Build command", "Structure follows the
//! workspace", "Endpoint page content", "Self-contained offline site").

mod common;

use std::fs;

use common::{TOKEN, build_site, build_with, files, read, visible_text, workspace, write};

#[test]
fn default_build_has_one_section_per_collection_and_one_page_per_request() {
    let ws = workspace("");
    let report = build_site(ws.path());
    let site = ws.path().join("site");
    let index = read(&site, "index.html");
    assert!(index.contains("id=\"users\""), "{index}");
    assert!(index.contains("id=\"auth\""), "{index}");
    let pages: Vec<&String> = report
        .files
        .iter()
        .filter(|f| f.starts_with("endpoints/"))
        .collect();
    assert_eq!(
        pages,
        [
            "endpoints/auth-list.html",
            "endpoints/auth-login.html",
            "endpoints/users-create.html",
            "endpoints/users-delete.html",
            "endpoints/users-get.html",
        ]
    );
    for page in pages {
        assert!(site.join(page).is_file(), "{page}");
    }
    assert!(site.join(".rustman-docs-manifest").is_file());
}

#[test]
fn collection_order_is_kept_in_the_navigation() {
    let ws = workspace("");
    build_site(ws.path());
    let index = read(&ws.path().join("site"), "index.html");
    let login = index.find("endpoints/auth-login.html").unwrap();
    let list = index.find("endpoints/auth-list.html").unwrap();
    assert!(login < list, "login must come before list");
}

#[test]
fn hidden_requests_have_no_page_navigation_or_search_entry() {
    let ws = workspace("");
    let report = build_site(ws.path());
    assert!(!report.files.iter().any(|f| f.contains("debug")));
    for (name, bytes) in files(&ws.path().join("site")) {
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("Debug dump"), "{name}");
        assert!(!text.contains("/debug"), "{name}");
    }
}

#[test]
fn endpoint_page_shows_method_url_tables_and_expectations() {
    let ws = workspace("");
    build_site(ws.path());
    let page = read(&ws.path().join("site"), "endpoints/users-create.html");
    let text = visible_text(&page);
    assert!(text.contains("POST {{base_url}}/users"), "{text}");
    assert!(
        text.contains("Headers Header Value Content-Type application/json"),
        "{text}"
    );
    assert!(text.contains("Field Value name Ada role admin"), "{text}");
    assert!(text.contains("Body type: json"), "{text}");
    assert!(text.contains("Expected: status 201"), "{text}");
    assert!(
        text.contains("Produces: {{user_id}} from json $.id"),
        "{text}"
    );
    assert!(text.contains("Authorization Bearer {{token}}"), "{text}");
    let login = visible_text(&read(&ws.path().join("site"), "endpoints/auth-login.html"));
    assert!(login.contains("Auth: basic"), "{login}");
    let (details, samples) = login.split_once("Code samples").unwrap();
    assert!(!details.contains("ada"), "no credentials: {details}");
    assert!(samples.contains("«redacted:token»"), "{samples}");
}

#[test]
fn descriptions_are_commonmark_with_user_content_escaped() {
    let ws = workspace("");
    write(
        ws.path(),
        "users/get.rman.yaml",
        &common::request(
            "Get user",
            "GET",
            "{{base_url}}/users/1",
            "description: \"Reads *one* user. <script>alert(1)</script>\"\ndocs:\n  description: Needs `admin`.\n",
        ),
    );
    write(
        ws.path(),
        "users/x.rman.yaml",
        &common::request("<img src=x onerror=alert(1)>", "GET", "/x", ""),
    );
    build_site(ws.path());
    let site = ws.path().join("site");
    let page = read(&site, "endpoints/users-get.html");
    assert!(page.contains("<em>one</em>"), "{page}");
    assert!(page.contains("<code>admin</code>"), "{page}");
    assert!(!page.contains("<script>alert"), "{page}");
    for (name, bytes) in files(&site) {
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("<img src=x"), "{name}");
    }
    let index = read(&site, "index.html");
    assert!(index.contains("<strong>accounts</strong>"), "{index}");
}

#[test]
fn no_output_file_refers_outside_the_site() {
    let ws = workspace("logo = \"brand/logo.png\"\n");
    write(ws.path(), "brand/logo.png", "png-bytes");
    build_site(ws.path());
    let user_links = ["https://guide.example/users"];
    for (name, bytes) in files(&ws.path().join("site")) {
        let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
        for attr in ["src=", "href=", "url("] {
            for (at, _) in text.match_indices(attr) {
                let value = text[at + attr.len()..].trim_start_matches(['"', '\'', ' ']);
                let external = value.starts_with("http://")
                    || value.starts_with("https://")
                    || value.starts_with("//");
                let allowed = attr == "href=" && user_links.iter().any(|l| value.starts_with(l));
                assert!(
                    !external || allowed,
                    "{name}: {}",
                    &value[..value.len().min(60)]
                );
            }
        }
    }
    let index = read(&ws.path().join("site"), "index.html");
    assert!(index.contains("src=\"assets/logo."), "{index}");
}

#[test]
fn navigation_and_content_need_no_javascript() {
    let ws = workspace("");
    build_site(ws.path());
    let page = read(&ws.path().join("site"), "endpoints/users-create.html");
    let without_scripts = strip_scripts(&page);
    assert!(without_scripts.contains("<nav class=\"side\""));
    assert!(without_scripts.contains("href=\"../endpoints/users-get.html\""));
    assert!(without_scripts.contains("href=\"../index.html#auth\""));
    let index = strip_scripts(&read(&ws.path().join("site"), "index.html"));
    assert!(
        index.contains("href=\"endpoints/auth-login.html\""),
        "the index lists every page"
    );
    assert!(visible_text(&without_scripts).contains("POST {{base_url}}/users"));
    assert!(
        page.contains("<div id=\"search\" hidden>"),
        "search waits for JavaScript"
    );
    assert!(page.contains("id=\"theme-toggle\" type=\"button\" hidden"));
}

fn strip_scripts(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(start) = rest.find("<script") {
        out.push_str(&rest[..start]);
        let end = rest[start..]
            .find("</script>")
            .map_or(rest.len(), |e| start + e + 9);
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

#[test]
fn a_foreign_output_folder_needs_force() {
    let ws = workspace("");
    write(ws.path(), "site/notes.txt", "mine");
    let err = build_with(ws.path(), |_| {}).unwrap_err();
    assert_eq!(
        err.to_string(),
        "site/ is not empty and was not generated by rustman docs; use --force"
    );
    assert_eq!(err.exit_code(), 2);
    assert!(!ws.path().join("site/index.html").exists());
    build_with(ws.path(), |o| o.force = true).unwrap();
    assert!(ws.path().join("site/notes.txt").exists());
    build_with(ws.path(), |_| {}).expect("a folder with the manifest is ours");
}

#[test]
fn rebuilding_replaces_only_generated_files() {
    let ws = workspace("");
    build_site(ws.path());
    write(ws.path(), "site/CNAME", "docs.example");
    fs::remove_file(ws.path().join("users/delete.rman.yaml")).unwrap();
    build_site(ws.path());
    let site = ws.path().join("site");
    assert!(!site.join("endpoints/users-delete.html").exists());
    assert!(site.join("CNAME").exists());
}

#[test]
fn two_builds_are_byte_identical() {
    let ws = workspace("");
    build_with(ws.path(), |o| o.out = ws.path().join("a")).unwrap();
    build_with(ws.path(), |o| o.out = ws.path().join("b")).unwrap();
    let a = files(&ws.path().join("a"));
    let b = files(&ws.path().join("b"));
    assert_eq!(a, b);
    for (name, bytes) in &a {
        assert!(!bytes.contains(&b'\r'), "{name} has CR");
    }
}

#[test]
fn the_secret_never_reaches_an_output_file() {
    let ws = workspace("");
    write(
        ws.path(),
        "users/echo.rman.yaml",
        &common::request(
            "Echo",
            "POST",
            "{{base_url}}/echo",
            &format!(
                "description: 'Token {TOKEN} pasted here'\nbody:\n  type: text\n  content: 'k={TOKEN}'\n"
            ),
        ),
    );
    build_site(ws.path());
    for (name, bytes) in files(&ws.path().join("site")) {
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("CANARY-tok"), "{name}");
    }
    let page = read(&ws.path().join("site"), "endpoints/users-echo.html");
    assert!(page.contains("«redacted:token»"), "{page}");
    let delete = visible_text(&read(
        &ws.path().join("site"),
        "endpoints/users-delete.html",
    ));
    assert!(
        delete.contains("X-Api-Key «redacted:X-Api-Key»"),
        "{delete}"
    );
}

#[test]
fn literal_credentials_never_reach_an_output_file() {
    let ws = workspace("");
    write(
        ws.path(),
        "users/keyed.rman.yaml",
        &common::request(
            "Keyed",
            "POST",
            "{{base_url}}/keyed",
            "headers:\n  - name: X-Api-Key\n    value: hardcoded-literal-key-123\n  - name: X-Custom-Token\n    value: LITERAL-custom-cred-456\n",
        ),
    );
    write(
        ws.path(),
        "users/apikey.rman.yaml",
        &common::request(
            "Api key",
            "GET",
            "{{base_url}}/apikey",
            "auth:\n  type: api_key\n  name: X-Api-Key\n  value: literal-apikey-value-777\n",
        ),
    );
    write(
        ws.path(),
        "users/query-key.rman.yaml",
        &common::request(
            "Query key",
            "GET",
            "{{base_url}}/q",
            "auth:\n  type: api_key\n  name: access\n  value: literal-query-key-888\n  in: query\n",
        ),
    );
    build_with(ws.path(), |o| o.format = rustman_docs::Format::Both).unwrap();
    for (name, bytes) in files(&ws.path().join("site")) {
        let text = String::from_utf8_lossy(&bytes);
        for literal in [
            "hardcoded-literal-key-123",
            "LITERAL-custom-cred-456",
            "literal-apikey-value-777",
            "literal-query-key-888",
        ] {
            assert!(!text.contains(literal), "{literal} in {name}");
        }
    }
    let page = read(&ws.path().join("site"), "endpoints/users-keyed.html");
    assert!(page.contains("X-Api-Key: «redacted:X-Api-Key»"), "{page}");
    assert!(page.contains("«redacted:X-Custom-Token»"), "{page}");
}

/// Review regression: credentials written as literals in URL userinfo, a
/// credential-named query parameter or a credential-named body field are
/// published in no file (pages, samples, search index, examples).
#[test]
fn literal_url_query_and_body_credentials_never_reach_an_output_file() {
    let ws = workspace("");
    write(
        ws.path(),
        "evil/r.rman.yaml",
        &common::request(
            "R",
            "POST",
            "https://user:pw-literal-123@api.example/x?api_key=querysecret123&page=2",
            "docs:\n  example: true\nbody:\n  type: json\n  content:\n    name: ada\n    password: hunter2literal\n",
        ),
    );
    write(
        ws.path(),
        "evil/form.rman.yaml",
        &common::request(
            "Form",
            "POST",
            "{{base_url}}/form",
            "query:\n  - name: token\n    value: listedquerytoken9\nbody:\n  type: form\n  fields:\n    - name: client_secret\n      value: formsecret-777\n",
        ),
    );
    common::record(
        ws.path(),
        "01JDQ3XG000000000000000009",
        "evil/r.rman.yaml",
        200,
        "{\"echo\":\"hunter2literal querysecret123 pw-literal-123\"}",
        serde_json::json!([]),
    );
    build_with(ws.path(), |o| o.format = rustman_docs::Format::Both).unwrap();
    let site = files(&ws.path().join("site"));
    assert!(site.contains_key("search-index.json"));
    for (name, bytes) in &site {
        let text = String::from_utf8_lossy(bytes);
        for literal in [
            "user:pw-literal-123",
            "pw-literal-123",
            "querysecret123",
            "hunter2literal",
            "listedquerytoken9",
            "formsecret-777",
        ] {
            assert!(!text.contains(literal), "{literal} in {name}");
        }
    }
    let page = read(&ws.path().join("site"), "endpoints/evil-r.html");
    assert!(page.contains("«redacted:userinfo»@api.example"), "{page}");
    assert!(page.contains("api_key=«redacted:api_key»"), "{page}");
    assert!(page.contains("«redacted:password»"), "{page}");
    assert!(page.contains("page=2"), "{page}");
    assert!(page.contains("Example response"), "{page}");
}

/// Review regression: an SVG logo could run script on the docs origin
/// when a static host serves it, so only raster images are accepted.
#[test]
fn the_logo_must_be_a_raster_image() {
    for name in ["logo.svg", "logo.SVG", "logo.html", "logo"] {
        let ws = workspace(&format!("logo = \"brand/{name}\"\n"));
        write(
            ws.path(),
            &format!("brand/{name}"),
            "<svg onload='alert(1)'/>",
        );
        let err = build_with(ws.path(), |_| {}).unwrap_err();
        assert!(
            matches!(err, rustman_docs::DocsError::Usage(_)),
            "{name}: {err}"
        );
        assert!(err.to_string().contains("SVG is refused"), "{err}");
        assert!(!ws.path().join("site").exists(), "{name}");
    }
    for name in [
        "logo.png",
        "logo.JPG",
        "logo.jpeg",
        "logo.gif",
        "logo.webp",
        "logo.ico",
    ] {
        let ws = workspace(&format!("logo = \"brand/{name}\"\n"));
        write(ws.path(), &format!("brand/{name}"), "image-bytes");
        build_with(ws.path(), |_| {}).unwrap();
    }
}
