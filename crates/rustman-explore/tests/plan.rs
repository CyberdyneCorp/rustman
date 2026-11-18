//! Task 3.2: the deterministic planner and the probe registry (`--probes`,
//! `--operations`, `--seed`, `--fuzz-cases`), plus the method gating and
//! skipped reasons the plan records.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::{load, load_bad_api};
use rustman_core::safety::{Permission, PolicyOptions, SafetyPolicy, SafetySettings};
use rustman_explore::ApiModel;
use rustman_explore::explore::graph::Graph;
use rustman_explore::explore::plan::{Disposition, Family, Plan, PlanOptions, Probe, plan, render};

fn policy(permission: Permission) -> SafetyPolicy {
    let options = PolicyOptions {
        target: "http://127.0.0.1:8080",
        permission,
        ..PolicyOptions::default()
    };
    SafetyPolicy::new(&SafetySettings::default(), options).unwrap()
}

struct Planner {
    families: BTreeSet<Family>,
    operations: Vec<String>,
    seed: u64,
    fuzz_cases: usize,
    permission: Permission,
    identity_b: bool,
    known: BTreeSet<String>,
}

impl Default for Planner {
    fn default() -> Self {
        Self {
            families: Family::defaults(),
            operations: Vec::new(),
            seed: 1234,
            fuzz_cases: 4,
            permission: Permission::ReadOnly,
            identity_b: false,
            known: BTreeSet::new(),
        }
    }
}

impl Planner {
    fn plan(&self, model: &ApiModel) -> Plan {
        let graph = Graph::build(model, &BTreeMap::new());
        let policy = policy(self.permission);
        let options = PlanOptions {
            families: self.families.clone(),
            operations: self.operations.clone(),
            seed: self.seed,
            fuzz_cases: self.fuzz_cases,
            identity_b: self.identity_b,
            known: self.known.clone(),
            policy: &policy,
        };
        plan(model, &graph, &options)
    }
}

fn find<'a>(plan: &'a Plan, family: Family, operation: &str) -> Vec<&'a Probe> {
    plan.probes
        .iter()
        .filter(|p| p.family == family && p.operation == operation)
        .collect()
}

#[tokio::test]
async fn the_same_seed_gives_an_identical_plan() {
    let model = load_bad_api().await.model;
    let planner = Planner {
        fuzz_cases: 50,
        ..Planner::default()
    };
    let first = planner.plan(&model);
    let second = planner.plan(&model);
    assert_eq!(first, second);
    assert_eq!(
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&second).unwrap()
    );
    assert!(first.probes.iter().any(|p| p.family == Family::Fuzz));
    let other = Planner {
        seed: 99,
        fuzz_cases: 50,
        ..Planner::default()
    }
    .plan(&model);
    assert_ne!(
        first.probes, other.probes,
        "the seed changes the fuzz cases"
    );
}

#[tokio::test]
async fn the_plan_output() {
    let model = load_bad_api().await.model;
    let plan = Planner::default().plan(&model);
    insta::assert_snapshot!("bad-api-plan", render(&plan));
}

#[tokio::test]
async fn probes_selects_families() {
    let model = load_bad_api().await.model;
    let plan = Planner {
        families: Family::parse_list("conformance,auth").unwrap(),
        ..Planner::default()
    }
    .plan(&model);
    let families: BTreeSet<Family> = plan.probes.iter().map(|p| p.family).collect();
    assert_eq!(families, [Family::Conformance, Family::Auth].into());
    assert!(plan.scans.is_empty());
}

#[tokio::test]
async fn operations_filters_by_glob() {
    let model = load_bad_api().await.model;
    let plan = Planner {
        operations: vec!["*Order".into(), "GET /status".into()],
        ..Planner::default()
    }
    .plan(&model);
    let operations: BTreeSet<&str> = plan
        .probes
        .iter()
        .filter(|p| p.family != Family::MethodNotAllowed)
        .map(|p| p.operation.as_str())
        .collect();
    assert_eq!(
        operations,
        ["GET /orders/{orderId}", "GET /status", "POST /orders"].into()
    );
}

#[tokio::test]
async fn fuzz_cases_caps_the_cases_per_operation() {
    let model = load_bad_api().await.model;
    for cases in [1, 6] {
        let plan = Planner {
            families: [Family::Fuzz].into(),
            fuzz_cases: cases,
            ..Planner::default()
        }
        .plan(&model);
        let search = find(&plan, Family::Fuzz, "GET /search");
        assert!(
            !search.is_empty() && search.len() <= cases,
            "{cases}: {}",
            search.len()
        );
        assert!(find(&plan, Family::Fuzz, "POST /users").len() <= cases);
    }
}

#[tokio::test]
async fn the_default_plan_is_read_only() {
    let model = load_bad_api().await.model;
    let plan = Planner::default().plan(&model);
    for probe in &plan.probes {
        let mutating = probe
            .steps
            .iter()
            .any(|s| s.method != "GET" && s.method != "HEAD" && s.method != "OPTIONS");
        if mutating {
            assert!(
                matches!(&probe.disposition, Disposition::Blocked(r) if r == "mutations not allowed" || r == "destructive not allowed"),
                "{probe:?}"
            );
        }
    }
    let create = &find(&plan, Family::Conformance, "POST /users")[0];
    assert_eq!(
        create.disposition.to_string(),
        "blocked: mutations not allowed"
    );
    let delete = &find(&plan, Family::Conformance, "DELETE /users/{id}")[0];
    assert_eq!(
        delete.disposition.to_string(),
        "blocked: destructive not allowed"
    );
}

