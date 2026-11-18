//! Every example request in docs/auth.md loads, is written back unchanged,
//! and the page shows each signature auth type (runner-parity task 3.7).

use std::fs;
use std::path::Path;

use rustman_core::format::write::request_to_yaml;
use rustman_core::model::{Auth, KindRegistry, Request};

/// `(path, body)` of every ```yaml block that starts with `# <path>`.
fn examples() -> Vec<(String, String)> {
    let doc = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/auth.md");
    let text = fs::read_to_string(doc).unwrap().replace("\r\n", "\n");
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line != "```yaml" {
            continue;
        }
        let block: Vec<&str> = lines.by_ref().take_while(|l| *l != "```").collect();
        let path = block[0]
            .strip_prefix("# ")
            .expect("example starts with '# <path>'");
        let body = block[1..].iter().map(|l| format!("{l}\n")).collect();
        out.push((path.to_owned(), body));
    }
    out
}

#[test]
fn every_auth_example_loads_and_is_canonical() {
    let registry = KindRegistry::default();
    let mut kinds = Vec::new();
    for (path, body) in examples() {
        assert!(path.ends_with(".rman.yaml"), "{path}");
        let request = registry
            .load(Path::new(&path), &body)
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(request_to_yaml(&request), body, "{path} is not canonical");
        let Request::Http(http) = request else {
            panic!("{path} is not an HTTP request");
        };
        kinds.push(match http.auth {
            Some(Auth::Digest { .. }) => "digest",
            Some(Auth::AwsSigV4(_)) => "aws_sigv4",
            Some(Auth::Jwt(_)) => "jwt",
            other => panic!("{path}: unexpected auth {other:?}"),
        });
    }
    for kind in ["digest", "aws_sigv4", "jwt"] {
        assert!(kinds.contains(&kind), "no {kind} example");
    }
}
