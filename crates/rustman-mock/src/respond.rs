//! Choosing the response of a matched operation (spec: Response selection
//! order, Prefer header, Content negotiation, Deterministic schema
//! synthesis): the status from `Prefer: code=` or the lowest documented
//! `2xx` (else `default`), the media type from `Accept`, and the body from
//! the first named example, the media type's `example`, the schema's
//! example, or seeded synthesis.

use http::StatusCode;
use rustman_explore::openapi::model::{MediaType, Operation, Response};
use rustman_explore::synth::{Synthesizer, is_json_media_type};
use serde_json::Value;

use crate::negotiate;
use crate::prefer::Prefer;
use crate::problem::Problem;

/// Where a response came from, as the request log names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A hand-written override.
    Override,
    /// Chosen through the `Prefer` header.
    Prefer,
    /// A documented example.
    Example,
    /// Synthesized from the schema.
    Synthesized,
    /// The stateful store.
    Stateful,
    /// Forwarded to the `--proxy-unmatched` upstream.
    Proxy,
    /// A CORS preflight answered by the mock.
    Cors,
    /// An error the mock generated itself.
    Error,
}

impl serde::Serialize for Source {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Override => "override",
            Self::Prefer => "prefer",
            Self::Example => "example",
            Self::Synthesized => "synthesized",
            Self::Stateful => "stateful",
            Self::Proxy => "proxy",
            Self::Cors => "cors",
            Self::Error => "error",
        }
    }
}

/// What the response depends on besides the operation.
#[derive(Clone, Copy, Debug)]
pub struct Ask<'a> {
    /// The request path, part of the synthesis seed.
    pub path: &'a str,
    pub accept: Option<&'a str>,
    /// `None` when `Prefer` is ignored (`--no-prefer`) or absent.
    pub prefer: Option<&'a Prefer>,
    pub seed: u64,
}

/// A chosen response.
#[derive(Clone, Debug, PartialEq)]
pub struct Chosen {
    pub status: StatusCode,
    /// `None` for a response without content.
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    pub source: Source,
}

/// The response for `operation`, or the problem to answer instead.
pub fn respond(
    operation: &Operation,
    components: &Value,
    ask: &Ask<'_>,
) -> Result<Chosen, Problem> {
    let prefer = ask.prefer.filter(|p| !p.is_empty());
    let (response, status) = select_status(operation, prefer.and_then(|p| p.code.as_deref()))?;
    let Some(response) = response.filter(|r| !r.content.is_empty()) else {
        return Ok(Chosen {
            status,
            content_type: None,
            body: Vec::new(),
            source: prefer.map_or(Source::Example, |_| Source::Prefer),
        });
    };
    let index = negotiate::choose(ask.accept, &response.content)
        .ok_or_else(|| not_acceptable(ask.accept, &response.content))?;
    let media = &response.content[index];
    let (value, synthesized) = body_value(operation, media, components, ask, prefer)?;
    let source = match (prefer, synthesized) {
        (Some(_), _) => Source::Prefer,
        (None, true) => Source::Synthesized,
        (None, false) => Source::Example,
    };
    let content_type = content_type(&media.media_type);
    Ok(Chosen {
        status,
        body: serialize(&content_type, &value),
        content_type: Some(content_type),
        source,
    })
}

/// The documented response for `code`, or the default choice; with the
/// status to send.
fn select_status<'o>(
    operation: &'o Operation,
    code: Option<&str>,
) -> Result<(Option<&'o Response>, StatusCode), Problem> {
    match code {
        Some(code) => preferred(operation, code),
        None => Ok(default_response(operation)),
    }
}

fn preferred<'o>(
    operation: &'o Operation,
    code: &str,
) -> Result<(Option<&'o Response>, StatusCode), Problem> {
    let status = code
        .parse::<u16>()
        .ok()
        .and_then(|c| StatusCode::from_u16(c).ok())
        .ok_or_else(|| undocumented_code(operation, code))?;
    let range = format!("{}XX", status.as_u16() / 100);
    let responses = &operation.responses;
    let found = responses
        .iter()
        .find(|r| r.status == code)
        .or_else(|| {
            responses
                .iter()
                .find(|r| r.status.eq_ignore_ascii_case(&range))
        })
        .or_else(|| responses.iter().find(|r| r.status == "default"));
    match found {
        Some(response) => Ok((Some(response), status)),
        None => Err(undocumented_code(operation, code)),
    }
}

