//! The AWS SigV4 test suite (`tests/fixtures/aws-sigv4-test-suite`): every
//! header-signing case is signed with the suite's credentials and time, and
//! must give the suite's signature and signed headers. The signature is an
//! HMAC over the string to sign, which holds the SHA-256 of the canonical
//! request, so a matching signature means both match byte for byte; the
//! suite's own files are checked to agree with each other too.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use aws_sigv4::http_request::SignableBody;
use ring::digest::{SHA256, digest};
use serde_json::Value;

use super::{Keys, Parts, Scope, signed_headers, suite_settings};

fn suite_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/aws-sigv4-test-suite")
}

/// A suite file with LF line endings (a Windows checkout may add CRs).
fn read(case: &Path, name: &str) -> String {
    let text = std::fs::read_to_string(case.join(name)).unwrap();
    text.replace("\r\n", "\n").trim().to_owned()
}

/// A request file: `METHOD target HTTP/1.1`, headers (`Host` gives the
/// URI's authority; a line starting with whitespace folds into the value
/// before it), a blank line and the body.
struct SuiteRequest {
    method: String,
    uri: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn parse_request(text: &str) -> SuiteRequest {
    let (head, body) = text.split_once("\n\n").unwrap_or((text, ""));
    let mut lines = head.lines();
    let start: Vec<&str> = lines.next().unwrap().split(' ').collect();
    let target = wire_target(&start[1..start.len() - 1].join(" "));
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines {
        if line.starts_with([' ', '\t']) {
            let last = headers.last_mut().unwrap();
            // An obsolete line folding reads as a space (RFC 9112 5.2).
            last.1.push(' ');
            last.1.push_str(line.trim_start());
        } else {
            let (name, value) = line.split_once(':').unwrap();
            headers.push((name.to_owned(), value.to_owned()));
        }
    }
    let host_at = headers
        .iter()
        .position(|(n, _)| n.eq_ignore_ascii_case("host"))
        .unwrap();
    let (_, host) = headers.remove(host_at);
    SuiteRequest {
        method: start[0].to_owned(),
        uri: format!("https://{host}{target}"),
        headers,
        body: body.as_bytes().to_vec(),
    }
}

/// The request target as it goes on the wire: spaces and non-ASCII bytes
/// cannot be sent raw, so they are percent-encoded (`/ሴ` is `/%E1%88%B4`).
fn wire_target(target: &str) -> String {
    let mut out = String::new();
    for byte in target.bytes() {
        if byte == b' ' || !byte.is_ascii() {
            out.push_str(&format!("%{byte:02X}"));
        } else {
            out.push(char::from(byte));
        }
    }
    out
}

fn sign_case(case: &Path) -> (super::Signed, Value) {
    let context: Value = serde_json::from_str(&read(case, "context.json")).unwrap();
    let credentials = &context["credentials"];
    let keys = Keys {
        access_key: credentials["access_key_id"].as_str().unwrap().to_owned(),
        secret_key: credentials["secret_access_key"]
            .as_str()
            .unwrap()
            .to_owned(),
        session_token: credentials["token"].as_str().map(str::to_owned),
    };
    let flag = |name: &str| context[name].as_bool().unwrap_or(false);
    let settings = suite_settings(
        flag("normalize"),
        flag("sign_body"),
        flag("omit_session_token"),
    );
    // Every case is signed at 2015-08-30T12:36:00Z.
    assert_eq!(context["timestamp"], "2015-08-30T12:36:00Z");
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_440_938_160);
    let request = parse_request(&read(case, "request.txt"));
    let parts = Parts {
        method: &request.method,
        uri: &request.uri,
        headers: &request.headers,
        body: SignableBody::Bytes(&request.body),
    };
    let scope = Scope {
        region: context["region"].as_str().unwrap(),
        service: context["service"].as_str().unwrap(),
    };
    let signed = signed_headers(&parts, &keys, scope, settings, time)
        .unwrap_or_else(|e| panic!("{}: {e}", case.display()));
    (signed, context)
}

/// The headers `header-signed-request.txt` has beyond `request.txt`'s.
fn expected_signature_headers(case: &Path) -> Vec<(String, String)> {
    let before = parse_request(&read(case, "request.txt")).headers;
    let mut added: Vec<(String, String)> = parse_request(&read(case, "header-signed-request.txt"))
        .headers
        .into_iter()
        .filter(|h| !before.contains(h))
        .map(|(n, v)| (n.to_ascii_lowercase(), v))
        .collect();
    added.sort();
    added
}

#[test]
fn every_header_case_of_the_aws_suite_matches() {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(suite_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    cases.sort();
    assert!(cases.len() >= 30, "the suite has {} cases", cases.len());
    for case in &cases {
        let name = case.file_name().unwrap().to_string_lossy();
        let canonical = read(case, "header-canonical-request.txt");
        let string_to_sign = read(case, "header-string-to-sign.txt");
        let hash = digest(&SHA256, canonical.as_bytes());
        let hex: String = hash.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        assert!(
            string_to_sign.ends_with(&hex),
            "{name}: suite files disagree"
        );

        let (signed, _) = sign_case(case);
        assert_eq!(
            signed.signature,
            read(case, "header-signature.txt"),
            "{name}"
        );
        let mut added: Vec<(String, String)> = signed
            .headers
            .into_iter()
            .map(|(n, v)| (n.to_ascii_lowercase(), v))
            .collect();
        added.sort();
        assert_eq!(added, expected_signature_headers(case), "{name}");
    }
}

#[test]
fn the_named_cases_are_present() {
    for case in [
        "get-vanilla",
        "post-x-www-form-urlencoded",
        "get-utf8",
        "get-vanilla-with-session-token",
    ] {
        assert!(suite_dir().join(case).is_dir(), "{case}");
    }
}
