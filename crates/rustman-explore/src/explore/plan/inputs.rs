//! The valid inputs of a request: parameter values, bodies, and the setup
//! steps that produce real path parameter values along the graph.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use rustman_core::safety::SafetyPolicy;
use serde_json::Value;

use super::{AuthMode, Capture, Check, PlanOptions, Step, escape, escape_json};
use crate::collection::names;
use crate::explore::graph::Graph;
use crate::explore::operation_key;
use crate::openapi::model::{ApiModel, Operation, Parameter, ParameterLocation};
use crate::synth::{Synthesizer, body_example, is_json_media_type};

/// Producers of producers are followed this deep.
const MAX_SETUP_DEPTH: usize = 3;

/// Path segment characters left as they are: the RFC 3986 unreserved set.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Header parameters set elsewhere (OpenAPI says to ignore them).
const IGNORED_HEADERS: [&str; 3] = ["accept", "content-type", "authorization"];

/// Why a probe that needs a real path parameter value is skipped.
pub const NO_PRODUCER: &str = "no producer";

pub struct Inputs<'a> {
    pub model: &'a ApiModel,
    pub graph: &'a Graph,
    pub options: &'a PlanOptions<'a>,
    pub synth: Synthesizer<'a>,
}

/// Path parameter values (templates) by name.
pub type PathValues = Vec<(String, String)>;

impl<'a> Inputs<'a> {
    pub fn new(model: &'a ApiModel, graph: &'a Graph, options: &'a PlanOptions<'a>) -> Self {
        Self {
            model,
            graph,
            options,
            synth: Synthesizer::new(&model.components),
        }
    }

    pub fn policy(&self) -> &SafetyPolicy {
        self.options.policy
    }

    pub fn allows(&self, method: &str) -> bool {
        self.policy().check_method(method).is_ok()
    }

