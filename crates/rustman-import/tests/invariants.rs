//! The two import invariants, over every input of the security reviews
//! (`tests/fixtures/review`):
//!
//! 1. no secret-like secret value, as written or percent-, JSON- or
//!    HTML-encoded, appears in a workspace file outside `.rustman/`, the
//!    JSON report or the printed summary;
//! 2. every request sends exactly the bytes the source request would, under
//!    every imported environment, or visibly holds `«redacted:…»`.

mod common;

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use common::{Setup, apply, fixture, read_tree};
use rustman_core::Workspace;
use rustman_core::model::{Body, JsonContent, Request};
use rustman_import::ir::{Folder, ImportedBody, ImportedCollection, ImportedRequest, Item, Pair};
use rustman_import::secrets::is_secret_like;
use rustman_import::{Plan, detect};

const INPUTS: &[&str] = &[
    "a1.json",
    "a2.json",
    "env3.json",
    "ref.json",
    "n_b64.json",
    "n_braces.json",
    "n_inword_tok.json",
    "n_mixhex.json",
    "n_partial.json",
    "n_plus.json",
    "n_uesc.json",
    "n_xenv.json",
];

/// The environment the import's own secrets go to ([`Setup`]'s default).
const TARGET: &str = "local";

/// Stored secrets: (environment, name, value).
type Stored = Vec<(String, String, String)>;

/// Import `name` into a fresh workspace; returns the workspace, the plan,
/// the stored secrets and the source as parsed (variables translated).
fn import(name: &str) -> (tempfile::TempDir, Plan, Stored, ImportedCollection) {
    let text = fixture(&format!("review/{name}"));
    let format = detect(&text).unwrap_or_else(|| panic!("{name} is detected"));
    let mut source = format.parse(&text, name).unwrap();
    rustman_import::translate::translate(&mut source);
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("rustman.toml"),
        "[workspace]\nname = \"t\"\nversion = 1\n",
    )
    .unwrap();
    let plan = Setup::new(dir.path()).plan_collection(format, source.clone(), name);
    let store = Mutex::new(Vec::new());
    apply(&plan, dir.path(), &store);
    let stored = store.into_inner().unwrap();
    (dir, plan, stored, source)
}

/// `value` as written and in the encodings a request may carry it in.
fn forms(value: &str) -> Vec<String> {
    let mut percent = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            percent.push(char::from(byte));
        } else {
            percent.push_str(&format!("%{byte:02X}"));
        }
    }
    let json = serde_json::to_string(value).unwrap();
    let json = json[1..json.len() - 1].to_owned();
    let html = value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;");
    let mut out = vec![
        value.to_owned(),
        percent.to_ascii_lowercase(),
        json.replace('/', "\\/"),
        percent,
        json,
        html,
    ];
    out.dedup();
    out
}

#[test]
fn no_secret_in_any_form_reaches_a_file_the_report_or_the_output() {
    for name in INPUTS {
        let (dir, plan, stored, _) = import(name);
        let mut outputs: Vec<(String, String)> = read_tree(dir.path())
            .into_iter()
            .filter(|(path, _)| !path.starts_with(".rustman/"))
            .map(|(path, bytes)| (path, String::from_utf8(bytes).unwrap()))
            .collect();
        outputs.push(("report".into(), plan.report.to_json()));
        outputs.push(("summary".into(), plan.report.summary_text()));
        for (_, secret, value) in stored.iter().filter(|s| is_secret_like(&s.2)) {
            for form in forms(value) {
                for (path, text) in &outputs {
                    assert!(
                        !text.contains(&form),
                        "{name}: secret {secret} ({form}) in {path}:\n{text}"
                    );
                }
            }
            // Partly or differently encoded: decode each output first.
            for (path, text) in &outputs {
                for decoded in decodings(text) {
                    assert!(
                        !decoded.contains(value.as_str()),
                        "{name}: secret {secret} encoded in {path}:\n{text}"
                    );
                }
            }
        }
    }
}

/// `text` percent-decoded (`+` as a space), JSON-unescaped (`\uXXXX`
/// too) and HTML-unescaped.
fn decodings(text: &str) -> Vec<String> {
    let mut percent = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(byte) => {
                percent.push(byte);
                i += 3;
            }
            None => {
                percent.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
                i += 1;
            }
        }
    }
    let mut json = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            json.push(c);
            continue;
        }
        match chars.next() {
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                if let Some(decoded) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    json.push(decoded);
                }
            }
            Some(other) => json.push(other),
            None => {}
        }
    }
    let html = text
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#64;", "@");
    vec![String::from_utf8_lossy(&percent).into_owned(), json, html]
}

