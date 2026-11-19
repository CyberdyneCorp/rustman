//! Request validation (spec: Request validation; design D3): path, query,
//! header and cookie parameters (`400`), the body's media type (`415`) and
//! the body against its schema (`422`), each failure as problem+json with
//! up to [`MAX_ERRORS`] violations.
//!
//! Schemas are the explorer's lowered JSON Schema 2020-12 (OpenAPI 3.0
//! `nullable` and boolean `exclusiveMinimum` already converted), validated
//! with `jsonschema` through `rustman_explore::openapi::schema`. Validators
//! are compiled on first use and kept with the loaded state.
//!
//! The JSON Schema engine reports one violation per validation (collecting
//! every violation can grow exponentially with nested `$ref`s), so the body
//! is checked per top-level property: missing required properties, then the
//! first violation of each property, then the first of the whole body.

use std::sync::OnceLock;

use http::StatusCode;
use jsonschema::Validator;
use rustman_explore::openapi::model::{
    ApiModel, MediaType, Operation, Parameter, ParameterLocation, RequestBody,
};
use rustman_explore::openapi::schema;
use rustman_explore::synth::{Synthesizer, is_json_media_type};
use serde_json::{Map, Value};

use crate::negotiate;
use crate::problem::{Problem, ProblemError};
use crate::request::RequestView;

/// The most violations one problem lists.
pub const MAX_ERRORS: usize = 20;

/// A schema compiled the first time it is needed; `None` when it does not
/// compile (then nothing is checked against it).
#[derive(Debug)]
struct Lazy {
    schema: Value,
    compiled: OnceLock<Option<Validator>>,
}

impl Lazy {
    fn new(schema: Value) -> Self {
        Self {
            schema,
            compiled: OnceLock::new(),
        }
    }

    /// The first violation of `value`, as `(instance pointer, message)`.
    fn first_error(&self, value: &Value, components: &Value) -> Option<(String, String)> {
        let validator = self
            .compiled
            .get_or_init(|| schema::validator(&self.schema, components).ok())
            .as_ref()?;
        schema::errors(validator, value)
            .into_iter()
            .next()
            .map(|v| (v.instance, v.message))
    }
}

/// How a parameter's text is read before it is validated.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Shape {
    Integer,
    Number,
    Boolean,
    Array(Box<Shape>),
    Text,
}

#[derive(Debug)]
struct ParamCheck {
    name: String,
    location: ParameterLocation,
    required: bool,
    shape: Shape,
    schema: Option<Lazy>,
}

#[derive(Debug)]
struct MediaCheck {
    media_type: String,
    json: bool,
    whole: Option<Lazy>,
    required: Vec<String>,
    properties: Vec<(String, Lazy)>,
}

#[derive(Debug)]
struct BodyCheck {
    required: bool,
    media: Vec<MediaCheck>,
}

#[derive(Debug, Default)]
struct OperationCheck {
    params: Vec<ParamCheck>,
    body: Option<BodyCheck>,
}

/// The checks of every operation of a document, by operation index.
#[derive(Debug, Default)]
pub struct Checks {
    operations: Vec<OperationCheck>,
}

impl Checks {
    pub fn new(model: &ApiModel) -> Self {
        let synth = Synthesizer::new(&model.components);
        let operations = model
            .operations
            .iter()
            .map(|op| operation_check(op, &synth))
            .collect();
        Self { operations }
    }

    /// Validate `request` against operation `index`.
    pub fn validate(
        &self,
        index: usize,
        request: &RequestView<'_>,
        components: &Value,
    ) -> Result<(), Problem> {
        let Some(check) = self.operations.get(index) else {
            return Ok(());
        };
        let errors = params_errors(&check.params, request, components);
        if !errors.is_empty() {
            return Err(invalid(
                StatusCode::BAD_REQUEST,
                "invalid-parameters",
                "Invalid request parameters",
                errors,
            ));
        }
        match &check.body {
            Some(body) => body_problem(body, request, components),
            None => Ok(()),
        }
    }
}

fn operation_check(op: &Operation, synth: &Synthesizer<'_>) -> OperationCheck {
    OperationCheck {
        params: op
            .parameters
            .iter()
            .map(|p| param_check(p, synth))
            .collect(),
        body: op.request_body.as_ref().map(|b| body_check(b, synth)),
    }
}

fn param_check(param: &Parameter, synth: &Synthesizer<'_>) -> ParamCheck {
    ParamCheck {
        name: param.name.clone(),
        location: param.location,
        required: param.required || param.location == ParameterLocation::Path,
        shape: param
            .schema
            .as_ref()
            .map_or(Shape::Text, |s| shape(s, synth, 0)),
        schema: param.schema.clone().map(Lazy::new),
    }
}

