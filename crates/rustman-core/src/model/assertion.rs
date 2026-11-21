//! Declarative assertions. Each list item has one key naming its type
//! (`- status: 200`, `- json: {...}`). A hand-written `Deserialize` reads that
//! key and then the matching value, which keeps error positions exact (D4)
//! and gives a clear message for an item with more than one key.

use std::fmt;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use super::common::{Checked, Validate, exactly_one, given};
use super::snapshot::SnapshotAssertion;
use crate::format::Located;

#[derive(Clone, Debug, PartialEq)]
pub enum Assertion {
    Status(StatusExpectation),
    Header(Checked<HeaderAssertion>),
    Json(Box<Checked<JsonAssertion>>),
    Schema(Checked<SchemaAssertion>),
    Body(Checked<BodyAssertion>),
    Latency(Checked<LatencyAssertion>),
    /// `no_errors: true`: a GraphQL result without `errors` (`false`: with).
    NoErrors(bool),
    /// `errors_count: 0` or `errors_count: {gt: 0}`.
    ErrorsCount(CountExpectation),
    /// `error_message_contains: text`: some GraphQL error's message has it.
    ErrorMessageContains(String),
    /// `error_code: CODE`: some GraphQL error's `extensions.code` is it.
    ErrorCode(String),
    /// `data: {path, <op>: value}`: a JSONPath check rooted at `data`.
    Data(Box<Checked<JsonAssertion>>),
    /// `rpc_error: {code, message_contains}` or `rpc_error: none`: how a
    /// JSON-RPC call ended (add-jsonrpc-client).
    RpcError(RpcErrorExpectation),
    /// `grpc_status: NOT_FOUND` or `grpc_status: 5`: the final status of a
    /// gRPC call (add-grpc-client).
    GrpcStatus(GrpcStatusExpectation),
    /// `metadata: {name, equals | contains | matches | exists}`: a gRPC
    /// response header or trailer.
    Metadata(Checked<MetadataAssertion>),
    /// `snapshot` or `snapshot: {...}`: compare with a stored snapshot.
    Snapshot(Box<Checked<SnapshotAssertion>>),
    /// `is_error: true`: an MCP tool result with `isError` (`false`: without).
    IsError(bool),
    /// `content_text_contains: text`: some text content item of an MCP
    /// result has it.
    ContentTextContains(String),
    /// `structured: {path, <op>: value}`: a JSONPath check rooted at an MCP
    /// tool result's `structuredContent`.
    Structured(Box<Checked<JsonAssertion>>),
    /// `output_schema: true`: `structuredContent` matches the tool's
    /// `outputSchema` (implicit for tools that declare one).
    OutputSchema(bool),
    /// `tool_exists: name`: an MCP `tools/list` result has the tool.
    ToolExists(String),
    /// `messages: {count_gte | count_lte | count_eq}`: how many messages a
    /// realtime session received.
    Messages(Checked<MessagesAssertion>),
    /// `close_code: 1000`: the close code of a WebSocket session.
    CloseCode(u16),
    /// `plugin: {fn: <plugin>/<fn>, ...args}`: an assertion function of a
    /// plugin (add-plugin-system).
    Plugin(PluginAssertion),
    /// `evm: {...}`: a check of a decoded EVM call (add-evm-toolkit),
    /// evaluated by `rustman-evm`.
    Evm(Box<Checked<super::evm_check::EvmAssertion>>),
}

/// The key naming an assertion's type.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AssertionKey {
    Status,
    Header,
    Json,
    Schema,
    Body,
    Latency,
    NoErrors,
    ErrorsCount,
    ErrorMessageContains,
    ErrorCode,
    Data,
    RpcError,
    GrpcStatus,
    Metadata,
    Snapshot,
    IsError,
    ContentTextContains,
    Structured,
    OutputSchema,
    ToolExists,
    Messages,
    CloseCode,
    Plugin,
    Evm,
}

impl<'de> Deserialize<'de> for Assertion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(AssertionVisitor)
    }
}

struct AssertionVisitor;

