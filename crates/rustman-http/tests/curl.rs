//! Curl import and export (task 4.11).

mod support;

use proptest::prelude::*;
use rustman_core::Secret;
use rustman_core::format::Located;
use rustman_core::model::{
    ApiKeyLocation, Auth, Body, JsonContent, NameValue, Part, PartSource, RequestSettings,
};
use rustman_core::vars::{
    Resolved, ResolvedBody, ResolvedFile, ResolvedPart, ResolvedPartSource, ResolvedRequest,
    SecretSpan,
};
use rustman_http::{Imported, export_curl, import_curl, import_curl_words};
use support::request::{get, pair};

fn import(command: &str) -> Imported {
    import_curl(command).unwrap()
}

fn headers(imported: &Imported) -> Vec<(String, String)> {
    imported
        .request
        .headers
        .iter()
        .flatten()
        .map(|h| (h.name.clone(), h.value.clone()))
        .collect()
}

fn header(name: &str, value: &str) -> (String, String) {
    (name.to_owned(), value.to_owned())
}

#[test]
fn imports_a_post_with_a_json_body() {
    let imported =
        import(r#"curl -X POST https://x.io/a -H 'Content-Type: application/json' -d '{"a":1}'"#);
    let r = &imported.request;
    assert_eq!(r.effective_method(), "POST");
    assert_eq!(r.url, "https://x.io/a");
    assert_eq!(
        r.body,
        Some(Body::Json(JsonContent::Value(serde_json::json!({"a": 1}))))
    );
    assert!(r.headers.is_none(), "{:?}", r.headers);
    assert!(imported.warnings.is_empty());
    assert_eq!(r.name, "POST /a");
    let yaml = rustman_core::format::write::request_to_yaml(&rustman_core::model::Request::Http(
        Box::new(r.clone()),
    ));
    assert!(
        yaml.contains("method: POST\nurl: https://x.io/a\n"),
        "{yaml}"
    );
    assert!(
        yaml.contains("body:\n  type: json\n  content:\n    a: 1\n"),
        "{yaml}"
    );
}

#[test]
fn unsupported_options_are_warnings_and_their_values_are_skipped() {
    let imported = import("curl --limit-rate 1k -s https://x.io/a -o out.txt");
    assert_eq!(imported.request.url, "https://x.io/a");
    assert_eq!(
        imported.warnings,
        [
            "ignored unsupported curl option --limit-rate",
            "ignored unsupported curl option -s",
            "ignored unsupported curl option -o",
        ]
    );
}

#[test]
fn imports_method_url_and_headers() {
    let imported = import("curl -XPUT --url x.io/p -H 'X-A: 1' --header 'X-B:  two words '");
    assert_eq!(imported.request.effective_method(), "PUT");
    assert_eq!(imported.request.url, "http://x.io/p");
    assert_eq!(
        headers(&imported),
        [header("X-A", "1"), header("X-B", "two words")]
    );
    let get = import("curl --request DELETE https://x.io");
    assert_eq!(get.request.effective_method(), "DELETE");
}

#[test]
fn data_without_a_content_type_is_a_form() {
    let imported = import("curl https://x.io -d 'a=1' --data 'b=x%20y'");
    assert_eq!(imported.request.effective_method(), "POST");
    assert_eq!(
        imported.request.body,
        Some(Body::Form(vec![
            NameValue::new("a", "1"),
            NameValue::new("b", "x y")
        ]))
    );
    assert!(imported.request.headers.is_none());
}

#[test]
fn data_that_is_not_a_form_keeps_curls_content_type() {
    let imported = import("curl https://x.io --data-raw @literal");
    assert_eq!(imported.request.body, Some(Body::Text("@literal".into())));
    assert_eq!(
        headers(&imported),
        [header("Content-Type", "application/x-www-form-urlencoded")]
    );
    let typed = import("curl https://x.io -H 'Content-Type: text/plain; charset=utf-8' -d hi");
    assert_eq!(typed.request.body, Some(Body::Text("hi".into())));
    assert!(typed.request.headers.is_none());
}

#[test]
fn data_from_a_file_is_a_file_body() {
    let imported = import("curl https://x.io --data-binary @payload.bin");
    assert_eq!(
        imported.request.body,
        Some(Body::File(Located::new("payload.bin".into())))
    );
    assert_eq!(
        headers(&imported),
        [header("Content-Type", "application/x-www-form-urlencoded")]
    );
    let typed = import(
        "curl https://x.io -H 'Content-Type: application/octet-stream' -d @a.bin --data-ascii x=1",
    );
    assert_eq!(typed.request.body, Some(Body::Text("x=1".into())));
    assert_eq!(
        headers(&typed),
        [header("Content-Type", "application/octet-stream")]
    );
    assert_eq!(
        typed.warnings,
        ["ignored data file @a.bin: it cannot be combined with other data"]
    );
}

#[test]
fn data_urlencode_encodes_the_content() {
    let imported = import(
        "curl https://x.io --data-urlencode 'q=a b&c' --data-urlencode 'n=x y' --data-urlencode n@f",
    );
    assert_eq!(
        imported.request.body,
        Some(Body::Form(vec![
            NameValue::new("q", "a b&c"),
            NameValue::new("n", "x y"),
        ]))
    );
    let bare = import("curl https://x.io --data-urlencode '=x y'");
    assert_eq!(bare.request.body, Some(Body::Text("x+y".into())));
    assert_eq!(
        imported.warnings,
        ["ignored --data-urlencode n@f: reading a file is not supported"]
    );
}

#[test]
fn json_option_sets_the_json_headers() {
    let imported = import(r#"curl https://x.io --json '{"k":[1]}'"#);
    assert_eq!(
        imported.request.body,
        Some(Body::Json(JsonContent::Value(
            serde_json::json!({"k": [1]})
        )))
    );
    assert_eq!(headers(&imported), [header("Accept", "application/json")]);
    let string = import(r#"curl https://x.io --json '"text"'"#);
    assert_eq!(
        string.request.body,
        Some(Body::Json(JsonContent::Text("\"text\"".into())))
    );
}

#[test]
fn form_options_are_multipart_parts() {
    let imported = import(
        r#"curl https://x.io -F 'a=1' -F 'f=@./pic.png;type=image/png' -F 'q="x;y";type=text/csv' --form-string 'l=@not-a-file' -F 'g=<notes.txt;filename=n'"#,
    );
    let Some(Body::Multipart(parts)) = &imported.request.body else {
        panic!("{:?}", imported.request.body);
    };
    let expected = [
        ("a", PartSource::Text("1".into()), None),
        (
            "f",
            PartSource::File(Located::new("./pic.png".into())),
            Some("image/png"),
        ),
        ("q", PartSource::Text("x;y".into()), Some("text/csv")),
        ("l", PartSource::Text("@not-a-file".into()), None),
        (
            "g",
            PartSource::File(Located::new("notes.txt".into())),
            None,
        ),
    ];
    let expected: Vec<Part> = expected
        .into_iter()
        .map(|(name, source, ct)| Part {
            name: name.into(),
            source,
            content_type: ct.map(str::to_owned),
        })
        .collect();
    assert_eq!(parts, &expected);
    assert_eq!(imported.request.effective_method(), "POST");
    assert_eq!(
        imported.warnings,
        [
            "ignored form parameter 'filename=n' of field 'g'",
            "form field 'g' reads its value from a file; it is imported as a file part",
        ]
    );
}

#[test]
fn user_cookie_insecure_location_and_compressed() {
    let imported =
        import("curl -u ana:pw -b 'sid=abc; t=1' -k -L --compressed https://x.io -b jar.txt -sSLk");
    let r = &imported.request;
    assert_eq!(
        r.auth,
        Some(Auth::Basic {
            username: "ana".into(),
            password: "pw".into()
        })
    );
    assert_eq!(
        headers(&imported),
        [
            header("Cookie", "sid=abc; t=1"),
            header("Accept-Encoding", "deflate, gzip, br, zstd"),
        ]
    );
    let settings = r.settings.clone().unwrap();
    assert_eq!(settings.verify_tls, Some(false));
    assert_eq!(settings.follow_redirects, Some(true));
    assert_eq!(
        imported.warnings,
        [
            "ignored cookie file jar.txt: only inline cookies are imported",
            "ignored unsupported curl option -s",
            "ignored unsupported curl option -S",
        ]
    );
    let user_only = import("curl --user ana https://x.io");
    assert_eq!(
        user_only.request.auth,
        Some(Auth::Basic {
            username: "ana".into(),
            password: String::new()
        })
    );
}

#[test]
fn get_puts_the_data_in_the_query() {
    let imported = import("curl -d 'a=1' --data-urlencode 'q=x y' -G https://x.io/s");
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
    let request = &imported.request;
    assert_eq!(request.method.as_ref().unwrap().as_str(), "GET");
    assert_eq!(request.body, None);
    assert_eq!(
        request.query,
        Some(vec![NameValue::new("a", "1"), NameValue::new("q", "x y")])
    );
    let posted = import("curl --get -X POST -d a=1 https://x.io");
    assert_eq!(posted.request.method.as_ref().unwrap().as_str(), "POST");
    let file = import("curl -G -d @q.txt https://x.io");
    assert_eq!(file.request.query, None);
    assert_eq!(
        file.warnings,
        ["ignored data file @q.txt: -G cannot read a file into the query"]
    );
}

#[test]
fn line_continuations_and_split_words_are_accepted() {
    let imported = import("curl https://x.io/a \\\n  -H 'X-A: 1'");
    assert_eq!(headers(&imported), [header("X-A", "1")]);
    let words: Vec<String> = ["-X", "PATCH", "https://x.io/b"].map(String::from).to_vec();
    let imported = import_curl_words(&words).unwrap();
    assert_eq!(imported.request.effective_method(), "PATCH");
}

#[test]
fn malformed_commands_are_errors() {
    let err = |command: &str| import_curl(command).unwrap_err().to_string();
    assert_eq!(err("curl -H"), "curl: option -H needs a value");
    assert_eq!(err("curl -k"), "curl: no URL in the command");
    assert!(err("curl 'open").starts_with("curl: cannot split the command"));
    assert_eq!(
        err("curl -F novalue x.io"),
        "curl: form field without '=': novalue"
    );
    assert_eq!(
        err("curl -X 'GET X' x.io"),
        "curl: invalid HTTP method 'GET X'"
    );
    let extra = import("curl a.io b.io -H 'X-Empty:'");
    assert_eq!(
        extra.warnings,
        [
            "ignored extra URL b.io",
            "ignored header without a value: X-Empty:"
        ]
    );
}

fn bearer_request(token: &str) -> ResolvedRequest {
    let mut request = get("https://api.example.com/me");
    request.auth = Some(Auth::Bearer {
        token: token.into(),
    });
    request.secrets = vec![Secret::new("api_token", token.to_owned())];
    request
}

#[test]
fn export_redacts_secrets() {
    let exported = export_curl(&bearer_request("s3cr3t-tok"), false);
    assert_eq!(
        exported.command,
        "curl 'https://api.example.com/me' -H 'Authorization: «redacted:api_token»' -L"
    );
    assert!(!exported.command.contains("s3cr3t-tok"));
    let shown = export_curl(&bearer_request("s3cr3t-tok"), true);
    assert_eq!(
        shown.command,
        "curl 'https://api.example.com/me' -H 'Authorization: Bearer s3cr3t-tok' -L"
    );
}

#[test]
fn export_redacts_secrets_everywhere() {
    let mut request = get("https://h/x");
    request.method = "PUT".into();
    let secret = "k-123";
    request.secrets = vec![Secret::new("key", secret.to_owned())];
    request.query = vec![pair("token", secret)];
    request.headers = vec![
        rustman_core::vars::ResolvedPair {
            name: Resolved::literal("X-Key"),
            value: Resolved {
                text: format!("v-{secret}"),
                secret_spans: vec![SecretSpan {
                    name: "key".into(),
                    range: 2..7,
                }],
            },
        },
        pair("Cookie", "plain=1"),
    ];
    request.body = ResolvedBody::Json(serde_json::json!({"k": secret}));
    request.auth = Some(Auth::Basic {
        username: "u".into(),
        password: secret.into(),
    });
    request.settings = RequestSettings {
        verify_tls: Some(false),
        follow_redirects: Some(false),
        ..RequestSettings::default()
    };
    let exported = export_curl(&request, false);
    assert!(!exported.command.contains(secret), "{}", exported.command);
    assert_eq!(
        exported.command,
        "curl -X PUT 'https://h/x?token=«redacted:key»' -u 'u:«redacted:key»' \
         -H 'X-Key: v-«redacted:key»' -H 'Cookie: «redacted:Cookie»' \
         -H 'Content-Type: application/json' --data-raw '{\"k\":\"«redacted:key»\"}' -k"
    );
}

#[test]
fn export_covers_api_keys_files_multipart_and_oauth2() {
    let mut request = get("https://h/");
    request.method = "POST".into();
    request.auth = Some(Auth::ApiKey {
        name: "X-Api-Key".into(),
        value: "abc".into(),
        location: Some(ApiKeyLocation::Header),
    });
    request.body = ResolvedBody::File(ResolvedFile {
        raw: "data/a b.bin".into(),
        path: "/ws/data/a b.bin".into(),
    });
    assert_eq!(
        export_curl(&request, false).command,
        "curl 'https://h/' -H 'X-Api-Key: abc' -H 'Content-Type: application/octet-stream' \
         --data-binary '@data/a b.bin' -L"
    );
    request.auth = Some(Auth::ApiKey {
        name: "key".into(),
        value: "a b".into(),
        location: Some(ApiKeyLocation::Query),
    });
    request.body = ResolvedBody::Multipart(vec![
        ResolvedPart {
            name: "f".into(),
            source: ResolvedPartSource::File(ResolvedFile {
                raw: "x;y.png".into(),
                path: "/ws/x;y.png".into(),
            }),
            content_type: Some("image/png".into()),
        },
        ResolvedPart {
            name: "t".into(),
            source: ResolvedPartSource::Text(Resolved::literal("say \"hi\"")),
            content_type: Some("text/plain".into()),
        },
    ]);
    let exported = export_curl(&request, false);
    assert_eq!(
        exported.command,
        "curl 'https://h/?key=a+b' -F 'f=@\"x;y.png\";type=image/png' \
         -F 't=\"say \\\"hi\\\"\";type=text/plain' -L"
    );
    let back = import(&exported.command);
    let Some(Body::Multipart(parts)) = back.request.body else {
        panic!();
    };
    assert_eq!(
        parts[0].source,
        PartSource::File(Located::new("x;y.png".into()))
    );
    assert_eq!(parts[1].source, PartSource::Text("say \"hi\"".into()));

    request.auth = Some(Auth::OAuth2(rustman_core::model::OAuth2 {
        grant: rustman_core::model::OAuth2Grant::ClientCredentials,
        token_url: "https://h/token".into(),
        auth_url: None,
        client_id: "c".into(),
        client_secret: None,
        scopes: None,
    }));
    request.body = ResolvedBody::None;
    let exported = export_curl(&request, false);
    assert_eq!(exported.command, "curl -X POST 'https://h/' -L");
    assert_eq!(
        exported.warnings,
        ["the OAuth2 access token is not included; send the request with rustman to acquire it"]
    );

    request.auth = Some(Auth::Digest {
        username: "u".into(),
        password: "p".into(),
    });
    let exported = export_curl(&request, false);
    assert_eq!(exported.command, "curl -X POST 'https://h/' -L");
    assert_eq!(
        exported.warnings,
        ["digest auth is not included; send the request with rustman to sign it"]
    );
}

#[test]
fn an_explicit_authorization_header_wins_over_auth() {
    let mut request = bearer_request("tok");
    request.headers = vec![
        pair("Authorization", "Custom x"),
        pair("Content-Length", "9"),
    ];
    let exported = export_curl(&request, true);
    assert_eq!(
        exported.command,
        "curl 'https://api.example.com/me' -H 'Authorization: Custom x' -L"
    );
}

// Round trip: export, then import, gives the same method, URL, headers and
// body for any request without secrets.

/// Header names that the engine computes, or whose values are redacted.
const SPECIAL_HEADERS: &[&str] = &[
    "Authorization",
    "Proxy-Authorization",
    "Cookie",
    "Set-Cookie",
    "Content-Type",
    "Content-Length",
    "Host",
];

fn header_strategy() -> impl Strategy<Value = (String, String)> {
    ("[A-Za-z][A-Za-z0-9-]{0,15}", "[!-~]([ -~]{0,20}[!-~])?")
        .prop_filter("special header", |(name, _)| {
            !SPECIAL_HEADERS.iter().any(|h| h.eq_ignore_ascii_case(name))
        })
}

fn json_strategy() -> impl Strategy<Value = serde_json::Value> {
    let leaf = prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::from),
        any::<i64>().prop_map(serde_json::Value::from),
        ".{0,12}".prop_map(serde_json::Value::from),
    ];
    let tree = leaf.prop_recursive(3, 16, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(serde_json::Value::from),
            prop::collection::vec(("[a-z ]{1,6}", inner), 0..4)
                .prop_map(|kv| serde_json::Value::Object(kv.into_iter().collect())),
        ]
    });
    tree.prop_filter("object or array", |v| v.is_object() || v.is_array())
}