fn shape(schema: &Value, synth: &Synthesizer<'_>, depth: usize) -> Shape {
    let resolved = synth.resolve(schema);
    let kind = match resolved.get("type") {
        Some(Value::String(kind)) => kind.as_str(),
        Some(Value::Array(kinds)) => kinds
            .iter()
            .filter_map(Value::as_str)
            .find(|k| *k != "null")
            .unwrap_or("string"),
        _ => "string",
    };
    match kind {
        "integer" => Shape::Integer,
        "number" => Shape::Number,
        "boolean" => Shape::Boolean,
        "array" if depth == 0 => {
            let items = resolved
                .get("items")
                .map_or(Shape::Text, |items| shape(items, synth, depth + 1));
            Shape::Array(Box::new(items))
        }
        _ => Shape::Text,
    }
}

fn body_check(body: &RequestBody, synth: &Synthesizer<'_>) -> BodyCheck {
    BodyCheck {
        required: body.required,
        media: body.content.iter().map(|m| media_check(m, synth)).collect(),
    }
}

fn media_check(media: &MediaType, synth: &Synthesizer<'_>) -> MediaCheck {
    let json = is_json_media_type(&media.media_type);
    let view = media
        .schema
        .as_ref()
        .filter(|_| json)
        .and_then(|s| synth.object_view(s))
        .unwrap_or_default();
    let required = view
        .get("required")
        .and_then(Value::as_array)
        .map(|r| {
            r.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let properties = view
        .get("properties")
        .and_then(Value::as_object)
        .map(|props| {
            props
                .iter()
                .map(|(name, s)| (name.clone(), Lazy::new(s.clone())))
                .collect()
        })
        .unwrap_or_default();
    MediaCheck {
        media_type: negotiate::essence(&media.media_type),
        json,
        whole: media.schema.clone().filter(|_| json).map(Lazy::new),
        required,
        properties,
    }
}

fn params_errors(
    params: &[ParamCheck],
    request: &RequestView<'_>,
    components: &Value,
) -> Vec<ProblemError> {
    params
        .iter()
        .filter_map(|p| param_error(p, request, components))
        .take(MAX_ERRORS)
        .collect()
}

fn param_error(
    param: &ParamCheck,
    request: &RequestView<'_>,
    components: &Value,
) -> Option<ProblemError> {
    let location = location_name(param.location);
    let pointer = format!("/{}", escape(&param.name));
    let Some(raw) = param_values(param, request) else {
        return param.required.then(|| ProblemError {
            location: location.to_owned(),
            pointer,
            message: "required".to_owned(),
        });
    };
    let value = coerce(&param.shape, &raw);
    let (at, message) = param.schema.as_ref()?.first_error(&value, components)?;
    Some(ProblemError {
        location: location.to_owned(),
        pointer: format!("{pointer}{at}"),
        message,
    })
}

/// The raw values of `param` in `request`; `None` when it is absent.
fn param_values(param: &ParamCheck, request: &RequestView<'_>) -> Option<Vec<String>> {
    let one = |v: Option<&str>| v.map(|v| vec![v.to_owned()]);
    let values = match param.location {
        ParameterLocation::Path => one(request.param(&param.name)),
        ParameterLocation::Header => one(request.header(&param.name)),
        ParameterLocation::Cookie => one(request.cookie(&param.name)),
        ParameterLocation::Query => {
            let found = request.query_values(&param.name);
            (!found.is_empty()).then(|| found.into_iter().map(str::to_owned).collect())
        }
    };
    values.filter(|v| !v.is_empty())
}

/// The JSON value `raw` stands for under `shape`; text that does not
/// parse stays text, so the schema reports the type error.
fn coerce(shape: &Shape, raw: &[String]) -> Value {
    match shape {
        Shape::Array(items) => {
            let parts: Vec<&str> = if raw.len() == 1 {
                raw[0].split(',').collect()
            } else {
                raw.iter().map(String::as_str).collect()
            };
            Value::Array(parts.into_iter().map(|p| scalar(items, p)).collect())
        }
        other => scalar(other, raw.first().map_or("", String::as_str)),
    }
}

fn scalar(shape: &Shape, text: &str) -> Value {
    let parsed = match shape {
        Shape::Integer => text.parse::<i64>().ok().map(Value::from),
        Shape::Number => text
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number),
        Shape::Boolean => match text {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            _ => None,
        },
        Shape::Array(_) | Shape::Text => None,
    };
    parsed.unwrap_or_else(|| Value::String(text.to_owned()))
}

fn body_problem(
    body: &BodyCheck,
    request: &RequestView<'_>,
    components: &Value,
) -> Result<(), Problem> {
    if request.body.is_empty() {
        return if body.required {
            Err(unprocessable(vec![body_error(
                "",
                "a request body is required",
            )]))
        } else {
            Ok(())
        };
    }
    let media = media_for(body, request.content_type())?;
    if !media.json {
        return Ok(());
    }
    let value: Value = serde_json::from_slice(request.body)
        .map_err(|e| unprocessable(vec![body_error("", &format!("invalid JSON: {e}"))]))?;
    let errors = body_errors(media, &value, components);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(unprocessable(errors))
    }
}

