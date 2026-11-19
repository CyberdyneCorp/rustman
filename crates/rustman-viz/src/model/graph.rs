//! The API graph's data (spec "API graph"): one node per operation, one
//! link per data dependency (a value from one operation's response used as
//! a parameter of another), nodes coloured by their worst finding.
//! Selecting a node lists its findings.

use std::collections::BTreeMap;

use rustman_viz_protocol::{Finding, Severity};

use super::exploration::{rank, severity_name};

/// A node's colour: the worst severity of its findings, or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tint {
    Neutral,
    Severity(Severity),
}

impl Tint {
    /// RGB in 0..1.
    pub fn rgb(self) -> [f32; 3] {
        match self {
            Self::Neutral => [0.36, 0.40, 0.47],
            Self::Severity(Severity::Critical) => [0.85, 0.16, 0.20],
            Self::Severity(Severity::High) => [0.93, 0.45, 0.13],
            Self::Severity(Severity::Medium) => [0.91, 0.72, 0.16],
            Self::Severity(Severity::Low) => [0.25, 0.55, 0.88],
            Self::Severity(_) => [0.55, 0.58, 0.62],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub operation: String,
    pub tint: Tint,
    /// Layout position: column by dependency depth, row within it.
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub producer: usize,
    pub consumer: usize,
    /// The consumer's parameter, e.g. `id`.
    pub parameter: String,
}

#[derive(Debug, Clone, Default)]
pub struct GraphModel {
    pub title: String,
    pub nodes: Vec<Node>,
    pub links: Vec<Link>,
    pub findings: Vec<Finding>,
    pub selected: Option<usize>,
}

/// Horizontal and vertical distance between laid-out nodes.
const COLUMN: f32 = 300.0;
const ROW: f32 = 130.0;

impl GraphModel {
    /// The graph of `operations` (in document order), with `dependencies`
    /// as (producer, consumer, parameter) and the exploration's findings.
    pub fn new(
        title: impl Into<String>,
        operations: &[String],
        dependencies: &[(String, String, String)],
        findings: Vec<Finding>,
    ) -> Self {
        let mut names: Vec<String> = operations.to_vec();
        for (producer, consumer, _) in dependencies {
            for op in [producer, consumer] {
                if !names.contains(op) {
                    names.push(op.clone());
                }
            }
        }
        let index = |op: &str| names.iter().position(|n| n == op);
        let links: Vec<Link> = dependencies
            .iter()
            .filter_map(|(producer, consumer, parameter)| {
                Some(Link {
                    producer: index(producer)?,
                    consumer: index(consumer)?,
                    parameter: parameter.clone(),
                })
            })
            .filter(|l| l.producer != l.consumer)
            .collect();
        let depth = depths(names.len(), &links);
        let mut per_column: BTreeMap<usize, usize> = BTreeMap::new();
        let nodes = names
            .iter()
            .enumerate()
            .map(|(i, operation)| {
                let column = depth[i];
                let row = per_column.entry(column).or_insert(0);
                let node = Node {
                    operation: operation.clone(),
                    tint: worst(&findings, operation),
                    x: column as f32 * COLUMN,
                    y: *row as f32 * ROW,
                };
                *row += 1;
                node
            })
            .collect();
        Self {
            title: title.into(),
            nodes,
            links,
            findings,
            selected: None,
        }
    }

    /// The findings of the selected node, most severe first, as table
    /// cells.
    pub fn selection_cells(&self) -> (Vec<String>, Vec<Vec<String>>) {
        let columns = ["Severity", "Finding", "Detail"]
            .map(str::to_owned)
            .to_vec();
        let Some(node) = self.selected.and_then(|i| self.nodes.get(i)) else {
            return (columns, Vec::new());
        };
        let mut of_node: Vec<&Finding> = self
            .findings
            .iter()
            .filter(|f| f.operation == node.operation)
            .collect();
        of_node.sort_by_key(|f| rank(f.severity));
        let rows = of_node
            .into_iter()
            .map(|f| {
                vec![
                    severity_name(f.severity).to_owned(),
                    f.title.clone(),
                    f.detail.clone().unwrap_or_default(),
                ]
            })
            .collect();
        (columns, rows)
    }

    pub fn selection_heading(&self) -> String {
        match self.selected.and_then(|i| self.nodes.get(i)) {
            Some(node) => {
                let n = self
                    .findings
                    .iter()
                    .filter(|f| f.operation == node.operation)
                    .count();
                format!("{}: {n} findings", node.operation)
            }
            None => format!(
                "{} operations, {} dependencies. Select a node to list its findings.",
                self.nodes.len(),
                self.links.len()
            ),
        }
    }
}

fn worst(findings: &[Finding], operation: &str) -> Tint {
    findings
        .iter()
        .filter(|f| f.operation == operation)
        .min_by_key(|f| rank(f.severity))
        .map_or(Tint::Neutral, |f| Tint::Severity(f.severity))
}

/// The column of each node: the longest chain of producers before it
/// (cycles are cut after `n` rounds).
fn depths(n: usize, links: &[Link]) -> Vec<usize> {
    let mut depth = vec![0; n];
    for _ in 0..n {
        let mut changed = false;
        for link in links {
            let wanted = depth[link.producer] + 1;
            if depth[link.consumer] < wanted && wanted < n {
                depth[link.consumer] = wanted;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    depth
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deps() -> Vec<(String, String, String)> {
        vec![
            ("POST /users".into(), "GET /users/{id}".into(), "id".into()),
            (
                "POST /users".into(),
                "DELETE /users/{id}".into(),
                "id".into(),
            ),
            (
                "GET /users/{id}".into(),
                "GET /users/{id}/orders".into(),
                "id".into(),
            ),
        ]
    }

    #[test]
    fn a_dependency_links_producer_to_consumer() {
        let ops = [
            "POST /users",
            "GET /users/{id}",
            "DELETE /users/{id}",
            "GET /health",
        ]
        .map(str::to_owned);
        let finding = Finding {
            severity: Severity::High,
            operation: "POST /users".into(),
            title: "500 on empty body".into(),
            detail: None,
        };
        let mut graph = GraphModel::new("api", &ops, &deps(), vec![finding]);
        // An operation only a dependency names is added.
        assert_eq!(graph.nodes.len(), 5);
        let link = &graph.links[0];
        assert_eq!(graph.nodes[link.producer].operation, "POST /users");
        assert_eq!(graph.nodes[link.consumer].operation, "GET /users/{id}");
        assert_eq!(link.parameter, "id");
        // Columns follow the chain of producers.
        assert_eq!(graph.nodes[0].x, 0.0);
        assert_eq!(graph.nodes[1].x, COLUMN);
        assert_eq!(graph.nodes[4].x, 2.0 * COLUMN);
        assert_eq!(graph.nodes[0].tint, Tint::Severity(Severity::High));
        assert_eq!(graph.nodes[3].tint, Tint::Neutral);
        assert_eq!(graph.selection_cells().1.len(), 0);
        graph.selected = Some(0);
        assert_eq!(
            graph.selection_cells().1,
            [["high", "500 on empty body", ""]]
        );
        assert_eq!(graph.selection_heading(), "POST /users: 1 findings");
    }

    #[test]
    fn cycles_do_not_loop() {
        let deps = vec![
            ("A".into(), "B".into(), "x".into()),
            ("B".into(), "A".into(), "y".into()),
        ];
        let graph = GraphModel::new("t", &[], &deps, Vec::new());
        assert_eq!(graph.nodes.len(), 2);
        assert!(graph.nodes.iter().all(|n| n.x < 2.0 * COLUMN));
    }
}
