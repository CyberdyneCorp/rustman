//! The deterministic planner (design D4): a pure function of the model, the
//! graph, the seed and the settings that expands the probe families into a
//! `Vec<Probe>`. `explore plan` prints it without sending anything; the
//! executor walks it.

mod auth;
mod chains;
mod family;
mod inputs;
mod method;
mod negative;
mod render;
mod single;

use std::collections::BTreeSet;

use rustman_core::safety::SafetyPolicy;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use family::Family;
pub use inputs::segment;
pub use render::render;

use super::graph::Graph;
use super::operation_key;
use crate::openapi::model::{ApiModel, Operation};

/// The default number of fuzz cases per operation (`--fuzz-cases`).
pub const DEFAULT_FUZZ_CASES: usize = 50;

/// Whose credentials a step sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMode {
    /// The operation's auth, resolved with the active environment.
    Identity,
    /// No credentials at all.
    None,
    /// The operation's auth, resolved with the `--identity-b` environment.
    IdentityB,
}

/// What a step's response is checked for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum Check {
    /// Status documented, body valid against its schema, no 5xx.
    Conformance,
    /// Invalid input: 4xx expected.
    RejectsInput,
    /// Missing or invalid credentials: 401 or 403 expected.
    RequiresAuth,
    /// An undocumented method: 405 or 404 expected.
    MethodNotAllowed,
    /// A read after an update: the body shows every field of `expected`.
    ReflectsUpdate { expected: Value },
    /// A read after a delete: 404 or 410 expected.
    Gone,
    /// Identity B reading identity A's resource: refused expected.
    DeniedToOther,
}

/// A value taken from a step's response body into a variable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capture {
    pub variable: String,
    /// A JSON pointer into the response body.
    pub pointer: String,
}

/// One request of a probe. `path`, `query`, `headers` and body strings are
/// templates: `{{name}}` reads a variable (a capture of an earlier step, or
/// the environment), and literal `{{` is escaped as `\{{`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    /// `setup`, `probe`, `create`, `read`, `update`, `delete`, ...
    pub name: String,
    /// The documented operation, `None` for an undocumented method.
    pub operation: Option<String>,
    pub method: String,
    /// The path with its parameters filled in, relative to the target.
    pub path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub query: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
    pub auth: AuthMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<Check>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub captures: Vec<Capture>,
    /// The chain stops unless this step answers 2xx with every capture.
    #[serde(default)]
    pub require_success: bool,
    /// The step creates a resource (cleanup tracks it).
    #[serde(default)]
    pub creates: bool,
}

/// Whether a probe can run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "state", content = "reason")]
pub enum Disposition {
    Allowed,
    /// The safety policy refuses one of its steps.
    Blocked(String),
    /// It cannot be built, e.g. `no producer`.
    Skipped(String),
}

impl std::fmt::Display for Disposition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Allowed => f.write_str("allowed"),
            Self::Blocked(reason) => write!(f, "blocked: {reason}"),
            Self::Skipped(reason) => write!(f, "skipped: {reason}"),
        }
    }
}

/// One planned probe.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    /// `p0001`, `p0002`, ... in plan order.
    pub id: String,
    pub family: Family,
    /// `METHOD /path` of the operation probed.
    pub operation: String,
    /// The method of the probing request.
    pub method: String,
    /// What this probe varies, e.g. `missing required field 'name'`.
    pub variant: String,
    /// The expected outcome.
    pub expected: String,
    pub disposition: Disposition,
    /// Empty when skipped.
    pub steps: Vec<Step>,
}

/// A whole plan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub seed: u64,
    pub probes: Vec<Probe>,
    /// The enabled families that scan every response instead of planning
    /// probes of their own.
    pub scans: Vec<Family>,
}

