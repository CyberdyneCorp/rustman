//! Task 3.8 on every platform: an exploration of 900 probes cancelled (as
//! Ctrl+C does) after 400, then resumed from its journal, sends each probe
//! once and reports all 900. Also the coverage summary of a finished run.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::time::Duration;

use common::load_bad_api;
use rustman_core::model::Variables;
use rustman_core::safety::{Governor, Limits, PolicyOptions, SafetyPolicy, SafetySettings};
use rustman_core::vars::NoSecrets;
use rustman_explore::ApiModel;
use rustman_explore::explore::coverage::Cell;
use rustman_explore::explore::drive::{Drive, Finished, drive};
use rustman_explore::explore::exec::{Context, Executor, Identity};
use rustman_explore::explore::graph::Graph;
use rustman_explore::explore::plan::{Family, Plan, PlanOptions, Probe, plan};
use rustman_explore::explore::rules::Severity;
use rustman_explore::explore::session::{Phase, Progress, RunState, Session, Settings, State};
use rustman_http::{GovernedSender, SendOptions};
use rustman_test_support::bad_api::BadApi;
use tokio_util::sync::CancellationToken;

fn policy(target: &str) -> SafetyPolicy {
    let options = PolicyOptions {
        target,
        ..PolicyOptions::default()
    };
    SafetyPolicy::new(&SafetySettings::default(), options).unwrap()
}

fn conformance(model: &ApiModel, graph: &Graph, policy: &SafetyPolicy) -> Plan {
    plan(
        model,
        graph,
        &PlanOptions {
            families: BTreeSet::from([Family::Conformance]),
            operations: Vec::new(),
            seed: 1,
            fuzz_cases: 0,
            identity_b: false,
            known: BTreeSet::new(),
            policy,
        },
    )
}

/// 900 copies of the conformance probe of `GET /status`.
fn nine_hundred(conformance: &Plan) -> Plan {
    let status = conformance
        .probes
        .iter()
        .find(|p| p.operation == "GET /status")
        .unwrap();
    let probes = (1..=900)
        .map(|i| Probe {
            id: format!("p{i:04}"),
            ..status.clone()
        })
        .collect();
    Plan {
        seed: 1,
        probes,
        scans: Vec::new(),
    }
}

fn state(target: &str) -> State {
    State {
        id: "exp-20261118-abcdef".into(),
        phase: Phase::Plan,
        state: RunState::Running,
        started_at: "2026-11-18T10:00:00Z".into(),
        started_unix_ms: 1,
        updated_at: "2026-11-18T10:00:00Z".into(),
        executed_ms: 0,
        spec: "bad-api.yaml".into(),
        document_sha256: "0".into(),
        target: target.into(),
        environment: None,
        identity_b: None,
        settings: Settings {
            probes: vec![Family::Conformance],
            operations: Vec::new(),
            seed: 1,
            fuzz_cases: 0,
            allow_mutations: false,
            allow_destructive: false,
            allow_hosts: Vec::new(),
            production: None,
            allow_remote_refs: false,
            rate: 1000,
            concurrency: 1,
            max_probes: 10_000,
            max_duration_ms: 3_600_000,
            fail_on: Severity::High,
        },
        progress: Progress::default(),
        graph: Graph::default(),
    }
}

/// Run the probes `session` has not run yet; `cancel_after` cancels once
/// that many probes are done.
async fn run(
    session: &mut Session,
    model: &ApiModel,
    planned: &Plan,
    cancel_after: Option<usize>,
) -> Finished {
    let target = session.state.target.clone();
    let limits = Limits {
        rate: NonZeroU32::new(1000).unwrap(),
        concurrency: NonZeroU32::new(1).unwrap(),
        max_probes: 10_000,
        max_duration: Duration::from_secs(3600),
    };
    let sender = GovernedSender::new(policy(&target), Governor::new(limits));
    let graph = Graph::build(model, &BTreeMap::new());
    let variables = Variables::default();
    let disabled = Vec::new();
    let context = Context {
        model,
        graph: &graph,
        target: &target,
        variables: &variables,
        identity: Identity {
            environment: None,
            secrets: &NoSecrets,
        },
        identity_b: None,
        scans: &planned.scans,
        disabled_rules: &disabled,
        send: SendOptions::default(),
    };
    let executor = Executor::new(&sender, context);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let mut progress = move |p: &Progress, _: Duration| {
        if cancel_after == Some(p.done) {
            token.cancel();
        }
    };
    let options = Drive {
        plan: planned,
        model,
        concurrency: 1,
        cancel,
        progress: &mut progress,
    };
    drive(session, &executor, options).await.unwrap()
}

