//! The Pact file model (design D4): HTTP interactions of Pact Specification
//! v3 and v4, written and read with our own serde code. Message
//! interactions and plugins are out of scope.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The Pact Specification version of a file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PactVersion {
    V3,
    #[default]
    V4,
}

impl PactVersion {
    /// `metadata.pactSpecification.version`.
    pub fn spec_version(self) -> &'static str {
        match self {
            Self::V3 => "3.0.0",
            Self::V4 => "4.0",
        }
    }

    /// The version a `metadata.pactSpecification.version` names.
    pub fn parse(text: &str) -> Option<Self> {
        match text.split('.').next()? {
            "3" => Some(Self::V3),
            "4" => Some(Self::V4),
            _ => None,
        }
    }
}

/// One Pact file: the interactions between a consumer and a provider.
#[derive(Clone, Debug, PartialEq)]
pub struct Pact {
    pub consumer: String,
    pub provider: String,
    pub version: PactVersion,
    pub interactions: Vec<Interaction>,
}

/// One HTTP request and the response the consumer expects.
#[derive(Clone, Debug, PartialEq)]
pub struct Interaction {
    pub description: String,
    pub provider_states: Vec<ProviderState>,
    pub request: PactRequest,
    pub response: PactResponse,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderState {
    pub name: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub params: Map<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PactRequest {
    pub method: String,
    pub path: String,
    /// Each parameter with its values, in order.
    pub query: Vec<(String, Vec<String>)>,
    pub headers: Vec<(String, String)>,
    pub body: Option<Value>,
    pub rules: Rules,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PactResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Option<Value>,
    pub rules: Rules,
}

/// The matching rules of a request or response, by category.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rules {
    /// By JSON path (`$.id`).
    pub body: Vec<(String, RuleSet)>,
    /// By header name.
    pub header: Vec<(String, RuleSet)>,
    /// By query parameter name.
    pub query: Vec<(String, RuleSet)>,
    pub path: Option<RuleSet>,
    pub status: Option<RuleSet>,
}

impl Rules {
    pub fn is_empty(&self) -> bool {
        self.body.is_empty()
            && self.header.is_empty()
            && self.query.is_empty()
            && self.path.is_none()
            && self.status.is_none()
    }

    /// The rules of header `name` (any case).
    pub fn header(&self, name: &str) -> Option<&RuleSet> {
        self.header
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, r)| r)
    }
}

/// The matchers of one place, combined with `AND` (every one must hold) or
/// `OR`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RuleSet {
    #[serde(default = "and")]
    pub combine: String,
    pub matchers: Vec<Matcher>,
}

fn and() -> String {
    "AND".to_owned()
}

impl RuleSet {
    pub fn one(matcher: Matcher) -> Self {
        Self {
            combine: and(),
            matchers: vec![matcher],
        }
    }

    pub fn is_or(&self) -> bool {
        self.combine.eq_ignore_ascii_case("OR")
    }
}

/// One Pact matcher: `{"match": "regex", "regex": "^a$"}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Matcher {
    #[serde(rename = "match")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<usize>,
    /// `statusCode`: a class name (`success`) or a list of codes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Value>,
}

impl Matcher {
    pub fn new(kind: &str) -> Self {
        Self {
            kind: kind.to_owned(),
            regex: None,
            value: None,
            min: None,
            max: None,
            status: None,
        }
    }

    pub fn regex(pattern: &str) -> Self {
        Self {
            regex: Some(pattern.to_owned()),
            ..Self::new("regex")
        }
    }
}

impl Pact {
    /// The Pact file as JSON, in the shape of its version.
    pub fn to_json(&self) -> Value {
        let interactions: Vec<Value> = self
            .interactions
            .iter()
            .map(|i| interaction_json(i, self.version))
            .collect();
        json!({
            "consumer": {"name": self.consumer},
            "provider": {"name": self.provider},
            "interactions": interactions,
            "metadata": {
                "pactSpecification": {"version": self.version.spec_version()},
                "rustman": {"version": rustman_core::VERSION},
            },
        })
    }
}

fn interaction_json(interaction: &Interaction, version: PactVersion) -> Value {
    let mut out = Map::new();
    if version == PactVersion::V4 {
        out.insert("type".into(), json!("Synchronous/HTTP"));
    }
    out.insert("description".into(), json!(interaction.description));
    if !interaction.provider_states.is_empty() {
        out.insert("providerStates".into(), json!(interaction.provider_states));
    }
    out.insert(
        "request".into(),
        request_json(&interaction.request, version),
    );
    out.insert(
        "response".into(),
        response_json(&interaction.response, version),
    );
    Value::Object(out)
}

fn request_json(request: &PactRequest, version: PactVersion) -> Value {
    let mut out = Map::new();
    out.insert("method".into(), json!(request.method));
    out.insert("path".into(), json!(request.path));
    if !request.query.is_empty() {
        let query: Map<String, Value> = request
            .query
            .iter()
            .map(|(name, values)| (name.clone(), json!(values)))
            .collect();
        out.insert("query".into(), Value::Object(query));
    }
    common_json(&mut out, &request.headers, request.body.as_ref(), version);
    if !request.rules.is_empty() {
        out.insert("matchingRules".into(), rules_json(&request.rules));
    }
    Value::Object(out)
}

fn response_json(response: &PactResponse, version: PactVersion) -> Value {
    let mut out = Map::new();
    out.insert("status".into(), json!(response.status));
    common_json(&mut out, &response.headers, response.body.as_ref(), version);
    if !response.rules.is_empty() {
        out.insert("matchingRules".into(), rules_json(&response.rules));
    }
    Value::Object(out)
}

/// Headers and body: a v4 body is wrapped with its content type.
fn common_json(
    out: &mut Map<String, Value>,
    headers: &[(String, String)],
    body: Option<&Value>,
    version: PactVersion,
) {
    if !headers.is_empty() {
        let map: Map<String, Value> = headers
            .iter()
            .map(|(name, value)| (name.clone(), json!(value)))
            .collect();
        out.insert("headers".into(), Value::Object(map));
    }
    let Some(body) = body else { return };
    let body = match version {
        PactVersion::V3 => body.clone(),
        PactVersion::V4 => json!({
            "content": body,
            "contentType": content_type(headers).unwrap_or("application/json"),
            "contentTypeHint": "TEXT",
            "encoded": false,
        }),
    };
    out.insert("body".into(), body);
}

fn content_type(headers: &[(String, String)]) -> Option<&str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.as_str())
}

fn rules_json(rules: &Rules) -> Value {
    let keyed = |sets: &[(String, RuleSet)]| -> Value {
        let map: Map<String, Value> = sets
            .iter()
            .map(|(key, set)| (key.clone(), json!(set)))
            .collect();
        Value::Object(map)
    };
    let mut out = Map::new();
    if !rules.body.is_empty() {
        out.insert("body".into(), keyed(&rules.body));
    }
    if !rules.header.is_empty() {
        out.insert("header".into(), keyed(&rules.header));
    }
    if !rules.query.is_empty() {
        out.insert("query".into(), keyed(&rules.query));
    }
    if let Some(path) = &rules.path {
        out.insert("path".into(), json!(path));
    }
    if let Some(status) = &rules.status {
        out.insert("status".into(), json!({"$": status}));
    }
    Value::Object(out)
}