/// The lowest documented `2xx` (`2XX` counting as 200), else `default`
/// (as 200), else the lowest documented status.
pub(crate) fn default_response(operation: &Operation) -> (Option<&Response>, StatusCode) {
    let mut statuses: Vec<(StatusCode, &Response)> = operation
        .responses
        .iter()
        .filter_map(|r| status_of(&r.status).map(|s| (s, r)))
        .collect();
    statuses.sort_by_key(|(status, _)| *status);
    let success = statuses.iter().find(|(s, _)| s.is_success());
    let default = operation.responses.iter().find(|r| r.status == "default");
    match (success, default) {
        (Some((status, response)), _) => (Some(*response), *status),
        (None, Some(response)) => (Some(response), StatusCode::OK),
        (None, None) => statuses
            .first()
            .map_or((None, StatusCode::OK), |(s, r)| (Some(*r), *s)),
    }
}

/// `404` as 404, `4XX` as 400; `None` for `default`.
fn status_of(status: &str) -> Option<StatusCode> {
    let code = match status.to_ascii_uppercase().strip_suffix("XX") {
        Some(class) => class.parse::<u16>().ok()? * 100,
        None => status.parse().ok()?,
    };
    StatusCode::from_u16(code).ok()
}

/// The body value, and whether it was synthesized.
fn body_value(
    operation: &Operation,
    media: &MediaType,
    components: &Value,
    ask: &Ask<'_>,
    prefer: Option<&Prefer>,
) -> Result<(Value, bool), Problem> {
    let synthesize = || (synthesized(operation, media, components, ask), true);
    if prefer.is_some_and(|p| p.dynamic) {
        return Ok(synthesize());
    }
    if let Some(name) = prefer.and_then(|p| p.example.as_deref()) {
        return named_example(operation, media, name).map(|v| (v, false));
    }
    let synth = Synthesizer::new(components);
    let schema_example = media.schema.as_ref().and_then(|schema| {
        let resolved = synth.resolve(schema);
        let examples = resolved.get("examples").and_then(Value::as_array);
        examples
            .and_then(|e| e.first())
            .or_else(|| resolved.get("example"))
            .cloned()
    });
    let documented = media
        .examples
        .first()
        .map(|(_, v)| v.clone())
        .or_else(|| media.example.clone())
        .or(schema_example);
    Ok(documented.map_or_else(synthesize, |v| (v, false)))
}

fn named_example(operation: &Operation, media: &MediaType, name: &str) -> Result<Value, Problem> {
    match media.examples.iter().find(|(n, _)| n == name) {
        Some((_, value)) => Ok(value.clone()),
        None => {
            let names: Vec<&str> = media.examples.iter().map(|(n, _)| n.as_str()).collect();
            let detail = format!(
                "{} {} has no example '{name}' for {}; documented: {}",
                operation.method,
                operation.path,
                media.media_type,
                list_or_none(&names)
            );
            Err(Problem::new(
                StatusCode::BAD_REQUEST,
                "unknown-example",
                "Unknown example",
                detail,
            ))
        }
    }
}

/// A value for the media type's schema, seeded by the server seed, the
/// operation and the request path (design D4).
fn synthesized(
    operation: &Operation,
    media: &MediaType,
    components: &Value,
    ask: &Ask<'_>,
) -> Value {
    let Some(schema) = &media.schema else {
        return Value::Object(serde_json::Map::new());
    };
    let key = operation
        .operation_id
        .as_deref()
        .unwrap_or(&operation.pointer);
    let seed = mix(ask.seed, &[key, ask.path]);
    Synthesizer::new(components).seeded(seed).value(schema)
}