/// A variable lookup: the first scope that defines the name wins.
type Scopes = Vec<HashMap<String, String>>;

/// `text` with each `{{name}}` the scopes define replaced, once; others
/// stay as written.
fn substitute(text: &str, scopes: &Scopes) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let inner = &rest[start + 2..];
        let Some(end) = inner.find("}}") else {
            break;
        };
        out.push_str(&rest[..start]);
        let name = inner[..end].trim();
        match scopes.iter().find_map(|s| s.get(name)) {
            Some(value) => out.push_str(value),
            None => out.push_str(&rest[start..start + end + 4]),
        }
        rest = &inner[end + 2..];
    }
    out.push_str(rest);
    out
}

/// What a request sends that the import may change: URL, query, its own
/// headers and a text or form body, one entry per field.
fn fields_of_source(r: &ImportedRequest) -> Vec<String> {
    let pairs = |pairs: &[Pair]| -> Vec<String> {
        pairs
            .iter()
            .map(|p| format!("{}={}", p.name, p.value))
            .collect()
    };
    let mut out = vec![r.url.clone()];
    out.extend(pairs(&r.query));
    out.extend(pairs(&r.headers));
    match &r.body {
        Some(ImportedBody::Json { text } | ImportedBody::Text { text }) => out.push(text.clone()),
        Some(ImportedBody::Form { fields }) => out.extend(pairs(fields)),
        _ => {}
    }
    out
}

fn fields_of_written(r: &Request) -> Vec<String> {
    let Request::Http(http) = r else {
        return Vec::new();
    };
    let pairs = |pairs: &[rustman_core::model::NameValue]| -> Vec<String> {
        pairs
            .iter()
            .map(|p| format!("{}={}", p.name, p.value))
            .collect()
    };
    let mut out = vec![http.url.clone()];
    out.extend(pairs(http.query.as_deref().unwrap_or_default()));
    out.extend(pairs(http.headers.as_deref().unwrap_or_default()));
    match &http.body {
        Some(Body::Json(JsonContent::Text(text)) | Body::Text(text)) => out.push(text.clone()),
        Some(Body::Json(JsonContent::Value(value))) => out.push(value.to_string()),
        Some(Body::Form(fields)) => out.extend(pairs(fields)),
        _ => {}
    }
    out
}

/// The source's requests, depth first (the order files are planned in),
/// with the variable scopes above each: nearest folder first.
fn source_requests(folder: &Folder, above: &Scopes, out: &mut Vec<(ImportedRequest, Scopes)>) {
    let mut scopes = vec![
        folder
            .variables
            .iter()
            .map(|v| (v.name.clone(), v.value.clone()))
            .collect(),
    ];
    scopes.extend(above.iter().cloned());
    for item in &folder.items {
        match item {
            Item::Folder(child) => source_requests(child, &scopes, out),
            Item::Request(r) => out.push(((**r).clone(), scopes.clone())),
        }
    }
}

fn to_map<'a>(entries: impl Iterator<Item = (&'a str, String)>) -> HashMap<String, String> {
    entries.map(|(k, v)| (k.to_owned(), v)).collect()
}

#[test]
fn every_request_sends_its_original_bytes_or_shows_a_redaction() {
    for name in INPUTS {
        let (dir, plan, stored, source) = import(name);
        let ws = Workspace::load(dir.path()).unwrap();
        let secrets_of = |env: &str| {
            to_map(
                stored
                    .iter()
                    .filter(|s| s.0 == env)
                    .map(|s| (s.1.as_str(), s.2.clone())),
            )
        };
        let mut sources = Vec::new();
        source_requests(&source.root, &Vec::new(), &mut sources);
        let files: Vec<&str> = plan
            .files
            .iter()
            .filter(|f| f.path.ends_with(".rman.yaml"))
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(files.len(), sources.len(), "{name}");
        let mut envs: Vec<&str> = ws.environments.iter().map(|e| e.name.as_str()).collect();
        if !envs.contains(&TARGET) {
            envs.push(TARGET);
        }
        for (file, (original, above)) in files.iter().zip(&sources) {
            let request = ws
                .requests()
                .into_iter()
                .find(|r| r.entry.path == Path::new(file))
                .unwrap_or_else(|| panic!("{name}: {file} not loaded"));
            for env in &envs {
                // Written: request, collections (nearest first), the
                // environment, then the import's own secrets.
                let mut written: Scopes = Vec::new();
                if let Some(vars) = request.request().variables() {
                    written.push(to_map(vars.iter().map(|(k, v)| (k, v.to_string()))));
                }
                for vars in request.collection_variables() {
                    written.push(to_map(vars.iter().map(|(k, v)| (k, v.to_string()))));
                }
                if let Some(e) = ws.environments.iter().find(|e| e.name == *env) {
                    written.push(to_map(e.variables.iter().map(|(k, v)| (k, v.to_string()))));
                }
                written.push(secrets_of(env));
                written.push(secrets_of(TARGET));
                // Source: request, folders, the source environment.
                let mut scopes: Scopes = vec![to_map(
                    original
                        .variables
                        .iter()
                        .map(|v| (v.name.as_str(), v.value.clone())),
                )];
                scopes.extend(above.iter().cloned());
                let source_env = source
                    .environments
                    .iter()
                    .find(|e| rustman_import::secrets::environment_name(&e.name) == *env);
                if let Some(e) = source_env {
                    scopes.push(to_map(
                        e.variables
                            .iter()
                            .map(|v| (v.name.as_str(), v.value.clone())),
                    ));
                }
                let sent: Vec<String> = fields_of_written(request.request())
                    .iter()
                    .map(|f| substitute(f, &written))
                    .collect();
                let expected: Vec<String> = fields_of_source(original)
                    .iter()
                    .map(|f| substitute(f, &scopes))
                    .collect();
                for (i, field) in sent.iter().enumerate() {
                    let same = expected.get(i) == Some(field);
                    assert!(
                        same || field.contains("«redacted:"),
                        "{name}: {file} under {env}: sends {field:?}, the source {:?}",
                        expected.get(i)
                    );
                }
            }
        }
    }
}

