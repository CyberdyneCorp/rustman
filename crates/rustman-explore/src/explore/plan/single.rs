//! Conformance and fuzz probes: valid requests, one per operation or one per
//! fuzz case, after the setup steps that produce their path parameters.

use serde_json::Value;

use super::inputs::{Inputs, PathValues, text};
use super::{AuthMode, Check, Disposition, Family, Probe, Step, escape, escape_json};
use crate::explore::fuzz::{Fuzzer, Mode, runner};
use crate::explore::operation_key;
use crate::openapi::model::{Operation, ParameterLocation};

/// A probe of `family` on `op`, skipped when its steps could not be built.
pub fn probe(
    family: Family,
    op: &Operation,
    variant: &str,
    expected: &str,
    steps: Result<Vec<Step>, String>,
) -> Probe {
    let (disposition, steps) = match steps {
        Ok(steps) => (Disposition::Allowed, steps),
        Err(reason) => (Disposition::Skipped(reason), Vec::new()),
    };
    let method = steps
        .last()
        .map_or_else(|| op.method.clone(), |s| s.method.clone());
    Probe {
        id: String::new(),
        family,
        operation: operation_key(op),
        method,
        variant: variant.to_owned(),
        expected: expected.to_owned(),
        disposition,
        steps,
    }
}

/// One valid request per operation.
pub fn conformance(inputs: &Inputs<'_>, ops: &[&Operation]) -> Vec<Probe> {
    ops.iter()
        .map(|op| {
            let steps = inputs.real_path(op).map(|(mut steps, path)| {
                let check = Some(Check::Conformance);
                steps.push(inputs.request("probe", op, &path, AuthMode::Identity, check));
                steps
            });
            probe(
                Family::Conformance,
                op,
                "valid request",
                "documented status and schema",
                steps,
            )
        })
        .collect()
}

/// Up to `fuzz_cases` seeded requests per operation that has a JSON body or
/// query parameters, alternating valid and boundary values. Duplicate cases
/// are dropped.
pub fn fuzz(inputs: &Inputs<'_>, ops: &[&Operation]) -> Vec<Probe> {
    let mut probes = Vec::new();
    for op in ops {
        let has_query = op
            .parameters_in(ParameterLocation::Query)
            .any(|p| p.schema.is_some());
        if Inputs::body_schema(op).is_none() && !has_query {
            continue;
        }
        match inputs.real_path(op) {
            Ok(base) => probes.extend(fuzz_cases(inputs, op, &base)),
            Err(reason) => probes.push(probe(Family::Fuzz, op, "fuzz", FUZZ_EXPECTED, Err(reason))),
        }
    }
    probes
}

/// The fuzzed body and query of one case.
type Case = (Option<Value>, Vec<(String, String)>);

fn fuzz_cases(inputs: &Inputs<'_>, op: &Operation, base: &(Vec<Step>, PathValues)) -> Vec<Probe> {
    let fuzzer = Fuzzer::new(&inputs.model.components);
    let mut seen: Vec<Case> = Vec::new();
    let mut probes = Vec::new();
    for case in 0..inputs.options.fuzz_cases {
        let mode = if case % 2 == 0 {
            Mode::Valid
        } else {
            Mode::Boundary
        };
        let drawn = draw(&fuzzer, op, mode, inputs.options.seed, case);
        if seen.contains(&drawn) {
            continue;
        }
        let (mut steps, path) = base.clone();
        let check = Some(Check::Conformance);
        let mut step = inputs.request("probe", op, &path, AuthMode::Identity, check);
        step.body = drawn.0.as_ref().map(escape_json);
        step.query = drawn
            .1
            .iter()
            .map(|(n, v)| (n.clone(), escape(v)))
            .collect();
        steps.push(step);
        seen.push(drawn);
        let kind = if mode == Mode::Valid {
            "valid"
        } else {
            "boundary"
        };
        let variant = format!("case {} ({kind})", case + 1);
        probes.push(probe(Family::Fuzz, op, &variant, FUZZ_EXPECTED, Ok(steps)));
    }
    probes
}

/// The body and query parameters of case `case`, drawn from a generator
/// seeded with the plan seed, the operation and the case number.
fn draw(fuzzer: &Fuzzer<'_>, op: &Operation, mode: Mode, seed: u64, case: usize) -> Case {
    let mut rng = runner(seed, &format!("{}#{case}", operation_key(op)));
    let body = Inputs::body_schema(op).map(|s| fuzzer.sample(s, mode, &mut rng));
    let query = op
        .parameters_in(ParameterLocation::Query)
        .filter_map(|p| {
            let value = fuzzer.sample(p.schema.as_ref()?, mode, &mut rng);
            Some((p.name.clone(), text(&value)))
        })
        .collect();
    (body, query)
}

const FUZZ_EXPECTED: &str = "no 5xx, documented status and schema";