impl<'de> Visitor<'de> for AssertionVisitor {
    type Value = Assertion;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an assertion such as `status: 200` (one key naming its type)")
    }

    /// The only assertion written as a bare word: `- snapshot`.
    fn visit_str<E: de::Error>(self, v: &str) -> Result<Assertion, E> {
        if v == "snapshot" {
            let default = Checked::new(SnapshotAssertion::default()).map_err(E::custom)?;
            return Ok(Assertion::Snapshot(Box::new(default)));
        }
        Err(E::custom(format!(
            "unknown assertion `{v}` (only `snapshot` may be written without a value)"
        )))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Assertion, A::Error> {
        let Some(key) = map.next_key::<AssertionKey>()? else {
            return Err(de::Error::custom(
                "empty assertion (expected one key naming its type)",
            ));
        };
        let assertion = match key {
            AssertionKey::Status => Assertion::Status(map.next_value()?),
            AssertionKey::Header => Assertion::Header(map.next_value()?),
            AssertionKey::Json => Assertion::Json(map.next_value()?),
            AssertionKey::Schema => Assertion::Schema(map.next_value()?),
            AssertionKey::Body => Assertion::Body(map.next_value()?),
            AssertionKey::Latency => Assertion::Latency(map.next_value()?),
            AssertionKey::NoErrors => Assertion::NoErrors(map.next_value()?),
            AssertionKey::ErrorsCount => Assertion::ErrorsCount(map.next_value()?),
            AssertionKey::ErrorMessageContains => {
                Assertion::ErrorMessageContains(map.next_value()?)
            }
            AssertionKey::ErrorCode => Assertion::ErrorCode(map.next_value()?),
            AssertionKey::Data => Assertion::Data(map.next_value()?),
            AssertionKey::RpcError => Assertion::RpcError(map.next_value()?),
            AssertionKey::GrpcStatus => Assertion::GrpcStatus(map.next_value()?),
            AssertionKey::Metadata => Assertion::Metadata(map.next_value()?),
            AssertionKey::Snapshot => Assertion::Snapshot(map.next_value()?),
            AssertionKey::IsError => Assertion::IsError(map.next_value()?),
            AssertionKey::ContentTextContains => Assertion::ContentTextContains(map.next_value()?),
            AssertionKey::Structured => Assertion::Structured(map.next_value()?),
            AssertionKey::OutputSchema => Assertion::OutputSchema(map.next_value()?),
            AssertionKey::ToolExists => Assertion::ToolExists(map.next_value()?),
            AssertionKey::Messages => Assertion::Messages(map.next_value()?),
            AssertionKey::CloseCode => Assertion::CloseCode(map.next_value()?),
            AssertionKey::Plugin => Assertion::Plugin(map.next_value()?),
            AssertionKey::Evm => Assertion::Evm(map.next_value()?),
        };
        if let Some(extra) = map.next_key::<String>()? {
            return Err(de::Error::custom(format!(
                "an assertion has exactly one key; found `{extra}` after `{}` (start a new `- ` item)",
                key.name()
            )));
        }
        Ok(assertion)
    }
}

impl AssertionKey {
    fn name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Header => "header",
            Self::Json => "json",
            Self::Schema => "schema",
            Self::Body => "body",
            Self::Latency => "latency",
            Self::NoErrors => "no_errors",
            Self::ErrorsCount => "errors_count",
            Self::ErrorMessageContains => "error_message_contains",
            Self::ErrorCode => "error_code",
            Self::Data => "data",
            Self::RpcError => "rpc_error",
            Self::GrpcStatus => "grpc_status",
            Self::Metadata => "metadata",
            Self::Snapshot => "snapshot",
            Self::IsError => "is_error",
            Self::ContentTextContains => "content_text_contains",
            Self::Structured => "structured",
            Self::OutputSchema => "output_schema",
            Self::ToolExists => "tool_exists",
            Self::Messages => "messages",
            Self::CloseCode => "close_code",
            Self::Plugin => "plugin",
            Self::Evm => "evm",
        }
    }
}

/// `plugin: {fn: <plugin>/<fn>, ...args}`: the function to call and the
/// other fields as its arguments (a JSON object).
#[derive(Clone, Debug, PartialEq)]
pub struct PluginAssertion {
    pub function: String,
    pub args: serde_json::Map<String, serde_json::Value>,
}

impl<'de> Deserialize<'de> for PluginAssertion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut args = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
        let function = match args.remove("fn") {
            Some(serde_json::Value::String(function)) => function,
            Some(_) => return Err(de::Error::custom("plugin assertion: 'fn' must be a string")),
            None => return Err(de::Error::custom("plugin assertion requires 'fn'")),
        };
        if crate::plugins::split_qualified(&function).is_none() {
            return Err(de::Error::custom(format!(
                "plugin assertion: invalid fn '{function}' (expected <plugin>/<function>)"
            )));
        }
        Ok(Self { function, args })
    }
}