#[derive(Clone, Debug)]
enum TestBody {
    None,
    Json(serde_json::Value),
    Text(String),
    Form(Vec<(String, String)>),
    Multipart(Vec<(String, String, Option<String>)>),
}

fn body_strategy() -> impl Strategy<Value = TestBody> {
    prop_oneof![
        Just(TestBody::None),
        json_strategy().prop_map(TestBody::Json),
        ".{1,40}".prop_map(TestBody::Text),
        prop::collection::vec((".{1,8}", ".{0,8}"), 1..4).prop_map(TestBody::Form),
        prop::collection::vec(
            (
                "[a-z][a-z0-9_]{0,7}",
                ".{0,12}",
                prop::option::of("(text/plain|application/json)")
            ),
            1..4
        )
        .prop_map(TestBody::Multipart),
    ]
}

impl TestBody {
    fn resolved(&self) -> ResolvedBody {
        match self {
            Self::None => ResolvedBody::None,
            Self::Json(value) => ResolvedBody::Json(value.clone()),
            Self::Text(text) => ResolvedBody::Text(Resolved::literal(text.clone())),
            Self::Form(fields) => {
                ResolvedBody::Form(fields.iter().map(|(n, v)| pair(n, v)).collect())
            }
            Self::Multipart(parts) => ResolvedBody::Multipart(
                parts
                    .iter()
                    .map(|(name, value, ct)| ResolvedPart {
                        name: name.clone(),
                        source: ResolvedPartSource::Text(Resolved::literal(value.clone())),
                        content_type: ct.clone(),
                    })
                    .collect(),
            ),
        }
    }

