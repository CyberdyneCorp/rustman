//! Secrets inside free text: the userinfo of a URL, the `"key": "value"`
//! pairs of JSON-like bodies, the `key=value` pairs of form-encoded bodies,
//! and literal values that scripts give to secret-named variables or
//! headers.

use std::ops::Range;
use std::sync::LazyLock;

use regex::{Captures, Regex};

/// A quoted string literal: `'...'`, `"..."` or `` `...` ``, escapes kept.
const QUOTED: &str = r#"(?:'(?:[^'\\]|\\.)*'|"(?:[^"\\]|\\.)*"|`(?:[^`\\]|\\.)*`)"#;

/// `"key": "value"` (JSON) and `key: 'value'` (object literals); also
/// `"pin": 12345678`, numbers of four digits or more.
static KEY_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = format!(r#"["']?([A-Za-z_$][\w$.-]*)["']?\s*:\s*({QUOTED}|-?\d{{4,}}\b)"#);
    Regex::new(&pattern).expect("the key-value pattern is valid")
});

/// `"key": [` : the start of an array value.
static KEY_ARRAY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"["']?([A-Za-z_$][\w$.-]*)["']?\s*:\s*\["#)
        .expect("the key-array pattern is valid")
});

/// `btoa('user:pw')` and `atob('...')`: encoded credentials.
static ENCODED: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = format!(r"\b(btoa|atob)\(\s*({QUOTED})");
    Regex::new(&pattern).expect("the encoded-literal pattern is valid")
});

/// `key=value` pairs of an `application/x-www-form-urlencoded` body.
static FORM_PAIR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|&)([^=&\s]+)=([^&\r\n]*)").expect("the form-pair pattern is valid")
});

/// `x.set("name", "value")`, `bru.setEnvVar(...)`, `req.setHeader(...)`.
static SETTER: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = format!(r"\.set\w*\(\s*({QUOTED})\s*,\s*({QUOTED})");
    Regex::new(&pattern).expect("the setter pattern is valid")
});

/// `const name = "value"` (also `let`, `var`, or a plain assignment such
/// as `secret = '...'` or `headers.Authorization = '...'`).
static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = format!(r"([A-Za-z_$][\w$.]*)\s*=\s*({QUOTED})");
    Regex::new(&pattern).expect("the assignment pattern is valid")
});

/// `{key: "X-Token", value: "..."}`, as in `pm.request.headers.add(...)`.
static KEY_AND_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = format!(r#"\bkey["']?\s*:\s*({QUOTED})\s*,\s*["']?value["']?\s*:\s*({QUOTED})"#);
    Regex::new(&pattern).expect("the key-and-value pattern is valid")
});

static STRING_LITERAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(QUOTED).expect("the string-literal pattern is valid"));

/// A named value found in text, with the byte range of its literal text
/// (inside the quotes, when quoted).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub name: String,
    pub value: String,
    pub range: Range<usize>,
}