/// `status: 200`, `status: [200, 201]` or `status: "2xx"`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusExpectation {
    Code(u16),
    AnyOf(Vec<u16>),
    /// The leading digit of a class pattern such as `2xx`.
    Class(u8),
}

impl<'de> Deserialize<'de> for StatusExpectation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StatusVisitor)
    }
}

struct StatusVisitor;

fn status_code<E: de::Error>(value: u64) -> Result<u16, E> {
    u16::try_from(value)
        .ok()
        .filter(|code| (100..=599).contains(code))
        .ok_or_else(|| E::custom(format!("invalid status code {value} (expected 100-599)")))
}

impl<'de> Visitor<'de> for StatusVisitor {
    type Value = StatusExpectation;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a status code, a list of codes or a class such as \"2xx\"")
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        status_code(v).map(StatusExpectation::Code)
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        let v = u64::try_from(v).map_err(|_| E::custom(format!("invalid status code {v}")))?;
        self.visit_u64(v)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        let bytes = v.as_bytes();
        if let [digit @ b'1'..=b'5', b'x' | b'X', b'x' | b'X'] = bytes {
            return Ok(StatusExpectation::Class(digit - b'0'));
        }
        Err(E::custom(format!(
            "invalid status pattern '{v}' (expected a code or a class such as 2xx)"
        )))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut codes = Vec::new();
        while let Some(code) = seq.next_element::<u64>()? {
            codes.push(status_code(code)?);
        }
        Ok(StatusExpectation::AnyOf(codes))
    }
}

/// `header: {name, equals | contains | matches | exists}`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderAssertion {
    pub name: String,
    pub equals: Option<String>,
    pub contains: Option<String>,
    pub matches: Option<String>,
    pub exists: Option<bool>,
}

impl Validate for HeaderAssertion {
    fn validate(&self) -> Result<(), String> {
        exactly_one(
            "header assertion",
            &[
                ("equals", given(self.equals.is_some())),
                ("contains", given(self.contains.is_some())),
                ("matches", given(self.matches.is_some())),
                ("exists", given(self.exists.is_some())),
            ],
        )
    }
}

/// `metadata: {name, equals | contains | matches | exists}`: a header
/// assertion over the metadata (headers and trailers) of a gRPC call.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(transparent)]
pub struct MetadataAssertion(pub HeaderAssertion);

impl std::ops::Deref for MetadataAssertion {
    type Target = HeaderAssertion;

    fn deref(&self) -> &HeaderAssertion {
        &self.0
    }
}

impl Validate for MetadataAssertion {
    fn validate(&self) -> Result<(), String> {
        self.0
            .validate()
            .map_err(|e| e.replacen("header assertion", "metadata assertion", 1))
    }
}

/// `json: {path, <op>: value}` with one operator.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonAssertion {
    pub path: String,
    pub equals: Option<serde_json::Value>,
    pub not_equals: Option<serde_json::Value>,
    pub contains: Option<serde_json::Value>,
    pub matches: Option<String>,
    pub gt: Option<serde_json::Value>,
    pub gte: Option<serde_json::Value>,
    pub lt: Option<serde_json::Value>,
    pub lte: Option<serde_json::Value>,
    pub exists: Option<bool>,
    pub length: Option<serde_json::Value>,
    #[serde(rename = "type")]
    pub value_type: Option<String>,
}

impl Validate for JsonAssertion {
    fn validate(&self) -> Result<(), String> {
        exactly_one(
            "json assertion",
            &[
                ("equals", given(self.equals.is_some())),
                ("not_equals", given(self.not_equals.is_some())),
                ("contains", given(self.contains.is_some())),
                ("matches", given(self.matches.is_some())),
                ("gt", given(self.gt.is_some())),
                ("gte", given(self.gte.is_some())),
                ("lt", given(self.lt.is_some())),
                ("lte", given(self.lte.is_some())),
                ("exists", given(self.exists.is_some())),
                ("length", given(self.length.is_some())),
                ("type", given(self.value_type.is_some())),
            ],
        )
    }
}

/// `schema: {file: <path>}` or `schema: {inline: {...}}`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaAssertion {
    pub file: Option<Located<String>>,
    pub inline: Option<serde_json::Value>,
}

