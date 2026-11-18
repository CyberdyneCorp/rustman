//! docs/exploration.md lists every rule (with its severity) and every probe
//! family, and quotes real plan and graph output.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::load_bad_api;
use rustman_core::safety::{PolicyOptions, SafetyPolicy, SafetySettings};
use rustman_explore::explore::graph::Graph;
use rustman_explore::explore::plan::{Family, PlanOptions, plan, render};
use rustman_explore::explore::rules::RULES;

fn doc() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/exploration.md");
    std::fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

#[test]
fn every_rule_is_documented_with_its_severity() {
    let doc = doc();
    for rule in RULES {
        let row = format!("| `{}` | {} |", rule.id, rule.severity);
        assert!(doc.contains(&row), "missing: {row}");
    }
}

#[test]
fn every_rule_has_its_documented_regression() {
    let doc = doc();
    let start = doc.find("## Reproducing findings").unwrap();
    let end = doc[start..]
        .find("\n## ")
        .map_or(doc.len(), |e| start + e + 1);
    let section = &doc[start..end];
    for rule in RULES {
        assert!(
            section.contains(&format!("`{}`", rule.id)),
            "missing: {}",
            rule.id
        );
    }
}

#[test]
fn every_family_is_documented() {
    let doc = doc();
    for family in Family::ALL {
        assert!(
            doc.contains(&format!("| `{family}` |")),
            "missing: {family}"
        );
    }
}

#[tokio::test]
async fn the_documented_plan_and_graph_are_real_output() {
    let model = load_bad_api().await.model;
    let graph = Graph::build(&model, &BTreeMap::new());
    let options = PolicyOptions {
        target: "https://staging.example.com",
        ..PolicyOptions::default()
    };
    let policy = SafetyPolicy::new(&SafetySettings::default(), options).unwrap();
    let options = PlanOptions {
        families: Family::parse_list("conformance,auth,lifecycle").unwrap(),
        operations: vec!["*User*".into()],
        seed: 1234,
        fuzz_cases: 50,
        identity_b: false,
        known: BTreeSet::new(),
        policy: &policy,
    };
    let doc = doc();
    let rendered = render(&plan(&model, &graph, &options));
    assert!(doc.contains(&rendered), "{rendered}");
    let rendered = graph.render();
    for line in [
        "GET /users/{id}: id <- POST /users /id (heuristic)",
        "GET /orders/{orderId}: orderId <- POST /orders /id (heuristic)",
        "GET /reports/{reportId}: reportId <- no producer",
    ] {
        assert!(rendered.contains(line) && doc.contains(line), "{line}");
    }
}