#[tokio::test]
async fn an_interrupted_exploration_resumes_with_the_remaining_probes() {
    let server = BadApi::start().await;
    let target = server.url("");
    let model = load_bad_api().await.model;
    let graph = Graph::build(&model, &BTreeMap::new());
    let planned = nine_hundred(&conformance(&model, &graph, &policy(&target)));
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path(), state(&target), &planned).unwrap();

    let first = run(&mut session, &model, &planned, Some(400)).await;
    let Finished::Interrupted(progress) = first else {
        panic!("not interrupted: {first:?}");
    };
    assert_eq!((progress.done, progress.total), (400, 900));
    assert_eq!(server.hits().len(), 400);
    let saved = Session::open(root.path(), "exp-20261118-abcdef").unwrap();
    assert_eq!(saved.state.state, RunState::Interrupted);
    assert_eq!(saved.state.phase, Phase::Execute);
    assert_eq!(saved.state.progress.done, 400);
    assert!(!saved.report_path().exists());

    let mut resumed = Session::open(root.path(), "exp-20261118-abcdef").unwrap();
    let plan = resumed.plan().unwrap();
    let second = run(&mut resumed, &model, &plan, None).await;
    let Finished::Completed(report) = second else {
        panic!("not completed: {second:?}");
    };
    assert_eq!(server.hits().len(), 900, "only the remaining 500 were sent");
    assert_eq!(report.summary.probes.total, 900);
    assert_eq!(report.probes.len(), 900);
    assert!(report.probes.iter().all(|p| p.outcome != "not run"));
    let ids: BTreeSet<&str> = report.probes.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids.len(), 900);
    assert_eq!(resumed.state.state, RunState::Completed);
    assert!(resumed.report_path().is_file());
}

#[tokio::test]
async fn coverage_marks_observed_unobserved_and_blocked_statuses() {
    let server = BadApi::start().await;
    let target = server.url("");
    let model = load_bad_api().await.model;
    let graph = Graph::build(&model, &BTreeMap::new());
    let planned = conformance(&model, &graph, &policy(&target));
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path(), state(&target), &planned).unwrap();
    let Finished::Completed(report) = run(&mut session, &model, &planned, None).await else {
        panic!("not completed");
    };
    let row = |op: &str| {
        report
            .coverage
            .matrix
            .iter()
            .find(|r| r.operation == op)
            .unwrap()
            .statuses
            .iter()
            .map(|c| (c.status.as_str(), c.state))
            .collect::<Vec<_>>()
    };
    assert_eq!(row("GET /users"), [("200", Cell::Observed)]);
    assert_eq!(
        row("POST /users"),
        [("201", Cell::Blocked), ("400", Cell::Blocked)]
    );
    assert!(
        row("GET /status")
            .iter()
            .all(|(_, c)| *c == Cell::NotObserved)
    );
    let ops = &report.coverage.operations;
    assert_eq!(ops.total, model.operations.len());
    let exercised = report
        .coverage
        .matrix
        .iter()
        .filter(|r| r.statuses.iter().any(|c| c.state == Cell::Observed))
        .count();
    assert!(
        ops.covered >= exercised,
        "GET /status answered undocumented 202"
    );
    let statuses = &report.coverage.statuses;
    let documented: usize = model.operations.iter().map(|o| o.responses.len()).sum();
    assert_eq!(statuses.total, documented);
    let expected = (statuses.covered as f64 * 1000.0 / documented as f64).round() / 10.0;
    assert_eq!(statuses.percent, expected);
}