/// The documented media type the request's `Content-Type` falls under;
/// `415` otherwise.
fn media_for<'b>(
    body: &'b BodyCheck,
    content_type: Option<&str>,
) -> Result<&'b MediaCheck, Problem> {
    let sent = content_type.map(negotiate::essence).unwrap_or_default();
    let found = (!sent.is_empty())
        .then(|| {
            body.media
                .iter()
                .find(|m| negotiate::matches(&m.media_type, &sent))
        })
        .flatten();
    found.ok_or_else(|| {
        let documented: Vec<&str> = body.media.iter().map(|m| m.media_type.as_str()).collect();
        let sent = if sent.is_empty() {
            "no Content-Type"
        } else {
            sent.as_str()
        };
        Problem::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported-media-type",
            "Unsupported media type",
            format!(
                "{sent} is not documented; documented: {}",
                documented.join(", ")
            ),
        )
    })
}

fn body_errors(media: &MediaCheck, value: &Value, components: &Value) -> Vec<ProblemError> {
    let mut errors = Vec::new();
    if let Value::Object(object) = value {
        errors.extend(missing(&media.required, object));
        errors.extend(property_errors(media, object, components));
    }
    if errors.is_empty()
        && let Some((at, message)) = media
            .whole
            .as_ref()
            .and_then(|w| w.first_error(value, components))
    {
        errors.push(body_error(&at, &message));
    }
    errors.truncate(MAX_ERRORS);
    errors
}

fn missing<'m>(
    required: &'m [String],
    object: &'m Map<String, Value>,
) -> impl Iterator<Item = ProblemError> + 'm {
    required
        .iter()
        .filter(|name| !object.contains_key(name.as_str()))
        .map(|name| body_error(&format!("/{}", escape(name)), "required"))
}

fn property_errors<'m>(
    media: &'m MediaCheck,
    object: &'m Map<String, Value>,
    components: &'m Value,
) -> impl Iterator<Item = ProblemError> + 'm {
    media.properties.iter().filter_map(move |(name, check)| {
        let value = object.get(name)?;
        let (at, message) = check.first_error(value, components)?;
        Some(body_error(&format!("/{}{at}", escape(name)), &message))
    })
}

fn body_error(pointer: &str, message: &str) -> ProblemError {
    ProblemError {
        location: "body".to_owned(),
        pointer: pointer.to_owned(),
        message: message.to_owned(),
    }
}

fn unprocessable(errors: Vec<ProblemError>) -> Problem {
    invalid(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid-body",
        "Invalid request body",
        errors,
    )
}

fn invalid(status: StatusCode, slug: &str, title: &str, errors: Vec<ProblemError>) -> Problem {
    let noun = if errors.len() == 1 {
        "violation"
    } else {
        "violations"
    };
    let detail = format!("{} {noun} of the operation's schema", errors.len());
    Problem::new(status, slug, title, detail).with_errors(errors)
}

fn location_name(location: ParameterLocation) -> &'static str {
    match location {
        ParameterLocation::Path => "path",
        ParameterLocation::Query => "query",
        ParameterLocation::Header => "header",
        ParameterLocation::Cookie => "cookie",
    }
}