#[tokio::test]
async fn mutations_without_destructive_block_only_deletes() {
    let model = load_bad_api().await.model;
    let plan = Planner {
        permission: Permission::Mutations,
        ..Planner::default()
    }
    .plan(&model);
    assert_eq!(
        find(&plan, Family::Conformance, "POST /users")[0].disposition,
        Disposition::Allowed
    );
    let delete = &find(&plan, Family::Conformance, "DELETE /users/{id}")[0];
    assert_eq!(
        delete.disposition.to_string(),
        "blocked: destructive not allowed"
    );
    let lifecycle = &find(&plan, Family::Lifecycle, "GET /users/{id}")[0];
    assert_eq!(lifecycle.disposition, Disposition::Allowed);
    assert_eq!(lifecycle.variant, "create, read, update, read");
    let undocumented = &find(&plan, Family::MethodNotAllowed, "PUT /users")[0];
    assert_eq!(undocumented.disposition, Disposition::Allowed);
}

#[tokio::test]
async fn an_unresolvable_parameter_skips_the_probes_that_need_it() {
    let model = load("explore/crud.yaml").await.model;
    let plan = Planner::default().plan(&model);
    let order = &find(&plan, Family::Conformance, "GET /orders/{orderId}")[0];
    assert_eq!(order.disposition.to_string(), "skipped: no producer");
    assert!(order.steps.is_empty());
    let known = Planner {
        known: ["orderId".to_owned()].into(),
        ..Planner::default()
    }
    .plan(&model);
    let order = &find(&known, Family::Conformance, "GET /orders/{orderId}")[0];
    assert_eq!(order.disposition, Disposition::Allowed);
    assert_eq!(order.steps[0].path, "/orders/{{orderId}}");
}

#[tokio::test]
async fn read_only_conformance_reads_ids_from_a_list() {
    let model = load_bad_api().await.model;
    let plan = Planner::default().plan(&model);
    let get = &find(&plan, Family::Conformance, "GET /users/{id}")[0];
    assert_eq!(get.disposition, Disposition::Allowed);
    let steps: Vec<(&str, &str, &str)> = get
        .steps
        .iter()
        .map(|s| (s.name.as_str(), s.method.as_str(), s.path.as_str()))
        .collect();
    assert_eq!(
        steps,
        [
            ("setup", "GET", "/users"),
            ("probe", "GET", "/users/{{id}}")
        ]
    );
    assert_eq!(get.steps[0].captures[0].pointer, "/0/id");
}

#[tokio::test]
async fn bola_needs_a_second_identity() {
    let model = load_bad_api().await.model;
    let families: BTreeSet<Family> = [Family::Bola].into();
    let plan = Planner {
        families: families.clone(),
        ..Planner::default()
    }
    .plan(&model);
    let order = &find(&plan, Family::Bola, "GET /orders/{orderId}")[0];
    assert_eq!(
        order.disposition.to_string(),
        "skipped: no second identity (--identity-b)"
    );
    let plan = Planner {
        families,
        identity_b: true,
        permission: Permission::Mutations,
        ..Planner::default()
    }
    .plan(&model);
    let order = &find(&plan, Family::Bola, "GET /orders/{orderId}")[0];
    let names: Vec<&str> = order.steps.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["setup", "read", "read-as-b"]);
    assert_eq!(order.disposition, Disposition::Allowed);
}

#[tokio::test]
async fn negative_variants_are_really_invalid() {
    let model = load_bad_api().await.model;
    let plan = Planner {
        families: [Family::NegativeInput].into(),
        ..Planner::default()
    }
    .plan(&model);
    let variants: Vec<&str> = find(&plan, Family::NegativeInput, "POST /users")
        .iter()
        .map(|p| p.variant.as_str())
        .collect();
    assert!(
        variants.contains(&"missing required field 'name'"),
        "{variants:?}"
    );
    assert!(
        variants.contains(&"wrong type for field 'age'"),
        "{variants:?}"
    );
    assert!(
        variants.contains(&"above maximum for field 'age'"),
        "{variants:?}"
    );
    assert!(
        variants.contains(&"longer than maxLength for field 'name'"),
        "{variants:?}"
    );
    // `email` has only a format, which JSON Schema does not assert.
    assert!(
        !variants
            .iter()
            .any(|v| v.contains("'email'") && !v.contains("wrong type")),
        "{variants:?}"
    );
    let search: Vec<&str> = find(&plan, Family::NegativeInput, "GET /search")
        .iter()
        .map(|p| p.variant.as_str())
        .collect();
    assert_eq!(
        search,
        [
            "missing required query parameter 'q'",
            "shorter than minLength for query parameter 'q'"
        ]
    );
}

#[tokio::test]
async fn a_fan_out_schema_plans_in_bounded_time() {
    // Nested required `$ref`s under `allOf` make the number of schema
    // violations grow exponentially; the planner only asks whether a
    // variant is valid, so it must finish quickly.
    let model = load("explore/fan-out.json").await.model;
    let started = std::time::Instant::now();
    let plan = Planner {
        permission: Permission::Destructive,
        ..Planner::default()
    }
    .plan(&model);
    assert!(
        plan.probes
            .iter()
            .any(|p| p.family == Family::NegativeInput),
        "{:?}",
        plan.probes.iter().map(|p| p.family).collect::<Vec<_>>()
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(30));
}