    fn model(&self) -> Option<Body> {
        match self {
            Self::None => None,
            Self::Json(value) => Some(Body::Json(JsonContent::Value(value.clone()))),
            Self::Text(text) => Some(Body::Text(text.clone())),
            Self::Form(fields) => Some(Body::Form(
                fields.iter().map(|(n, v)| NameValue::new(n, v)).collect(),
            )),
            Self::Multipart(parts) => Some(Body::Multipart(
                parts
                    .iter()
                    .map(|(name, value, ct)| Part {
                        name: name.clone(),
                        source: PartSource::Text(value.clone()),
                        content_type: ct.clone(),
                    })
                    .collect(),
            )),
        }
    }
}

proptest! {
    #[test]
    fn export_then_import_round_trips(
        method in "(GET|POST|PUT|PATCH|DELETE|OPTIONS)",
        host in "[a-z]{1,10}(\\.[a-z]{2,5})?",
        path in "(/[a-zA-Z0-9._~-]{0,8}){0,3}",
        query in prop::collection::vec((".{1,6}", ".{0,6}"), 0..3),
        headers in prop::collection::vec(header_strategy(), 0..4),
        body in body_strategy(),
    ) {
        let mut request = get(&format!("https://{host}{path}"));
        request.method = method.clone();
        request.query = query.iter().map(|(n, v)| pair(n, v)).collect();
        request.headers = headers.iter().map(|(n, v)| pair(n, v)).collect();
        request.body = body.resolved();

        let exported = export_curl(&request, false);
        if exported.command.starts_with("# ") {
            // A NUL byte cannot be a shell argument: the export refuses it.
            prop_assert!(exported.warnings.iter().any(|w| w.contains("NUL byte")));
            return Ok(());
        }
        let imported = import_curl(&exported.command).unwrap();
        prop_assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
        let r = imported.request;
        prop_assert_eq!(r.effective_method(), method.as_str());
        let mut sent = get(&request.url.text);
        sent.query = request.query.clone();
        let sent_url = export_curl(&sent, true).command;
        let expected_url = shell_words::split(&sent_url).unwrap()[1].clone();
        prop_assert_eq!(&r.url, &expected_url);
        let expected_headers: Vec<NameValue> =
            headers.iter().map(|(n, v)| NameValue::new(n, v)).collect();
        prop_assert_eq!(r.headers.unwrap_or_default(), expected_headers);
        prop_assert_eq!(r.body, body.model());
    }
}