impl Validate for SchemaAssertion {
    fn validate(&self) -> Result<(), String> {
        exactly_one(
            "schema assertion",
            &[
                ("file", given(self.file.is_some())),
                ("inline", given(self.inline.is_some())),
            ],
        )
    }
}

/// `body: {contains | not_contains | matches | equals}`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodyAssertion {
    pub contains: Option<String>,
    pub not_contains: Option<String>,
    pub matches: Option<String>,
    pub equals: Option<String>,
}

impl Validate for BodyAssertion {
    fn validate(&self) -> Result<(), String> {
        exactly_one(
            "body assertion",
            &[
                ("contains", given(self.contains.is_some())),
                ("not_contains", given(self.not_contains.is_some())),
                ("matches", given(self.matches.is_some())),
                ("equals", given(self.equals.is_some())),
            ],
        )
    }
}

/// `latency: {total_lt: <ms>, ttfb_lt: <ms>}`; at least one bound.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatencyAssertion {
    pub total_lt: Option<u64>,
    pub ttfb_lt: Option<u64>,
}

impl Validate for LatencyAssertion {
    fn validate(&self) -> Result<(), String> {
        if self.total_lt.is_none() && self.ttfb_lt.is_none() {
            return Err("latency assertion needs total_lt or ttfb_lt".to_owned());
        }
        Ok(())
    }
}

/// `messages: {count_gte | count_lte | count_eq}` with one bound.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessagesAssertion {
    pub count_gte: Option<u64>,
    pub count_lte: Option<u64>,
    pub count_eq: Option<u64>,
}

impl MessagesAssertion {
    /// The bound as a comparison.
    pub fn expectation(&self) -> CountExpectation {
        let bounds = [
            (CountOp::Gte, self.count_gte),
            (CountOp::Lte, self.count_lte),
            (CountOp::Equals, self.count_eq),
        ];
        bounds
            .into_iter()
            .find_map(|(op, value)| value.map(|value| CountExpectation { op, value }))
            .unwrap_or(CountExpectation {
                op: CountOp::Gte,
                value: 0,
            })
    }
}

impl Validate for MessagesAssertion {
    fn validate(&self) -> Result<(), String> {
        exactly_one(
            "messages assertion",
            &[
                ("count_gte", given(self.count_gte.is_some())),
                ("count_lte", given(self.count_lte.is_some())),
                ("count_eq", given(self.count_eq.is_some())),
            ],
        )
    }
}

/// The expected number of GraphQL errors: `errors_count: 2`, or one
/// comparison such as `errors_count: {gt: 0}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CountExpectation {
    pub op: CountOp,
    pub value: u64,
}

/// How [`CountExpectation`] compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CountOp {
    Equals,
    Gt,
    Gte,
    Lt,
    Lte,
}

impl CountOp {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Equals => "equals",
            Self::Gt => "gt",
            Self::Gte => "gte",
            Self::Lt => "lt",
            Self::Lte => "lte",
        }
    }

    /// Whether `actual` satisfies the comparison with `expected`.
    pub fn holds(self, actual: u64, expected: u64) -> bool {
        match self {
            Self::Equals => actual == expected,
            Self::Gt => actual > expected,
            Self::Gte => actual >= expected,
            Self::Lt => actual < expected,
            Self::Lte => actual <= expected,
        }
    }
}

/// The mapping form of `errors_count`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct CountComparison {
    equals: Option<u64>,
    gt: Option<u64>,
    gte: Option<u64>,
    lt: Option<u64>,
    lte: Option<u64>,
}

impl Validate for CountComparison {
    fn validate(&self) -> Result<(), String> {
        exactly_one(
            "errors_count assertion",
            &[
                ("equals", given(self.equals.is_some())),
                ("gt", given(self.gt.is_some())),
                ("gte", given(self.gte.is_some())),
                ("lt", given(self.lt.is_some())),
                ("lte", given(self.lte.is_some())),
            ],
        )
    }
}

impl CountComparison {
    fn expectation(&self) -> Option<CountExpectation> {
        [
            (CountOp::Equals, self.equals),
            (CountOp::Gt, self.gt),
            (CountOp::Gte, self.gte),
            (CountOp::Lt, self.lt),
            (CountOp::Lte, self.lte),
        ]
        .into_iter()
        .find_map(|(op, value)| value.map(|value| CountExpectation { op, value }))
    }
}