/// The written file at `path` of an import.
fn written(plan: &Plan, path: &str) -> String {
    plan.files
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("no {path}"))
        .text
        .clone()
}

fn notes(plan: &Plan) -> Vec<String> {
    plan.report
        .notes
        .iter()
        .map(|n| n.message.clone())
        .collect()
}

#[test]
fn each_review_input_gets_its_rule() {
    // A: an environment's own secret on token boundaries in its own file.
    let (_, plan, _, _) = import("env3.json");
    let prod = written(&plan, "environments/prod.yaml");
    assert!(
        prod.contains("cb_url: https://x.test/cb?t={{token}}"),
        "{prod}"
    );
    assert!(prod.contains("copy: '{{token}}'"), "{prod}");
    // B: an environment's secret repeated in a request is redacted.
    let (_, plan, _, _) = import("n_xenv.json");
    let copy = written(&plan, "xenv/copy.rman.yaml");
    assert!(copy.contains("value: «redacted:token»"), "{copy}");
    assert!(notes(&plan).iter().any(|n| n.ends_with(
        "field header X-Copy repeated secret token of environment prod; it was redacted, edit the field"
    )), "{:?}", notes(&plan));
    // C: a secret holding `{{` is matched as text.
    let (_, plan, stored, _) = import("n_braces.json");
    assert!(written(&plan, "nbr/br.rman.yaml").contains("value: '{{password}}'"));
    assert!(stored.iter().any(|s| s.2 == "ab{{cd}}ef-123"));
    // D: a token right after letters.
    let (_, plan, stored, _) = import("n_inword_tok.json");
    let tok = written(&plan, "nt/tok.rman.yaml");
    assert!(tok.contains("value: '{{x_id}}'"), "{tok}");
    assert!(
        stored
            .iter()
            .any(|s| s.2.contains("ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8"))
    );
    assert!(rustman_import::secrets::is_known_token(
        "xxghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8yy"
    ));
    // E: partly, differently, `+` and `\u` encoded repeats: redacted (the
    // whole field when only a decoded variant shows the secret).
    for (input, path) in [
        ("n_partial.json", "np/partial.rman.yaml"),
        ("n_mixhex.json", "nm/mix.rman.yaml"),
        ("n_plus.json", "ns/plus.rman.yaml"),
        ("n_uesc.json", "nu/uesc.rman.yaml"),
    ] {
        let (_, plan, _, _) = import(input);
        let text = written(&plan, path);
        assert!(text.contains("«redacted:password»"), "{input}: {text}");
        assert!(
            !text.contains("w0rd") && !text.contains("secret+pass"),
            "{input}: {text}"
        );
        assert!(
            notes(&plan)
                .iter()
                .any(|n| n.contains("contained secret password in encoded form")),
            "{input}: {:?}",
            notes(&plan)
        );
    }
    // Bare base64 is a documented gap: only the `Basic` header is found.
    let (_, plan, _, _) = import("n_b64.json");
    let b64 = written(&plan, "nb/b64.rman.yaml");
    assert!(b64.contains("value: Basic {{x_legacy}}"), "{b64}");
    assert!(b64.contains("value: UzNjcjN0LVBhc3Mh"), "{b64}");
}