/// A JSON pointer token (RFC 6901).
fn escape(name: &str) -> String {
    name.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http::{HeaderMap, HeaderValue};
    use serde_json::json;

    fn model(op: Operation, components: Value) -> ApiModel {
        ApiModel {
            title: "t".into(),
            version: "1".into(),
            description: None,
            servers: Vec::new(),
            tags: Vec::new(),
            operations: vec![op],
            security: Vec::new(),
            security_schemes: Vec::new(),
            components,
        }
    }

    fn operation(parameters: Vec<Parameter>, request_body: Option<RequestBody>) -> Operation {
        Operation {
            pointer: "/paths/~1x/post".into(),
            operation_id: None,
            method: "POST".into(),
            path: "/x".into(),
            summary: None,
            description: None,
            tags: Vec::new(),
            parameters,
            request_body,
            responses: Vec::new(),
            security: None,
            deprecated: false,
        }
    }

    fn param(name: &str, location: ParameterLocation, schema: Value) -> Parameter {
        Parameter {
            name: name.into(),
            location,
            required: true,
            schema: Some(schema),
            example: None,
            deprecated: false,
        }
    }

    fn check(
        model: &ApiModel,
        query: &str,
        content_type: Option<&str>,
        body: &str,
    ) -> Result<(), Problem> {
        let mut headers = HeaderMap::new();
        if let Some(t) = content_type {
            headers.insert("content-type", HeaderValue::from_str(t).unwrap());
        }
        let body = Bytes::from(body.to_owned());
        let request = RequestView::new("POST", "/x", Some(query), &headers, &body);
        Checks::new(model).validate(0, &request, &model.components)
    }

    #[test]
    fn parameters_are_coerced_before_validation() {
        let params = vec![
            param(
                "n",
                ParameterLocation::Query,
                json!({"type": "integer", "maximum": 5}),
            ),
            param(
                "tags",
                ParameterLocation::Query,
                json!({"type": "array", "items": {"type": "boolean"}}),
            ),
        ];
        let model = model(operation(params, None), json!({}));
        assert!(check(&model, "n=3&tags=true,false", None, "").is_ok());
        let problem = check(&model, "n=9&tags=yes", None, "").unwrap_err();
        assert_eq!(problem.status, 400);
        let pointers: Vec<&str> = problem.errors.iter().map(|e| e.pointer.as_str()).collect();
        assert_eq!(pointers, ["/n", "/tags/0"]);
        assert!(problem.errors.iter().all(|e| e.location == "query"));
    }

    #[test]
    fn every_missing_parameter_is_listed_up_to_the_cap() {
        let params = (0..25)
            .map(|i| {
                param(
                    &format!("p{i}"),
                    ParameterLocation::Header,
                    json!({"type": "string"}),
                )
            })
            .collect();
        let model = model(operation(params, None), json!({}));
        let problem = check(&model, "", None, "").unwrap_err();
        assert_eq!(problem.errors.len(), MAX_ERRORS);
        assert_eq!(problem.errors[0].message, "required");
    }

    fn json_body(schema: Value) -> Option<RequestBody> {
        Some(RequestBody {
            required: true,
            content: vec![MediaType {
                media_type: "application/json".into(),
                schema: Some(schema),
                example: None,
                examples: Vec::new(),
            }],
        })
    }

    #[test]
    fn body_violations_are_collected_per_property() {
        let components = json!({"schemas": {"Pet": {"type": "object", "required": ["name", "kind"], "properties": {
            "name": {"type": "string"}, "age": {"type": "integer"}, "kind": {"type": "string"}}}}});
        let body = json_body(json!({"$ref": "#/components/schemas/Pet"}));
        let model = model(operation(Vec::new(), body), components);
        let ct = Some("application/json; charset=utf-8");
        assert!(check(&model, "", ct, r#"{"name": "a", "kind": "cat"}"#).is_ok());
        let problem = check(&model, "", ct, r#"{"name": 5, "age": "x"}"#).unwrap_err();
        assert_eq!(problem.status, 422);
        let pointers: Vec<&str> = problem.errors.iter().map(|e| e.pointer.as_str()).collect();
        assert_eq!(pointers, ["/kind", "/name", "/age"]);
        let not_json = check(&model, "", ct, "{").unwrap_err();
        assert_eq!(not_json.status, 422);
        let whole = check(&model, "", ct, "[1]").unwrap_err();
        assert_eq!(whole.errors[0].pointer, "");
    }

    #[test]
    fn undocumented_media_types_and_missing_bodies() {
        let model = model(
            operation(Vec::new(), json_body(json!({"type": "object"}))),
            json!({}),
        );
        assert_eq!(
            check(&model, "", Some("text/plain"), "hi")
                .unwrap_err()
                .status,
            415
        );
        assert_eq!(check(&model, "", None, "{}").unwrap_err().status, 415);
        assert_eq!(check(&model, "", None, "").unwrap_err().status, 422);
    }

    #[test]
    fn pointer_tokens_are_escaped() {
        assert_eq!(escape("a/b~c"), "a~1b~0c");
    }
}
