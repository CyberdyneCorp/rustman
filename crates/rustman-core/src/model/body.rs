//! Request bodies: `body: {type: json|text|form|multipart|file|none, ...}`.
//!
//! The mapping is read into a flat struct and then checked per `type`, so no
//! internally tagged enum is involved and error positions stay exact (D4).

use serde::Deserialize;

use super::common::{NameValue, check_fields, exactly_one, seen};
use crate::format::{JsonValue, Located, error_at};

pub const BODY_TYPES: &[&str] = &["json", "text", "form", "multipart", "file", "none"];

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "RawBody")]
pub enum Body {
    None,
    Json(JsonContent),
    Text(String),
    /// `application/x-www-form-urlencoded` fields, in order.
    Form(Vec<NameValue>),
    Multipart(Vec<Part>),
    /// Raw bytes sent from a workspace file.
    File(Located<String>),
}

/// JSON body content: a YAML value converted to JSON, or JSON text sent as is.
#[derive(Clone, Debug, PartialEq)]
pub enum JsonContent {
    Value(serde_json::Value),
    Text(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBody {
    #[serde(rename = "type")]
    kind: Located<String>,
    content: Option<Located<JsonValue>>,
    fields: Option<Located<Vec<NameValue>>>,
    parts: Option<Located<Vec<Part>>>,
    path: Option<Located<String>>,
}

impl TryFrom<RawBody> for Body {
    type Error = String;

    fn try_from(raw: RawBody) -> Result<Self, String> {
        let present = [
            ("content", seen(&raw.content)),
            ("fields", seen(&raw.fields)),
            ("parts", seen(&raw.parts)),
            ("path", seen(&raw.path)),
        ];
        let what = format!("body type '{}'", *raw.kind);
        let check = |required: &[&str]| check_fields(&what, &present, required, &[]);
        let content = raw.content.map(|c| c.into_inner().0);
        match raw.kind.as_str() {
            "none" => check(&[]).map(|()| Self::None),
            "json" => check(&["content"]).map(|()| Self::Json(json_content(content))),
            "text" => check(&["content"]).and_then(|()| text_content(content)),
            "form" => check(&["fields"]).map(|()| Self::Form(inner(raw.fields))),
            "multipart" => check(&["parts"]).map(|()| Self::Multipart(inner(raw.parts))),
            "file" => check(&["path"])
                .map(|()| Self::File(raw.path.unwrap_or_else(|| Located::new(String::new())))),
            other => Err(error_at(
                raw.kind.position(),
                format!(
                    "unknown body type '{other}' (expected one of: {})",
                    BODY_TYPES.join(", ")
                ),
            )),
        }
    }
}

fn inner<T: Default>(value: Option<Located<T>>) -> T {
    value.map(Located::into_inner).unwrap_or_default()
}

fn json_content(content: Option<serde_json::Value>) -> JsonContent {
    match content {
        Some(serde_json::Value::String(text)) => JsonContent::Text(text),
        Some(value) => JsonContent::Value(value),
        None => JsonContent::Value(serde_json::Value::Null),
    }
}

fn text_content(content: Option<serde_json::Value>) -> Result<Body, String> {
    match content {
        Some(serde_json::Value::String(text)) => Ok(Body::Text(text)),
        _ => Err("body type 'text' needs a string 'content'".to_owned()),
    }
}

/// One multipart part: inline text (`value`) or a workspace file (`file`).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "RawPart")]
pub struct Part {
    pub name: String,
    pub source: PartSource,
    pub content_type: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PartSource {
    Text(String),
    File(Located<String>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPart {
    name: String,
    value: Option<Located<String>>,
    file: Option<Located<String>>,
    content_type: Option<String>,
}

impl TryFrom<RawPart> for Part {
    type Error = String;

    fn try_from(raw: RawPart) -> Result<Self, String> {
        let what = format!("multipart part '{}'", raw.name);
        exactly_one(
            &what,
            &[("value", seen(&raw.value)), ("file", seen(&raw.file))],
        )?;
        let source = match (raw.value, raw.file) {
            (Some(text), _) => PartSource::Text(text.into_inner()),
            (None, file) => PartSource::File(file.unwrap_or_else(|| Located::new(String::new()))),
        };
        Ok(Self {
            name: raw.name,
            source,
            content_type: raw.content_type,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn body(text: &str) -> Result<Body, crate::error::LoadError> {
        crate::format::from_yaml(Path::new("b.yaml"), text)
    }

    #[test]
    fn every_body_type_loads() {
        assert_eq!(body("type: none").unwrap(), Body::None);
        assert_eq!(
            body("type: json\ncontent: {a: no}").unwrap(),
            Body::Json(JsonContent::Value(serde_json::json!({"a": "no"})))
        );
        assert_eq!(
            body("type: json\ncontent: '{\"a\":1}'").unwrap(),
            Body::Json(JsonContent::Text("{\"a\":1}".into()))
        );
        assert_eq!(
            body("type: text\ncontent: hi").unwrap(),
            Body::Text("hi".into())
        );
        assert_eq!(
            body("type: form\nfields: [{name: a, value: b}]").unwrap(),
            Body::Form(vec![NameValue::new("a", "b")])
        );
        assert_eq!(
            body("type: file\npath: a.bin").unwrap(),
            Body::File(Located::new("a.bin".into()))
        );
        let Body::Multipart(parts) =
            body("type: multipart\nparts:\n  - {name: a, value: x}\n  - {name: f, file: ./a.png}")
                .unwrap()
        else {
            panic!("expected multipart");
        };
        assert_eq!(parts[0].source, PartSource::Text("x".into()));
        assert_eq!(
            parts[1].source,
            PartSource::File(Located::new("./a.png".into()))
        );
    }

    #[test]
    fn type_specific_fields_are_checked() {
        let err = body("type: json\nfields: []").unwrap_err();
        assert_eq!(err.message, "body type 'json' does not accept 'fields'");
        let err = body("type: text\ncontent: [1]").unwrap_err();
        assert_eq!(err.message, "body type 'text' needs a string 'content'");
        let err = body("type: xml").unwrap_err();
        assert_eq!(
            err.message,
            "unknown body type 'xml' (expected one of: json, text, form, multipart, file, none)"
        );
        let err = body("type: multipart\nparts:\n  - {name: a}").unwrap_err();
        assert_eq!(
            err.message,
            "multipart part 'a' needs exactly one of: value, file"
        );
    }

    #[test]
    fn field_errors_point_at_the_offending_key() {
        let at = |text: &str| {
            let err = body(text).unwrap_err();
            (err.line(), err.column())
        };
        assert_eq!(at("type: json\ncontent: 1\nfields: []"), (Some(3), Some(9)));
        assert_eq!(at("type: xml"), (Some(1), Some(7)));
        assert_eq!(
            at("type: multipart\nparts:\n  - name: a\n    value: x\n    file: f"),
            (Some(5), Some(11))
        );
    }

    #[test]
    fn json_numbers_follow_yaml_1_2_and_lossy_integers_are_refused() {
        assert_eq!(
            body("type: json\ncontent: {a: 0x1F, f: 0o17, e: +12, z: 0755, big: 18446744073709551615}")
                .unwrap(),
            Body::Json(JsonContent::Value(serde_json::json!(
                {"a": 31, "f": 15, "e": 12, "z": "0755", "big": 18446744073709551615_u64}
            )))
        );
        let err = body("type: json\ncontent:\n  l: [1, 12345678901234567890123]").unwrap_err();
        assert_eq!((err.line(), err.column()), (Some(3), Some(10)));
        assert_eq!(
            err.message,
            "body content has an integer too large to send exactly; quote it, or give the JSON body as a string"
        );
        assert!(body("type: json\ncontent: '{\"l\": 12345678901234567890123}'").is_ok());
        assert!(body("type: json\ncontent: {f: 1.5, n: -3}").is_ok());
        assert_eq!(
            body("type: json\ncontent: [1e300, 1.0e20]").unwrap(),
            Body::Json(JsonContent::Value(serde_json::json!([1e300, 1.0e20])))
        );
    }
}