impl<'de> Deserialize<'de> for CountExpectation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(CountVisitor)
    }
}

struct CountVisitor;

impl<'de> Visitor<'de> for CountVisitor {
    type Value = CountExpectation;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a number of errors, or one of equals, gt, gte, lt, lte")
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(CountExpectation {
            op: CountOp::Equals,
            value,
        })
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        let v = u64::try_from(v).map_err(|_| E::custom(format!("invalid errors_count {v}")))?;
        self.visit_u64(v)
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        let checked: Checked<CountComparison> =
            Deserialize::deserialize(de::value::MapAccessDeserializer::new(map))?;
        checked
            .expectation()
            .ok_or_else(|| de::Error::custom("errors_count needs a comparison"))
    }
}

/// `rpc_error: {code: -32602, message_contains: text}` (the call failed
/// with that error) or `rpc_error: none` (the call succeeded).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RpcErrorExpectation {
    None,
    Code {
        code: i64,
        message_contains: Option<String>,
    },
}

/// The mapping form of `rpc_error`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RpcErrorFields {
    code: i64,
    message_contains: Option<String>,
}

impl<'de> Deserialize<'de> for RpcErrorExpectation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(RpcErrorVisitor)
    }
}

struct RpcErrorVisitor;

impl<'de> Visitor<'de> for RpcErrorVisitor {
    type Value = RpcErrorExpectation;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("`none` or a map with `code` (and optionally `message_contains`)")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        if v == "none" {
            return Ok(RpcErrorExpectation::None);
        }
        Err(E::custom(format!(
            "invalid rpc_error '{v}' (expected `none` or {{code: <int>}})"
        )))
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        let fields: RpcErrorFields =
            Deserialize::deserialize(de::value::MapAccessDeserializer::new(map))?;
        Ok(RpcErrorExpectation::Code {
            code: fields.code,
            message_contains: fields.message_contains,
        })
    }
}

/// `grpc_status: OK`, `grpc_status: not_found` or `grpc_status: 5`: the
/// expected gRPC status code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrpcStatusExpectation {
    pub code: i32,
}

impl GrpcStatusExpectation {
    /// `OK`: what a gRPC request without assertions expects.
    pub const OK: Self = Self { code: 0 };

    /// The canonical name of the expected code, such as `NOT_FOUND`.
    pub fn name(self) -> &'static str {
        super::grpc::grpc_code_name(self.code)
    }
}

impl<'de> Deserialize<'de> for GrpcStatusExpectation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(GrpcStatusVisitor)
    }
}

struct GrpcStatusVisitor;

fn grpc_code<E: de::Error>(code: Option<i32>, shown: &str) -> Result<GrpcStatusExpectation, E> {
    let valid = super::grpc::GRPC_CODE_NAMES.len();
    code.filter(|c| usize::try_from(*c).is_ok_and(|c| c < valid))
        .map(|code| GrpcStatusExpectation { code })
        .ok_or_else(|| {
            E::custom(format!(
                "invalid grpc_status '{shown}' (expected a code name such as NOT_FOUND or a number 0-16)"
            ))
        })
}