/// What the planner needs beyond the model and the graph.
#[derive(Clone, Debug)]
pub struct PlanOptions<'a> {
    /// The selected families (`--probes`).
    pub families: BTreeSet<Family>,
    /// `--operations` globs over `operationId`, the path or `METHOD /path`;
    /// empty selects every operation.
    pub operations: Vec<String>,
    pub seed: u64,
    pub fuzz_cases: usize,
    /// Whether `--identity-b` names a second identity.
    pub identity_b: bool,
    /// Variables with a value in the active environment.
    pub known: BTreeSet<String>,
    /// Method gating for the `blocked` column.
    pub policy: &'a SafetyPolicy,
}

/// Plan probes for `model`.
pub fn plan(model: &ApiModel, graph: &Graph, options: &PlanOptions<'_>) -> Plan {
    let selected: Vec<&Operation> = model
        .operations
        .iter()
        .filter(|op| selects(&options.operations, op))
        .collect();
    let inputs = inputs::Inputs::new(model, graph, options);
    let mut probes = Vec::new();
    for family in &options.families {
        let family_probes = match family {
            Family::Conformance => single::conformance(&inputs, &selected),
            Family::NegativeInput => negative::probes(&inputs, &selected),
            Family::Auth => auth::probes(&inputs, &selected),
            Family::MethodNotAllowed => method::probes(&inputs, &selected),
            Family::Lifecycle => chains::lifecycle(&inputs, &selected),
            Family::Bola => chains::bola(&inputs, &selected),
            Family::Fuzz => single::fuzz(&inputs, &selected),
            Family::UndocumentedStatus | Family::ErrorLeakage | Family::SecurityHeaders => {
                Vec::new()
            }
        };
        probes.extend(family_probes);
    }
    for (i, probe) in probes.iter_mut().enumerate() {
        probe.id = format!("p{:04}", i + 1);
        if probe.disposition == Disposition::Allowed {
            probe.disposition = disposition(options.policy, &probe.steps);
        }
    }
    let scans = options
        .families
        .iter()
        .copied()
        .filter(|f| f.is_scan())
        .collect();
    Plan {
        seed: options.seed,
        probes,
        scans,
    }
}

/// `blocked: <reason>` for the first step the policy refuses.
fn disposition(policy: &SafetyPolicy, steps: &[Step]) -> Disposition {
    steps
        .iter()
        .find_map(|s| policy.check_method(&s.method).err())
        .map_or(Disposition::Allowed, |b| {
            Disposition::Blocked(b.reason().to_owned())
        })
}

/// Whether `op` matches one of `globs` (all when there are none).
fn selects(globs: &[String], op: &Operation) -> bool {
    if globs.is_empty() {
        return true;
    }
    let key = operation_key(op);
    let names = [
        op.operation_id.as_deref(),
        Some(op.path.as_str()),
        Some(key.as_str()),
    ];
    globs
        .iter()
        .any(|g| names.iter().flatten().any(|n| glob(g, n)))
}

/// `*` matches any run of characters, `?` one character.
pub fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// A literal made safe to embed in a template: `{{` becomes `\{{`.
pub fn escape(text: &str) -> String {
    text.replace("{{", "\\{{")
}

/// `value` with every string (keys included) escaped.
pub fn escape_json(value: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(escape(s)),
        Value::Array(items) => Value::Array(items.iter().map(escape_json).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (escape(k), escape_json(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob("GET /users*", "GET /users/{id}"));
        assert!(glob("*Order*", "createOrder"));
        assert!(glob("get?ser", "getUser"));
        assert!(!glob("/users", "/users/{id}"));
        assert!(glob("*", ""));
    }

    #[test]
    fn escaping_keeps_literals_literal() {
        assert_eq!(escape("a{{b}}"), "a\\{{b}}");
        let v = serde_json::json!({"{{k}}": ["{{v}}", 1]});
        assert_eq!(
            escape_json(&v),
            serde_json::json!({"\\{{k}}": ["\\{{v}}", 1]})
        );
    }
}
