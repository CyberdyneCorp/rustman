//! Choosing a request body example: the first JSON media type (otherwise the
//! first one), then its `example`, its first `examples` entry, the schema's
//! own `example`/`examples`/`default`, or a synthesized value.

use serde_json::Value;

use super::Synthesizer;
use crate::openapi::model::MediaType;

/// The example chosen for a request body.
#[derive(Clone, Debug, PartialEq)]
pub struct BodyExample {
    pub media_type: String,
    pub value: Value,
    /// Whether the value was synthesized rather than documented.
    pub synthesized: bool,
}

/// Whether `media_type` is JSON: `application/json`, or any `+json` type.
pub fn is_json_media_type(media_type: &str) -> bool {
    let essence = media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    essence == "application/json" || essence.ends_with("+json") || essence == "*/*"
}

/// The example body for `content`, or `None` when it has no media type.
pub fn body_example(content: &[MediaType], components: &Value) -> Option<BodyExample> {
    let media = content
        .iter()
        .find(|m| is_json_media_type(&m.media_type))
        .or_else(|| content.first())?;
    let documented = media
        .example
        .clone()
        .or_else(|| media.examples.first().map(|(_, v)| v.clone()));
    let synth = Synthesizer::new(components);
    let (value, synthesized) = match (documented, &media.schema) {
        (Some(value), _) => (value, false),
        (None, Some(schema)) => schema_value(&synth, schema),
        (None, None) => (Value::Object(serde_json::Map::new()), true),
    };
    Some(BodyExample {
        media_type: media.media_type.clone(),
        value,
        synthesized,
    })
}

/// The schema's own example or default, else a synthesized value.
fn schema_value(synth: &Synthesizer<'_>, schema: &Value) -> (Value, bool) {
    let resolved = synth.resolve(schema);
    let documented = resolved
        .get("examples")
        .and_then(Value::as_array)
        .and_then(|e| e.first())
        .or_else(|| resolved.get("example"))
        .or_else(|| resolved.get("default"));
    match documented {
        Some(value) => (value.clone(), false),
        None => (synth.value(schema), true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn media(media_type: &str, schema: Option<Value>) -> MediaType {
        MediaType {
            media_type: media_type.into(),
            schema,
            example: None,
            examples: Vec::new(),
        }
    }

    fn pick(content: &[MediaType]) -> BodyExample {
        body_example(content, &json!({})).unwrap()
    }

    #[test]
    fn json_is_preferred_over_earlier_media_types() {
        let content = [
            media("application/xml", None),
            media("application/vnd.api+json", Some(json!({"type": "string"}))),
        ];
        let chosen = pick(&content);
        assert_eq!(chosen.media_type, "application/vnd.api+json");
        assert_eq!(chosen.value, json!("string"));
        assert!(chosen.synthesized);
    }

    #[test]
    fn documented_examples_come_first_in_order() {
        let schema = json!({"type": "integer", "example": 3, "default": 4});
        let mut m = media("application/json", Some(schema));
        m.examples = vec![("one".into(), json!(2))];
        assert_eq!(pick(&[m.clone()]).value, json!(2));
        m.example = Some(json!(1));
        assert_eq!(pick(&[m.clone()]).value, json!(1));
        m.example = None;
        m.examples.clear();
        assert_eq!(pick(&[m.clone()]).value, json!(3));
        m.schema = Some(json!({"type": "integer", "default": 4}));
        let chosen = pick(&[m]);
        assert_eq!((chosen.value, chosen.synthesized), (json!(4), false));
    }

    #[test]
    fn no_content_means_no_example() {
        assert_eq!(body_example(&[], &json!({})), None);
        assert!(is_json_media_type("application/json; charset=utf-8"));
        assert!(!is_json_media_type("text/plain"));
    }
}