/// FNV-1a over the seed and each part (with a separator).
pub fn mix(seed: u64, parts: &[&str]) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01B3;
    let mut hash: u64 = 0xCBF2_9CE4_8422_2325;
    let bytes = seed.to_le_bytes();
    let all = bytes.iter().chain(
        parts
            .iter()
            .flat_map(|p| p.as_bytes().iter().chain(&[0xFF])),
    );
    for byte in all {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// The documented media type, or JSON for a range such as `*/*`.
fn content_type(media_type: &str) -> String {
    if media_type.contains('*') {
        "application/json".to_owned()
    } else {
        media_type.to_owned()
    }
}

fn serialize(content_type: &str, value: &Value) -> Vec<u8> {
    match value {
        Value::String(text) if !is_json_media_type(content_type) => text.clone().into_bytes(),
        _ => serde_json::to_vec(value).unwrap_or_default(),
    }
}

fn undocumented_code(operation: &Operation, code: &str) -> Problem {
    let codes: Vec<&str> = operation
        .responses
        .iter()
        .map(|r| r.status.as_str())
        .collect();
    let detail = format!(
        "{} {} does not document {code}; documented: {}",
        operation.method,
        operation.path,
        list_or_none(&codes)
    );
    Problem::new(
        StatusCode::BAD_REQUEST,
        "undocumented-code",
        "Undocumented response code",
        detail,
    )
}

fn not_acceptable(accept: Option<&str>, content: &[MediaType]) -> Problem {
    let offered: Vec<&str> = content.iter().map(|m| m.media_type.as_str()).collect();
    let detail = format!(
        "Accept: {} matches none of the documented media types: {}",
        accept.unwrap_or_default(),
        offered.join(", ")
    );
    Problem::new(
        StatusCode::NOT_ACCEPTABLE,
        "not-acceptable",
        "Not acceptable",
        detail,
    )
}

fn list_or_none(items: &[&str]) -> String {
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn media(media_type: &str, schema: Option<Value>, examples: &[(&str, Value)]) -> MediaType {
        MediaType {
            media_type: media_type.into(),
            schema,
            example: None,
            examples: examples
                .iter()
                .map(|(n, v)| ((*n).into(), v.clone()))
                .collect(),
        }
    }

    fn response(status: &str, content: Vec<MediaType>) -> Response {
        Response {
            status: status.into(),
            description: None,
            content,
            headers: Vec::new(),
            links: Vec::new(),
        }
    }

    fn operation(responses: Vec<Response>) -> Operation {
        Operation {
            pointer: "/paths/~1pets~1{id}/get".into(),
            operation_id: Some("getPet".into()),
            method: "GET".into(),
            path: "/pets/{id}".into(),
            summary: None,
            description: None,
            tags: Vec::new(),
            parameters: Vec::new(),
            request_body: None,
            responses,
            security: None,
            deprecated: false,
        }
    }

    fn pets() -> Operation {
        let pet = json!({"type": "object", "required": ["name"], "properties": {"name": {"type": "string"}}});
        operation(vec![
            response(
                "404",
                vec![media(
                    "application/json",
                    None,
                    &[("notFound", json!({"error": "no pet"}))],
                )],
            ),
            response(
                "200",
                vec![media(
                    "application/json",
                    Some(pet),
                    &[
                        ("cat", json!({"name": "Tom"})),
                        ("dog", json!({"name": "Rex"})),
                    ],
                )],
            ),
        ])
    }

    fn ask<'a>(prefer: Option<&'a Prefer>, accept: Option<&'a str>) -> Ask<'a> {
        Ask {
            path: "/pets/1",
            accept,
            prefer,
            seed: 0,
        }
    }

    fn body(chosen: &Chosen) -> Value {
        serde_json::from_slice(&chosen.body).unwrap()
    }

    #[test]
    fn the_lowest_success_with_its_first_named_example() {
        let chosen = respond(&pets(), &json!({}), &ask(None, None)).unwrap();
        assert_eq!(
            (chosen.status, chosen.source),
            (StatusCode::OK, Source::Example)
        );
        assert_eq!(body(&chosen), json!({"name": "Tom"}));
        assert_eq!(chosen.content_type.as_deref(), Some("application/json"));
    }

    #[test]
    fn prefer_picks_the_code_and_the_example() {
        let prefer = Prefer::parse(["code=404, example=notFound"]);
        let chosen = respond(&pets(), &json!({}), &ask(Some(&prefer), None)).unwrap();
        assert_eq!(
            (chosen.status, chosen.source),
            (StatusCode::NOT_FOUND, Source::Prefer)
        );
        assert_eq!(body(&chosen), json!({"error": "no pet"}));
        let dog = Prefer::parse(["example=dog"]);
        let chosen = respond(&pets(), &json!({}), &ask(Some(&dog), None)).unwrap();
        assert_eq!(body(&chosen), json!({"name": "Rex"}));
    }

    #[test]
    fn an_undocumented_code_or_example_is_a_400() {
        let teapot = Prefer::parse(["code=418"]);
        let problem = respond(&pets(), &json!({}), &ask(Some(&teapot), None)).unwrap_err();
        assert_eq!(problem.status, 400);
        assert!(
            problem.detail.ends_with("documented: 404, 200"),
            "{}",
            problem.detail
        );
        let unknown = Prefer::parse(["example=fish"]);
        let problem = respond(&pets(), &json!({}), &ask(Some(&unknown), None)).unwrap_err();
        assert!(
            problem.detail.ends_with("documented: cat, dog"),
            "{}",
            problem.detail
        );
    }

    #[test]
    fn dynamic_synthesizes_and_accept_negotiates() {
        let dynamic = Prefer::parse(["dynamic=true"]);
        let chosen = respond(&pets(), &json!({}), &ask(Some(&dynamic), None)).unwrap();
        assert!(
            body(&chosen)["name"]
                .as_str()
                .unwrap()
                .starts_with("string")
        );
        let problem =
            respond(&pets(), &json!({}), &ask(None, Some("application/xml"))).unwrap_err();
        assert_eq!(problem.status, 406);
    }

    #[test]
    fn ranges_default_and_empty_responses() {
        let op = operation(vec![
            response("2XX", Vec::new()),
            response("default", Vec::new()),
        ]);
        let chosen = respond(&op, &json!({}), &ask(None, None)).unwrap();
        assert_eq!((chosen.status, chosen.content_type), (StatusCode::OK, None));
        let server_error = Prefer::parse(["code=503"]);
        let chosen = respond(&op, &json!({}), &ask(Some(&server_error), None)).unwrap();
        assert_eq!(chosen.status, StatusCode::SERVICE_UNAVAILABLE);
        let only_errors = operation(vec![
            response("500", Vec::new()),
            response("404", Vec::new()),
        ]);
        let chosen = respond(&only_errors, &json!({}), &ask(None, None)).unwrap();
        assert_eq!(chosen.status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn schema_examples_come_before_synthesis_and_text_is_sent_raw() {
        let schema = json!({"type": "string", "example": "hello"});
        let op = operation(vec![response(
            "200",
            vec![media("text/plain", Some(schema), &[])],
        )]);
        let chosen = respond(&op, &json!({}), &ask(None, None)).unwrap();
        assert_eq!(
            (chosen.body.as_slice(), chosen.source),
            (&b"hello"[..], Source::Example)
        );
    }

    #[test]
    fn synthesis_depends_on_seed_and_path() {
        let schema =
            json!({"type": "object", "required": ["n"], "properties": {"n": {"type": "integer"}}});
        let op = operation(vec![response(
            "200",
            vec![media("application/json", Some(schema), &[])],
        )]);
        let run = |seed, path| {
            let ask = Ask {
                path,
                accept: None,
                prefer: None,
                seed,
            };
            respond(&op, &json!({}), &ask).unwrap().body
        };
        assert_eq!(run(0, "/pets/1"), run(0, "/pets/1"));
        let all: std::collections::BTreeSet<_> = (0..8).map(|seed| run(seed, "/pets/1")).collect();
        assert!(all.len() > 1);
        assert_ne!(mix(0, &["a", "b"]), mix(0, &["ab", ""]));
    }
}
