//! Method-not-allowed probes: one undocumented method per documented path,
//! from GET/HEAD/OPTIONS when the policy is read-only.

use super::inputs::Inputs;
use super::{AuthMode, Check, Disposition, Family, Probe};
use crate::openapi::model::Operation;

/// Candidate methods, most telling first. `HEAD` is left out where `GET` is
/// documented, since HTTP defines it through `GET`.
const CANDIDATES: [&str; 7] = ["PUT", "PATCH", "POST", "DELETE", "GET", "HEAD", "OPTIONS"];

pub fn probes(inputs: &Inputs<'_>, ops: &[&Operation]) -> Vec<Probe> {
    let mut paths: Vec<&str> = Vec::new();
    for op in ops {
        if !paths.contains(&op.path.as_str()) {
            paths.push(&op.path);
        }
    }
    paths
        .into_iter()
        .filter_map(|path| probe(inputs, ops, path))
        .collect()
}

fn probe(inputs: &Inputs<'_>, ops: &[&Operation], path: &str) -> Option<Probe> {
    let documented: Vec<&str> = inputs
        .model
        .operations
        .iter()
        .filter(|o| o.path == path)
        .map(|o| o.method.as_str())
        .collect();
    let method = CANDIDATES.into_iter().find(|m| {
        !documented.contains(m)
            && inputs.allows(m)
            && !(*m == "HEAD" && documented.contains(&"GET"))
    })?;
    let first = ops.iter().find(|o| o.path == path)?;
    let check = Some(Check::MethodNotAllowed);
    let mut step = inputs.request(
        "probe",
        first,
        &inputs.loose_path(first),
        AuthMode::Identity,
        check,
    );
    step.operation = None;
    step.method = method.to_owned();
    step.body = None;
    step.creates = false;
    Some(Probe {
        id: String::new(),
        family: Family::MethodNotAllowed,
        operation: format!("{method} {path}"),
        method: method.to_owned(),
        variant: format!("undocumented method {method}"),
        expected: "405 or 404".into(),
        disposition: Disposition::Allowed,
        steps: vec![step],
    })
}