    pub fn find(&self, key: &str) -> Option<&'a Operation> {
        self.model
            .operations
            .iter()
            .find(|op| operation_key(op) == key)
    }

    /// `{{name}}` when the environment has the parameter's variable.
    fn known(&self, param: &Parameter) -> Option<String> {
        let variable = names::variable(&param.name);
        self.options
            .known
            .contains(&variable)
            .then(|| format!("{{{{{variable}}}}}"))
    }

    /// A literal value for `param`: its example, else one synthesized.
    pub fn literal(&self, param: &Parameter) -> Value {
        if let Some(example) = &param.example {
            return example.clone();
        }
        param
            .schema
            .as_ref()
            .map_or(Value::String("1".into()), |s| self.synth.value(s))
    }

    /// Path values that need not name a real resource: the environment's,
    /// else literals.
    pub fn loose_path(&self, op: &Operation) -> PathValues {
        op.parameters_in(ParameterLocation::Path)
            .map(|p| {
                let value = self
                    .known(p)
                    .unwrap_or_else(|| segment(&text(&self.literal(p))));
                (p.name.clone(), value)
            })
            .collect()
    }

    /// Path values that name real resources: the environment's, else
    /// captured by setup steps from producers. `Err` when a parameter has
    /// neither.
    pub fn real_path(&self, op: &Operation) -> Result<(Vec<Step>, PathValues), String> {
        let mut setups = Vec::new();
        let values = self.real_path_at(op, 0, &mut setups)?;
        Ok((setups, values))
    }

    fn real_path_at(
        &self,
        op: &Operation,
        depth: usize,
        setups: &mut Vec<Step>,
    ) -> Result<PathValues, String> {
        let key = operation_key(op);
        let mut values = Vec::new();
        for param in op.parameters_in(ParameterLocation::Path) {
            if let Some(known) = self.known(param) {
                values.push((param.name.clone(), known));
                continue;
            }
            if depth >= MAX_SETUP_DEPTH {
                return Err(NO_PRODUCER.into());
            }
            let edges: Vec<_> = self.graph.producers(&key, &param.name).collect();
            let edge = edges
                .iter()
                .find(|e| {
                    self.find(&e.producer)
                        .is_some_and(|p| self.allows(&p.method))
                })
                .or(edges.first())
                .ok_or_else(|| NO_PRODUCER.to_owned())?;
            let producer = self
                .find(&edge.producer)
                .ok_or_else(|| NO_PRODUCER.to_owned())?;
            let variable = names::variable(&param.name);
            let capture = Capture {
                variable: variable.clone(),
                pointer: edge.pointer.clone(),
            };
            match setups
                .iter_mut()
                .find(|s| s.operation.as_deref() == Some(edge.producer.as_str()))
            {
                Some(step) => step.captures.push(capture),
                None => {
                    let path = self.real_path_at(producer, depth + 1, setups)?;
                    let mut step = self.request("setup", producer, &path, AuthMode::Identity, None);
                    step.captures.push(capture);
                    step.require_success = true;
                    setups.push(step);
                }
            }
            values.push((param.name.clone(), format!("{{{{{variable}}}}}")));
        }
        Ok(values)
    }

    /// Required query (or header) parameters: the environment's value, else
    /// a literal.
    pub fn params(&self, op: &Operation, location: ParameterLocation) -> Vec<(String, String)> {
        op.parameters_in(location)
            .filter(|p| p.required)
            .filter(|p| !IGNORED_HEADERS.contains(&p.name.to_ascii_lowercase().as_str()))
            .map(|p| {
                let value = self
                    .known(p)
                    .unwrap_or_else(|| escape(&text(&self.literal(p))));
                (p.name.clone(), value)
            })
            .collect()
    }

    /// The JSON schema of `op`'s request body.
    pub fn body_schema(op: &Operation) -> Option<&Value> {
        op.request_body
            .as_ref()?
            .content
            .iter()
            .find(|m| is_json_media_type(&m.media_type))?
            .schema
            .as_ref()
    }

    /// A valid JSON body for `op`: its example, else one synthesized.
    pub fn body(&self, op: &Operation) -> Option<Value> {
        let content = &op.request_body.as_ref()?.content;
        let example = body_example(content, &self.model.components)?;
        is_json_media_type(&example.media_type).then_some(example.value)
    }

    /// A valid request for `op`, with `path` values, as step `name`.
    pub fn request(
        &self,
        name: &str,
        op: &Operation,
        path: &PathValues,
        auth: AuthMode,
        check: Option<Check>,
    ) -> Step {
        let body = self.body(op);
        Step {
            name: name.to_owned(),
            operation: Some(operation_key(op)),
            method: op.method.clone(),
            path: fill_path(&op.path, path),
            query: self.params(op, ParameterLocation::Query),
            headers: self.params(op, ParameterLocation::Header),
            body: body.as_ref().map(escape_json),
            auth,
            check,
            captures: Vec::new(),
            require_success: false,
            creates: op.method == "POST",
        }
    }
}

/// `template` with each `{name}` replaced by its value.
pub fn fill_path(template: &str, values: &PathValues) -> String {
    let mut path = template.to_owned();
    for (name, value) in values {
        path = path.replace(&format!("{{{name}}}"), value);
    }
    path
}

/// A percent-encoded path segment (which can hold no `{{`). The dot
/// segments `.` and `..` are encoded too, so URL normalisation keeps them.
pub fn segment(value: &str) -> String {
    match value {
        "." => "%2E".into(),
        ".." => "%2E%2E".into(),
        _ => utf8_percent_encode(value, SEGMENT).to_string(),
    }
}

/// A JSON value as parameter text: strings as they are, others as JSON.
pub fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_filled_and_encoded() {
        let values = vec![("id".to_owned(), segment("a b/{{c}}"))];
        assert_eq!(
            fill_path("/x/{id}/y", &values),
            "/x/a%20b%2F%7B%7Bc%7D%7D/y"
        );
        assert_eq!(text(&serde_json::json!(5)), "5");
    }
}
