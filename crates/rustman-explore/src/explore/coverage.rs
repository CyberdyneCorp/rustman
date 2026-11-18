//! The coverage matrix: operations by documented statuses, each cell
//! observed, not observed, or blocked (nothing was sent for the operation
//! because the safety policy refused its probes).

use std::collections::BTreeSet;

use super::detect::documented_response;
use super::exec::{Outcome, Status};
use super::operation_key;
use super::plan::Plan;
use crate::openapi::model::ApiModel;

pub use rustman_report::exploration::{Cell, Coverage, Ratio, Row, StatusCell};

/// The coverage of `model` by `outcomes` of `plan`.
pub fn coverage(model: &ApiModel, plan: &Plan, outcomes: &[&Outcome]) -> Coverage {
    let observed: BTreeSet<(&str, u16)> = outcomes
        .iter()
        .flat_map(|o| &o.sent)
        .filter_map(|s| Some((s.operation.as_deref()?, s.status)))
        .collect();
    let blocked: BTreeSet<&str> = outcomes
        .iter()
        .filter(|o| matches!(o.status, Status::Blocked(_)))
        .filter_map(|o| plan.probes.iter().find(|p| p.id == o.probe))
        .map(|p| p.operation.as_str())
        .collect();
    let mut matrix = Vec::new();
    let (mut exercised, mut seen, mut documented) = (0, 0, 0);
    for op in &model.operations {
        let key = operation_key(op);
        let statuses: Vec<u16> = observed
            .iter()
            .filter(|(k, _)| *k == key)
            .map(|(_, s)| *s)
            .collect();
        exercised += usize::from(!statuses.is_empty());
        let unseen = if statuses.is_empty() && blocked.contains(key.as_str()) {
            Cell::Blocked
        } else {
            Cell::NotObserved
        };
        let cells: Vec<StatusCell> = op
            .responses
            .iter()
            .map(|r| {
                let hit = statuses
                    .iter()
                    .any(|s| documented_response(op, *s).is_some_and(|d| d.status == r.status));
                StatusCell {
                    status: r.status.clone(),
                    state: if hit { Cell::Observed } else { unseen },
                }
            })
            .collect();
        documented += cells.len();
        seen += cells.iter().filter(|c| c.state == Cell::Observed).count();
        matrix.push(Row {
            operation: key,
            statuses: cells,
        });
    }
    Coverage {
        operations: Ratio::new(exercised, model.operations.len()),
        statuses: Ratio::new(seen, documented),
        matrix,
    }
}
