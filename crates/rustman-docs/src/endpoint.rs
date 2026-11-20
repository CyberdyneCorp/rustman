//! One request's page (api-docs-generation "Endpoint page content" and
//! "Other request kinds").

use rustman_core::RequestRef;
use rustman_core::format::from_yaml;
use rustman_core::format::write::{assertions_to_yaml, request_to_yaml};
use rustman_core::model::{
    Assertion, Auth, Body, Capture, CaptureSource, HttpRequest, JsonContent, PartSource, Request,
};
use serde_json::{Map, Value};

use crate::commonmark::{Html, to_html};
use crate::model::{Endpoint, Row, Schema};
use crate::openapi::Operation;
use crate::rendered::{DocsRedactor, Rendered};
use crate::site::SiteInput;

/// Request-file keys a generic page leaves out: shown elsewhere on the
/// page, or values that are not documentation (request variables; a
/// GraphQL request's `variables` are its operation inputs and are kept).
const GENERIC_SKIPPED: [&str; 9] = [
    "version",
    "kind",
    "name",
    "description",
    "tags",
    "docs",
    "variables",
    "assertions",
    "captures",
];

/// The page of `request`.
pub fn endpoint(
    slug: String,
    section: &str,
    request: &RequestRef<'_>,
    input: &SiteInput<'_>,
) -> Endpoint {
    let r = input.redactor;
    let entry = request.entry;
    let mut page = Endpoint {
        slug,
        section: section.to_owned(),
        id: r.render(&entry.id()),
        name: r.render(entry.request.name()),
        kind: r.render(entry.request.kind()),
        method: None,
        url: None,
        summary: None,
        description: None,
        deprecated: false,
        query: Vec::new(),
        headers: Vec::new(),
        body_type: None,
        body_fields: Vec::new(),
        body_text: None,
        auth: request
            .effective_auth()
            .and_then(auth_scheme)
            .map(|s| r.render(s)),
        expected: Vec::new(),
        produces: Vec::new(),
        fields: Vec::new(),
        schemas: Vec::new(),
        example: input.examples.example(&input.workspace.root, request, r),
        samples: input
            .samples
            .for_request(request, &input.workspace.settings, r),
    };
    match &entry.request {
        Request::Http(http) => {
            let op = input.openapi.find(&request.ancestors, http);
            http_page(&mut page, http, request, op, r);
        }
        other => {
            page.description = description(other.description(), None, other, r);
            page.fields = generic_fields(other, r);
        }
    }
    page
}

fn http_page(
    page: &mut Endpoint,
    http: &HttpRequest,
    request: &RequestRef<'_>,
    op: Option<&Operation>,
    r: &DocsRedactor,
) {
    let op_text = |f: fn(&Operation) -> Option<&String>| op.and_then(f).map(String::as_str);
    let param = |name: &str| op.and_then(|o| o.parameters.get(name)).map(|d| r.render(d));
    page.method = Some(r.render(http.effective_method()));
    page.url = Some(r.render(&http.url));
    page.summary = op_text(|o| o.summary.as_ref()).map(|s| r.render(s));
    page.deprecated = op.is_some_and(|o| o.deprecated);
    let fallback = op_text(|o| o.description.as_ref());
    page.description = description(
        http.description.as_deref(),
        fallback,
        &request.entry.request,
        r,
    );
    page.query = http
        .query
        .iter()
        .flatten()
        .filter(|q| q.enabled)
        .map(|q| row(r.render(&q.name), r.render(&q.value), param(&q.name)))
        .collect();
    page.headers = request
        .effective_headers()
        .iter()
        .map(|h| {
            row(
                r.render(&h.name),
                r.header_value(&h.name, &h.value),
                param(&h.name),
            )
        })
        .collect();
    if let Some(body) = &http.body {
        body_section(page, body, r);
    }
    page.expected = http
        .assertions
        .iter()
        .flatten()
        .map(|a| expected(a, r))
        .collect();
    page.produces = http
        .captures
        .iter()
        .flatten()
        .map(|c| produces(c, r))
        .collect();
    page.schemas = op.map(|o| schemas(o, r)).unwrap_or_default();
}

/// The request's description (or `fallback`, the OpenAPI one), then its
/// `docs.description`, as CommonMark.
fn description(
    own: Option<&str>,
    fallback: Option<&str>,
    request: &Request,
    r: &DocsRedactor,
) -> Option<Html> {
    let extra = request.docs().and_then(|d| d.description.as_deref());
    let parts: Vec<&str> = [own.or(fallback), extra].into_iter().flatten().collect();
    (!parts.is_empty()).then(|| to_html(&r.render(&parts.join("\n\n"))))
}

fn row(name: Rendered, value: Rendered, description: Option<Rendered>) -> Row {
    Row {
        name,
        value,
        description,
    }
}