/// The byte range of the credentials in the userinfo of `url`: the
/// password of `user:password@`, or the whole userinfo when it has no
/// password (a token used as the user name).
pub fn userinfo_credentials(url: &str) -> Option<Range<usize>> {
    let start = url.find("://")? + 3;
    let rest = &url[start..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let at = rest[..end].rfind('@')?;
    let range = match rest[..at].find(':') {
        Some(colon) => start + colon + 1..start + at,
        None => start..start + at,
    };
    (!range.is_empty()).then_some(range)
}

/// The `"key": "value"` pairs of `text` whose key `wanted` accepts, and
/// each string of a `"key": ["a", "b"]` array, in text order.
pub fn key_values(text: &str, wanted: &dyn Fn(&str) -> bool) -> Vec<Found> {
    let mut found: Vec<Found> = KEY_VALUE
        .captures_iter(text)
        .filter(|c| wanted(&c[1]))
        .map(|c| quoted(&c, 1, 2))
        .collect();
    for c in KEY_ARRAY.captures_iter(text).filter(|c| wanted(&c[1])) {
        let end = c.get(0).expect("the match exists").end();
        found.extend(array_strings(text, end, &c[1]));
    }
    found.sort_by_key(|f| f.range.start);
    found
}

/// The string elements of the array starting at byte `from` of `text` (just
/// after its `[`), up to its `]` or the first element that is not a
/// string.
fn array_strings(text: &str, from: usize, name: &str) -> Vec<Found> {
    let mut out = Vec::new();
    let mut at = from;
    loop {
        let rest = &text[at..];
        let skipped = rest.len() - rest.trim_start_matches([' ', '\t', '\r', '\n', ',']).len();
        at += skipped;
        let Some(m) = STRING_LITERAL.find_at(text, at).filter(|m| m.start() == at) else {
            return out;
        };
        let literal = m.as_str();
        out.push(Found {
            name: name.to_owned(),
            value: decode(&literal[1..literal.len() - 1]),
            range: m.start() + 1..m.end() - 1,
        });
        at = m.end();
    }
}

/// The literals passed to `btoa(...)` and `atob(...)`, named after the call.
pub fn encoded_literals(source: &str) -> Vec<Found> {
    ENCODED
        .captures_iter(source)
        .map(|c| quoted(&c, 1, 2))
        .collect()
}

/// The `key=value` pairs of a form-encoded `text` whose key `wanted`
/// accepts; nothing when `text` does not look form-encoded.
pub fn form_values(text: &str, wanted: &dyn Fn(&str) -> bool) -> Vec<Found> {
    let form_like = !text.trim().is_empty()
        && !text.contains(['{', '<', '\n'])
        && text.split('&').all(|pair| pair.contains('='));
    if !form_like {
        return Vec::new();
    }
    FORM_PAIR
        .captures_iter(text)
        .filter(|c| wanted(&c[1]))
        .map(|c| {
            let value = c.get(2).expect("group 2 always matches");
            Found {
                name: c[1].to_owned(),
                value: value.as_str().to_owned(),
                range: value.range(),
            }
        })
        .collect()
}

/// The literal values a script gives to variables, headers or object keys
/// whose name `wanted` accepts.
pub fn script_values(source: &str, wanted: &dyn Fn(&str) -> bool) -> Vec<Found> {
    let mut found: Vec<Found> = SETTER
        .captures_iter(source)
        .filter(|c| wanted(unquote(&c[1])))
        .map(|c| {
            let mut f = quoted(&c, 1, 2);
            f.name = unquote(&f.name).to_owned();
            f
        })
        .collect();
    let pairs = KEY_AND_VALUE.captures_iter(source);
    found.extend(pairs.filter(|c| wanted(unquote(&c[1]))).map(|c| {
        let mut f = quoted(&c, 1, 2);
        f.name = unquote(&f.name).to_owned();
        f
    }));
    let assigned = ASSIGNMENT.captures_iter(source);
    found.extend(assigned.filter(|c| wanted(&c[1])).map(|c| quoted(&c, 1, 2)));
    // `key` and `value` name the parts of a pair, matched above.
    let named = |name: &str| name != "key" && name != "value" && wanted(name);
    found.extend(key_values(source, &named));
    found
}

/// The contents of every string literal of `source`.
pub fn string_literals(source: &str) -> impl Iterator<Item = &str> {
    STRING_LITERAL
        .find_iter(source)
        .map(|m| unquote(m.as_str()))
}

/// A [`Found`] from a name group and a value group: a quoted literal
/// (range inside the quotes) or a bare number.
fn quoted(c: &Captures<'_>, name: usize, value: usize) -> Found {
    let literal = c.get(value).expect("the value group always matches");
    if !literal.as_str().starts_with(['"', '\'', '`']) {
        return Found {
            name: c[name].to_owned(),
            value: literal.as_str().to_owned(),
            range: literal.range(),
        };
    }
    let range = literal.start() + 1..literal.end() - 1;
    Found {
        name: c[name].to_owned(),
        value: decode(&literal.as_str()[1..literal.len() - 1]),
        range,
    }
}

/// `"x"`, `'x'` or `` `x` `` without its quotes.
fn unquote(text: &str) -> &str {
    let quoted = text.len() >= 2 && text.starts_with(['"', '\'', '`']);
    if quoted {
        &text[1..text.len() - 1]
    } else {
        text
    }
}

/// The value of an escaped string literal's contents; as written when it
/// is not valid JSON string text.
fn decode(raw: &str) -> String {
    if !raw.contains('\\') {
        return raw.to_owned();
    }
    serde_json::from_str(&format!("\"{raw}\"")).unwrap_or_else(|_| raw.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secretish(name: &str) -> bool {
        crate::secrets::is_secret_name(name) || name.eq_ignore_ascii_case("authorization")
    }

    #[test]
    fn userinfo_password_or_token() {
        let url = "https://admin:p4ss@api.example.com/x?a=b@c";
        assert_eq!(&url[userinfo_credentials(url).unwrap()], "p4ss");
        let url = "https://ghp_abc@github.com";
        assert_eq!(&url[userinfo_credentials(url).unwrap()], "ghp_abc");
        assert_eq!(userinfo_credentials("https://x.com/a@b"), None);
        assert_eq!(userinfo_credentials("{{base}}/a"), None);
        assert_eq!(userinfo_credentials("https://user:@x.com"), None);
    }

    #[test]
    fn json_and_object_keys() {
        let text = r#"{"username": "u", "password": "p\"w", "nested": {"api_key":"k"}}"#;
        let found = key_values(text, &secretish);
        let names: Vec<_> = found.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["password", "api_key"]);
        assert_eq!(found[0].value, "p\"w");
        assert_eq!(&text[found[0].range.clone()], r#"p\"w"#);
    }

    #[test]
    fn arrays_and_long_numbers_under_secret_keys() {
        let text = r#"{"tokens": ["a1", "b2"], "ids": ["x"], "pin": 12345678, "pin2": 12}"#;
        let found = key_values(text, &secretish);
        let values: Vec<_> = found.iter().map(|f| f.value.as_str()).collect();
        assert_eq!(values, ["a1", "b2", "12345678"]);
        assert_eq!(&text[found[2].range.clone()], "12345678");
    }

    #[test]
    fn encoded_literals_and_plain_assignments() {
        let source = "let creds = btoa('user:pw1');\nsecret = 'pw2';\nif (a == 'b') {}";
        let encoded = encoded_literals(source);
        assert_eq!(encoded[0].value, "user:pw1");
        let found = script_values(source, &secretish);
        assert!(
            found.iter().any(|f| f.name == "secret" && f.value == "pw2"),
            "{found:?}"
        );
        assert!(!found.iter().any(|f| f.value == "b"), "{found:?}");
    }

    #[test]
    fn form_encoded_pairs() {
        let found = form_values("user=me&password=s3cret&x=1", &secretish);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].value, "s3cret");
        assert!(form_values("{\"password\":\"x\"}", &secretish).is_empty());
        assert!(form_values("plain text", &secretish).is_empty());
    }

    #[test]
    fn script_setters_assignments_and_keys() {
        let source = "pm.environment.set('apiSecret','LEAK13');\n\
                      req.setHeader(\"Authorization\", \"Bearer abc\");\n\
                      const token = `t-1`;\n\
                      let user = 'me';\n\
                      pm.sendRequest({ body: { password: 'pw' } });\n\
                      pm.request.headers.add({key: \"X-Token\", value: \"xt\"});\n\
                      pm.request.headers.add({key: \"Accept\", value: \"a/b\"});";
        let found = script_values(source, &secretish);
        let pairs: Vec<_> = found
            .iter()
            .map(|f| (f.name.as_str(), f.value.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("apiSecret", "LEAK13"),
                ("Authorization", "Bearer abc"),
                ("X-Token", "xt"),
                ("token", "t-1"),
                ("password", "pw"),
            ]
        );
        let literals: Vec<_> = string_literals("a('x', \"y\")").collect();
        assert_eq!(literals, ["x", "y"]);
    }
}
