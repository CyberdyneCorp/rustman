//! Probes made of chained steps: CRUD lifecycles along the graph, and BOLA
//! (one identity's resource read by another).

use serde_json::{Map, Value};

use super::inputs::{Inputs, PathValues};
use super::single::probe;
use super::{AuthMode, Capture, Check, Family, Probe, Step, escape_json};
use crate::collection::names;
use crate::explore::operation_key;
use crate::openapi::model::{Operation, ParameterLocation};

/// Why BOLA probes are skipped without `--identity-b`.
pub const NO_IDENTITY_B: &str = "no second identity (--identity-b)";

/// The text a lifecycle update writes into string fields.
const UPDATED: &str = "updated";

/// create -> read -> update -> read -> delete -> read for each item
/// operation whose path parameter a POST on its parent path produces. Steps
/// the policy refuses past the create are left out; a refused create
/// blocks the whole probe.
pub fn lifecycle(inputs: &Inputs<'_>, ops: &[&Operation]) -> Vec<Probe> {
    ops.iter()
        .filter(|op| op.method == "GET")
        .filter_map(|read| {
            let (create, param, pointer) = creator(inputs, read)?;
            let steps = inputs.real_path(create).map(|(setups, path)| {
                chain(inputs, read, create, setups, &path, (&param, &pointer))
            });
            let names = steps.as_ref().map(|s| step_names(s)).unwrap_or_default();
            let expected = expected(steps.as_deref().unwrap_or_default());
            Some(probe(Family::Lifecycle, read, &names, &expected, steps))
        })
        .collect()
}

/// The POST on `read`'s parent path that produces its last path parameter,
/// with that parameter and the pointer of its value.
fn creator<'a>(inputs: &Inputs<'a>, read: &Operation) -> Option<(&'a Operation, String, String)> {
    let (parent, last) = read.path.rsplit_once('/')?;
    let param = last.strip_prefix('{')?.strip_suffix('}')?;
    let create = inputs
        .model
        .operations
        .iter()
        .find(|o| o.method == "POST" && o.path == parent)?;
    let (key, consumer) = (operation_key(create), operation_key(read));
    let edge = inputs
        .graph
        .producers(&consumer, param)
        .find(|e| e.producer == key)?;
    Some((create, param.to_owned(), edge.pointer.clone()))
}

fn chain(
    inputs: &Inputs<'_>,
    read: &Operation,
    create: &Operation,
    mut steps: Vec<Step>,
    parent: &PathValues,
    (param, pointer): (&str, &str),
) -> Vec<Step> {
    let variable = names::variable(param);
    let mut create_step = inputs.request(
        "create",
        create,
        parent,
        AuthMode::Identity,
        Some(Check::Conformance),
    );
    create_step.captures.push(Capture {
        variable: variable.clone(),
        pointer: pointer.to_owned(),
    });
    create_step.require_success = true;
    steps.push(create_step);
    let mut item = parent.clone();
    item.push((param.to_owned(), format!("{{{{{variable}}}}}")));
    let step = |name: &str, op: &Operation, check: Check| {
        let mut s = inputs.request(name, op, &item, AuthMode::Identity, Some(check));
        s.require_success = !matches!(s.check, Some(Check::Gone | Check::ReflectsUpdate { .. }));
        s
    };
    steps.push(step("read", read, Check::Conformance));
    // While the create runs, steps the policy refuses are left out; when
    // the create itself is refused, the plan shows the whole chain.
    let trim = inputs.allows(&create.method);
    let sibling =
        |method: &str| {
            inputs.model.operations.iter().find(|o| {
                o.path == read.path && o.method == method && (!trim || inputs.allows(method))
            })
        };
    if let Some(update) = sibling("PUT").or_else(|| sibling("PATCH")) {
        let body = update_body(inputs, update);
        let mut update_step = step("update", update, Check::Conformance);
        update_step.body = body.as_ref().map(escape_json);
        steps.push(update_step);
        let expected = Value::Object(scalars(body.as_ref()));
        steps.push(step("read", read, Check::ReflectsUpdate { expected }));
    }
    if let Some(delete) = sibling("DELETE") {
        steps.push(step("delete", delete, Check::Conformance));
        steps.push(step("read", read, Check::Gone));
    }
    steps
}

/// The update's valid body with every free-form string field set to
/// `updated` (fitted to its length bounds), so a read can tell it apart.
fn update_body(inputs: &Inputs<'_>, update: &Operation) -> Option<Value> {
    let mut body = inputs.body(update)?;
    let schema = Inputs::body_schema(update)?;
    let object = inputs.synth.object_view(schema)?;
    let properties = object.get("properties").and_then(Value::as_object)?;
    let fields = body.as_object_mut()?;
    for (name, value) in fields.iter_mut() {
        let Some(property) = properties
            .get(name)
            .and_then(|p| inputs.synth.object_view(p))
        else {
            continue;
        };
        let constrained = ["format", "enum", "const", "pattern"]
            .iter()
            .any(|k| property.contains_key(*k));
        if value.is_string() && !constrained {
            *value = Value::String(fit(&property));
        }
    }
    Some(body)
}

fn fit(property: &Map<String, Value>) -> String {
    let length = |key: &str| {
        property
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
    };
    let mut text = UPDATED.to_owned();
    if let Some(min) = length("minLength") {
        while text.len() < min {
            text.push('x');
        }
    }
    if let Some(max) = length("maxLength") {
        text.truncate(max);
    }
    text
}

/// The scalar top-level fields of `body`: what a read must reflect.
fn scalars(body: Option<&Value>) -> Map<String, Value> {
    body.and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter(|(_, v)| !v.is_object() && !v.is_array())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn step_names(steps: &[Step]) -> String {
    let start = steps.iter().position(|s| s.name == "create").unwrap_or(0);
    let names: Vec<&str> = steps[start..].iter().map(|s| s.name.as_str()).collect();
    names.join(", ")
}

fn expected(steps: &[Step]) -> String {
    let mut parts = Vec::new();
    if steps
        .iter()
        .any(|s| matches!(s.check, Some(Check::ReflectsUpdate { .. })))
    {
        parts.push("update reflected");
    }
    if steps.iter().any(|s| s.check == Some(Check::Gone)) {
        parts.push("404 or 410 after delete");
    }
    if parts.is_empty() {
        parts.push("documented status and schema");
    }
    parts.join(", ")
}

/// For each read by id that requires credentials: identity A reads a
/// resource (produced along the graph), then identity B reads it by id.
pub fn bola(inputs: &Inputs<'_>, ops: &[&Operation]) -> Vec<Probe> {
    let expected = "identity B refused (401, 403 or 404)";
    ops.iter()
        .filter(|op| {
            op.method == "GET" && op.parameters_in(ParameterLocation::Path).next().is_some()
        })
        .filter(|op| super::auth::requires_credentials(inputs, op))
        .map(|op| {
            let steps = if inputs.options.identity_b {
                inputs.real_path(op).map(|(mut steps, path)| {
                    let mut read_a = inputs.request(
                        "read",
                        op,
                        &path,
                        AuthMode::Identity,
                        Some(Check::Conformance),
                    );
                    read_a.require_success = true;
                    steps.push(read_a);
                    steps.push(inputs.request(
                        "read-as-b",
                        op,
                        &path,
                        AuthMode::IdentityB,
                        Some(Check::DeniedToOther),
                    ));
                    steps
                })
            } else {
                Err(NO_IDENTITY_B.to_owned())
            };
            probe(Family::Bola, op, "read by identity B", expected, steps)
        })
        .collect()
}