fn body_section(page: &mut Endpoint, body: &Body, r: &DocsRedactor) {
    let (kind, fields, text): (&str, Vec<Row>, Option<String>) = match body {
        Body::None => ("none", Vec::new(), None),
        Body::Json(JsonContent::Value(value)) => json_body(value, r),
        Body::Json(JsonContent::Text(text)) => match serde_json::from_str::<Value>(text) {
            Ok(value) => json_body(&value, r),
            Err(_) => ("json", Vec::new(), Some(text.clone())),
        },
        Body::Text(text) => ("text", Vec::new(), Some(text.clone())),
        Body::Form(fields) => {
            let rows = fields
                .iter()
                .filter(|f| f.enabled)
                .map(|f| row(r.render(&f.name), r.render(&f.value), None))
                .collect();
            ("form", rows, None)
        }
        Body::Multipart(parts) => {
            let rows = parts
                .iter()
                .map(|p| {
                    let value = match &p.source {
                        PartSource::Text(text) => text.clone(),
                        PartSource::File(path) => format!("file: {}", path.get()),
                    };
                    row(r.render(&p.name), r.render(&value), None)
                })
                .collect();
            ("multipart", rows, None)
        }
        Body::File(path) => ("file", Vec::new(), Some(path.get().clone())),
    };
    page.body_type = Some(r.render(kind));
    page.body_fields = fields;
    page.body_text = text.map(|t| r.render(&t));
}

/// An object body as one row per top-level field; anything else as text.
fn json_body(value: &Value, r: &DocsRedactor) -> (&'static str, Vec<Row>, Option<String>) {
    match value {
        Value::Object(map) => ("json", object_rows(map, r), None),
        other => (
            "json",
            Vec::new(),
            Some(serde_json::to_string_pretty(other).unwrap_or_default()),
        ),
    }
}

fn object_rows(map: &Map<String, Value>, r: &DocsRedactor) -> Vec<Row> {
    map.iter()
        .map(|(name, value)| row(r.render(name), r.render(&scalar_text(value)), None))
        .collect()
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// `Expected: status 201`: the assertion as its file writes it, on one line
/// when it fits.
fn expected(assertion: &Assertion, r: &DocsRedactor) -> Rendered {
    let yaml = assertions_to_yaml(std::slice::from_ref(assertion));
    let text = yaml
        .trim_end()
        .strip_prefix("- ")
        .unwrap_or(yaml.trim_end());
    let text = if text.contains('\n') {
        text.to_owned()
    } else {
        text.replacen(": ", " ", 1)
    };
    r.render(&format!("Expected: {text}"))
}

/// `Produces: {{token}} from json $.token`.
fn produces(capture: &Capture, r: &DocsRedactor) -> Rendered {
    let detail = match capture.from {
        CaptureSource::Json => capture.path.clone(),
        CaptureSource::BodyRegex => capture.pattern.clone(),
        CaptureSource::Header | CaptureSource::Cookie | CaptureSource::Metadata => {
            capture.name.clone()
        }
        CaptureSource::Status => None,
    };
    let source = capture.from.as_str();
    let text = match detail {
        Some(detail) => format!("Produces: {{{{{}}}}} from {source} {detail}", capture.var),
        None => format!("Produces: {{{{{}}}}} from {source}", capture.var),
    };
    r.render(&text)
}

fn schemas(op: &Operation, r: &DocsRedactor) -> Vec<Schema> {
    let request = op.request_schema.iter().map(|s| Schema {
        label: r.render("Request body"),
        json: r.json(s),
    });
    let responses = op.response_schemas.iter().map(|(code, s)| Schema {
        label: r.render(&format!("Response {code}")),
        json: r.json(s),
    });
    request.chain(responses).collect()
}

/// The scheme name of `auth`; `None` for `type: none`.
fn auth_scheme(auth: &Auth) -> Option<&'static str> {
    Some(match auth {
        Auth::None => return None,
        Auth::Basic { .. } => "basic",
        Auth::Bearer { .. } => "bearer",
        Auth::ApiKey { .. } => "api_key",
        Auth::OAuth2(_) => "oauth2",
        Auth::Digest { .. } => "digest",
        Auth::AwsSigV4(_) => "aws_sigv4",
        Auth::Jwt(_) => "jwt",
    })
}

/// The request-file fields of a kind without a dedicated page, as written:
/// header and metadata values are masked like an HTTP page's, and `auth`
/// shows its type only.
fn generic_fields(request: &Request, r: &DocsRedactor) -> Vec<Row> {
    let yaml = request_to_yaml(request);
    let path = std::path::Path::new("request.rman.yaml");
    let Ok(Value::Object(map)) = from_yaml::<Value>(path, &yaml) else {
        return Vec::new();
    };
    let graphql = matches!(request, Request::Graphql(_));
    map.iter()
        .filter(|(key, _)| {
            (graphql && key.as_str() == "variables") || !GENERIC_SKIPPED.contains(&key.as_str())
        })
        .map(|(key, value)| row(r.render(key), generic_value(key, value, r), None))
        .collect()
}

fn generic_value(key: &str, value: &Value, r: &DocsRedactor) -> Rendered {
    match (key, value) {
        ("auth", Value::Object(auth)) => {
            r.render(auth.get("type").and_then(Value::as_str).unwrap_or("custom"))
        }
        ("headers" | "metadata", Value::Array(pairs)) => {
            let lines: Vec<String> = pairs.iter().filter_map(|p| pair_line(p, r)).collect();
            r.render(&lines.join("\n"))
        }
        _ => r.render(&scalar_text(value)),
    }
}

fn pair_line(pair: &Value, r: &DocsRedactor) -> Option<String> {
    let name = pair.get("name")?.as_str()?;
    let value = scalar_text(pair.get("value")?);
    Some(format!("{name}: {}", r.header_value(name, &value)))
}
