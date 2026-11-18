//! Exploration (the `api-exploration` spec): discover the resource graph,
//! plan probes, run them through a `GovernedSender` and detect findings.
//!
//! - [`graph`]: which operation produces each path parameter.
//! - [`plan`]: the deterministic planner and its probe families.
//! - [`fuzz`]: seeded fuzz values from schemas.
//! - [`rules`]: the static registry of finding rules.
//! - [`detect`]: the detectors that turn responses into findings.
//! - [`exec`]: running one probe under the safety policy.
//! - [`credentials`]: keeping credentials out of findings.

pub mod coverage;
pub mod credentials;
pub mod detect;
pub mod drive;
pub mod exec;
pub mod export;
pub mod fuzz;
pub mod graph;
pub mod journal;
pub mod plan;
pub mod report;
pub mod repro;
pub mod resources;
pub mod rules;
pub mod runner;
pub mod session;
pub mod setup;

use crate::openapi::model::Operation;

/// How an operation is named in plans, graphs and findings: `METHOD /path`.
pub fn operation_key(op: &Operation) -> String {
    format!("{} {}", op.method, op.path)
}