impl Visitor<'_> for GrpcStatusVisitor {
    type Value = GrpcStatusExpectation;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a gRPC status code name such as NOT_FOUND, or a number 0-16")
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        grpc_code(i32::try_from(v).ok(), &v.to_string())
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        grpc_code(i32::try_from(v).ok(), &v.to_string())
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        grpc_code(super::grpc::parse_grpc_code(v), v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn list(text: &str) -> Result<Vec<Assertion>, crate::error::LoadError> {
        crate::format::from_yaml(Path::new("a.yaml"), text)
    }

    #[test]
    fn status_forms() {
        let got = list("- status: 200\n- status: [200, 201]\n- status: \"2xx\"\n").unwrap();
        assert_eq!(
            got,
            [
                Assertion::Status(StatusExpectation::Code(200)),
                Assertion::Status(StatusExpectation::AnyOf(vec![200, 201])),
                Assertion::Status(StatusExpectation::Class(2)),
            ]
        );
        let err = list("- status: 999\n").unwrap_err();
        assert_eq!(err.message, "invalid status code 999 (expected 100-599)");
    }

    #[test]
    fn mcp_assertions_load() {
        let text = "\
- is_error: false
- content_text_contains: found
- structured: {path: $.count, equals: 3}
- output_schema: true
- tool_exists: search
";
        let all = list(text).unwrap();
        assert_eq!(all[0], Assertion::IsError(false));
        assert_eq!(all[1], Assertion::ContentTextContains("found".into()));
        assert!(matches!(&all[2], Assertion::Structured(s) if s.path == "$.count"));
        assert_eq!(all[3], Assertion::OutputSchema(true));
        assert_eq!(all[4], Assertion::ToolExists("search".into()));
    }

    #[test]
    fn every_assertion_type_loads() {
        let text = "\
- header: {name: x-request-id, exists: true}
- json: {path: $.items, length: 3}
- schema: {file: schemas/user.json}
- schema: {inline: {type: object}}
- body: {matches: 'order-[0-9]{6}'}
- latency: {total_lt: 200}
- no_errors: true
- errors_count: 0
- errors_count: {gte: 1}
- error_message_contains: forbidden
- error_code: FORBIDDEN
- data: {path: $.user.name, equals: Ada}
";
        let all = list(text).unwrap();
        assert_eq!(all.len(), 12);
        assert_eq!(all[6], Assertion::NoErrors(true));
        let count = |op, value| Assertion::ErrorsCount(CountExpectation { op, value });
        assert_eq!(all[7], count(CountOp::Equals, 0));
        assert_eq!(all[8], count(CountOp::Gte, 1));
        assert_eq!(all[10], Assertion::ErrorCode("FORBIDDEN".into()));
        assert!(matches!(&all[11], Assertion::Data(d) if d.path == "$.user.name"));
    }

    #[test]
    fn grpc_assertions_load() {
        let all = list(
            "- grpc_status: OK\n- grpc_status: not_found\n- grpc_status: 4\n- metadata: {name: x-served-by, equals: catalog}\n",
        )
        .unwrap();
        let code = |code| Assertion::GrpcStatus(GrpcStatusExpectation { code });
        assert_eq!(all[..3], [code(0), code(5), code(4)]);
        assert_eq!(GrpcStatusExpectation { code: 5 }.name(), "NOT_FOUND");
        assert!(matches!(&all[3], Assertion::Metadata(m) if m.name == "x-served-by"));
        let err = list("- grpc_status: NOPE\n").unwrap_err();
        assert!(
            err.message.starts_with("invalid grpc_status 'NOPE'"),
            "{err}"
        );
        assert!(list("- grpc_status: 17\n").is_err());
        assert!(list("- grpc_status: -1\n").is_err());
        let err = list("- metadata: {name: x}\n").unwrap_err();
        assert!(
            err.message
                .starts_with("metadata assertion needs exactly one of"),
            "{err}"
        );
    }

    #[test]
    fn errors_count_needs_one_comparison() {
        let err = list("- errors_count: {gt: 1, lt: 3}\n").unwrap_err();
        assert!(
            err.message
                .starts_with("errors_count assertion needs exactly one of: equals, gt"),
            "{err}"
        );
        assert!(list("- errors_count: -1\n").is_err());
        assert!(list("- errors_count: {between: 1}\n").is_err());
        assert!(CountOp::Lte.holds(2, 2) && !CountOp::Lt.holds(2, 2));
    }

    #[test]
    fn an_item_with_two_keys_is_explained() {
        let err = list("- status: 200\n  header: {name: x, exists: true}\n").unwrap_err();
        assert_eq!(err.line(), Some(1), "the item's line: {err}");
        assert!(
            err.message
                .starts_with("an assertion has exactly one key; found `header` after `status`"),
            "{err}"
        );
        let err = list("- {}\n").unwrap_err();
        assert!(err.message.starts_with("empty assertion"), "{err}");
    }

    #[test]
    fn errors_point_at_the_assertion() {
        let err = list("- status: 200\n- jsn: {path: $.a}\n").unwrap_err();
        assert_eq!((err.line(), err.column()), (Some(2), Some(3)));
        assert!(err.message.starts_with("unknown variant `jsn`"), "{err}");

        let err =
            list("- status: 200\n- json:\n    path: $.a\n    equals: 1\n    lt: 2\n").unwrap_err();
        assert_eq!(err.line(), Some(3));
        assert!(
            err.message
                .starts_with("json assertion needs exactly one of: equals"),
            "{err}"
        );
    }
}
