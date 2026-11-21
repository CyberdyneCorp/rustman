//! Fuzz mixing and malformed families under load (add-stress-testing
//! tasks 3.1-3.4, 4.1-4.3): valid and fuzzed traffic measured apart, the
//! baseline phase and `fuzz-degrades-valid-traffic`, malformed bodies that
//! keep the request's auth, and the malformed-input findings.
//!
//! The stub judges bodies server-side; SLO windows last 1 s, and latency
//! thresholds have wide margins (a 200 ms base delay against an 800 ms
//! slowdown), so slow runners do not flip a verdict.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{fast_recovery, request, write};
use rustman_bench::stress::run::Tuning;
use rustman_bench::stress::{FlatSpec, ModeSpec, Stress, StressInput, StressOutcome, Timing};
use rustman_core::model::Variables;
use rustman_core::safety::Permission;
use rustman_core::{Secret, SecretBackend, SecretStore, Workspace};
use rustman_test_support::StressServer;
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "fuzz-canary-7f3a9c";

/// A workspace whose `[stress]` lists `origin` (with `settings`), a `dev`
/// environment holding the `api_token` secret, and a POST with a JSON
/// body, bearer auth and the token in an `X-Token` header to `path`.
fn workspace(origin: &str, settings: &str, path: &str) -> tempfile::TempDir {
    let dir = common::workspace(
        origin,
        &format!("[stress]\ntargets = [\"{origin}\"]\n{settings}"),
    );
    write(
        dir.path(),
        "environments/dev.yaml",
        &format!("variables:\n  base_url: {origin}\nsecrets: [api_token]\n"),
    );
    let store = SecretStore::open(dir.path(), SecretBackend::File);
    store
        .set("dev", &Secret::new("api_token", TOKEN.to_owned()))
        .unwrap();
    let extra = "auth: {type: bearer, token: \"{{api_token}}\"}\n\
                 headers:\n  - name: X-Token\n    value: 'Bearer {{api_token}}'\n\
                 body:\n  type: json\n  content: {qty: 2, sku: A1}\n";
    write(
        dir.path(),
        "api/order.rman.yaml",
        &request("POST", &format!("{{{{base_url}}}}{path}"), extra),
    );
    dir
}

struct Fuzz {
    spec: Option<rustman_explore::ApiModel>,
    ratio: Option<f64>,
    malformed: &'static [&'static str],
    rps: f64,
    duration: Duration,
    baseline: Duration,
    seed: u64,
}

impl Fuzz {
    fn new(malformed: &'static [&'static str]) -> Self {
        Self {
            spec: None,
            ratio: Some(0.5),
            malformed,
            rps: 10.0,
            duration: Duration::from_secs(2),
            baseline: Duration::from_secs(1),
            seed: 42,
        }
    }
}

/// 1-second windows of two 500 ms ticks.
fn fast() -> Tuning {
    Tuning {
        timing: Timing {
            tick: Duration::from_millis(500),
            window_ticks: 2,
            warmup_windows: 1,
        },
        recovery: fast_recovery(),
        ..Tuning::default()
    }
}

fn input(root: &Path, target: &str, fuzz: Fuzz) -> StressInput {
    StressInput {
        workspace: Arc::new(Workspace::load(root).unwrap()),
        environment: Some("dev".into()),
        secrets: Arc::new(SecretStore::open(root, SecretBackend::File)),
        scopes: Vec::new(),
        mode: ModeSpec::Fuzz(FlatSpec {
            rps: fuzz.rps,
            duration: fuzz.duration,
        }),
        target: target.to_owned(),
        permission: Permission::Mutations,
        production: None,
        fuzz_ratio: fuzz.ratio,
        malformed: fuzz.malformed.iter().map(|f| (*f).to_owned()).collect(),
        wire: Vec::new(),
        allow_wire_faults: false,
        seed: Some(fuzz.seed),
        spec: fuzz.spec,
        fuzz_baseline: Some(fuzz.baseline),
        slos: Vec::new(),
        overrides: Variables::default(),
        timeout: Some(Duration::from_secs(10)),
        insecure: false,
        treat_throttling_as_failure: false,
        max_vus: None,
        health: None,
        recovery_timeout: None,
        interrupt: CancellationToken::new(),
        cancel: CancellationToken::new(),
        tuning: fast(),
    }
}

async fn fuzz(root: &Path, target: &str, fuzz: Fuzz) -> StressOutcome {
    let seed = fuzz.seed;
    let stress = Stress::prepare(input(root, target, fuzz)).unwrap();
    assert_eq!(stress.seed(), seed);
    stress.run().await.unwrap()
}

fn rules(outcome: &StressOutcome) -> Vec<&'static str> {
    outcome.findings.iter().map(|f| f.rule).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn valid_and_fuzzed_traffic_are_measured_apart() {
    let _serial = common::serial().await;
    let server = StressServer::start().await;
    let ws = workspace(&server.origin(), "", "/fuzz/strict");
    let outcome = fuzz(ws.path(), &server.origin(), Fuzz::new(&["json-syntax"])).await;
    assert_eq!(outcome.seed, 42);
    let fuzz = outcome.fuzz.as_ref().unwrap();
    let fuzzed = fuzz.fuzzed.overall.requests;
    let valid = outcome.summary.overall.requests;
    assert!(
        fuzzed > 0 && valid > fuzzed,
        "valid {valid}, fuzzed {fuzzed}"
    );
    // Valid traffic only: the strict stub refuses only fuzzed bodies.
    assert_eq!(outcome.summary.overall.error_rate, 0.0);
    assert!(fuzz.fuzzed.overall.error_rate > 0.0);
    assert_eq!(fuzz.baseline_ticks, 2);
    assert!(fuzz.baseline.is_some() && fuzz.valid.is_some());
    // Fuzzed windows offer as many requests as before: valid ones are
    // offered minus the fuzzed.
    let fuzzing: u64 = outcome.windows.iter().map(|w| w.stats.fuzzed).sum();
    assert_eq!(fuzzing, fuzzed);
    assert!(
        outcome.windows[0].stats.fuzzed == 0,
        "the baseline is clean"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_bodies_keep_the_request_auth_and_redact_it_in_evidence() {
    let _serial = common::serial().await;
    let server = StressServer::start().await;
    let ws = workspace(&server.origin(), "", "/fuzz/accept");
    let mut setup = Fuzz::new(&["duplicate-keys", "json-syntax"]);
    setup.ratio = None;
    let outcome = fuzz(ws.path(), &server.origin(), setup).await;
    assert!(server.malformed() > 0);
    assert_eq!(server.malformed_with_auth(), server.malformed());
    let accepted: Vec<_> = outcome
        .findings
        .iter()
        .filter(|f| f.rule == "accepted-malformed-input")
        .collect();
    let families: Vec<&str> = accepted
        .iter()
        .filter_map(|f| f.family.as_deref())
        .collect();
    assert_eq!(families, ["duplicate-keys", "json-syntax"], "{accepted:#?}");
    let evidence = accepted[0].evidence.as_ref().unwrap();
    assert!(
        evidence.request.starts_with("POST http://127.0.0.1:"),
        "{evidence:?}"
    );
    assert_eq!(evidence.response, "201");
    // The response's own body, not its status.
    assert_eq!(evidence.response_body, "ok");
    assert_eq!(evidence.body, r#"{\"qty\":2,\"qty\":\"2\",\"sku\":\"A1\"}"#);
    // A json-syntax body went out as JSON: its evidence (and so its
    // regression export) keeps the Content-Type it was sent with.
    let syntax = accepted[1].evidence.as_ref().unwrap();
    let content_type = syntax
        .headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.as_str());
    assert_eq!(content_type, Some("application/json"), "{syntax:?}");
    let token = evidence
        .headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("x-token"))
        .unwrap();
    assert_eq!(token.1, "Bearer «redacted:api_token»");
    let everything = format!("{:?}", outcome.findings);
    assert!(!everything.contains(TOKEN));
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_bodies_accepted_are_reported() {
    let _serial = common::serial().await;
    let server = StressServer::start().await;
    let ws = workspace(
        &server.origin(),
        "expected_max_body = 1024\n",
        "/fuzz/accept",
    );
    let mut setup = Fuzz::new(&["large-body"]);
    setup.ratio = None;
    let outcome = fuzz(ws.path(), &server.origin(), setup).await;
    let oversized = outcome
        .findings
        .iter()
        .find(|f| f.rule == "oversized-input-accepted")
        .unwrap();
    assert_eq!(oversized.family.as_deref(), Some("large-body"));
    // Large bodies are well-formed: accepting them is no malformed input.
    assert!(!rules(&outcome).contains(&"accepted-malformed-input"));

    // Without `expected_max_body`, nothing says how large is too large.
    let ws = workspace(&server.origin(), "", "/fuzz/accept");
    let mut setup = Fuzz::new(&["large-body"]);
    setup.ratio = None;
    let outcome = fuzz(ws.path(), &server.origin(), setup).await;
    assert!(!rules(&outcome).contains(&"oversized-input-accepted"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_parser_crash_names_the_family_and_the_smallest_failing_depth() {
    let _serial = common::serial().await;
    let server = StressServer::start().await;
    let ws = workspace(&server.origin(), "", "/fuzz/nest?fail_depth=5000");
    let mut setup = Fuzz::new(&["deep-nesting"]);
    setup.ratio = Some(1.0);
    setup.rps = 20.0;
    let outcome = fuzz(ws.path(), &server.origin(), setup).await;
    let crash = outcome
        .findings
        .iter()
        .find(|f| {
            f.rule == "server-error-on-malformed-input"
                && f.family.as_deref() == Some("deep-nesting")
        })
        .unwrap_or_else(|| panic!("{:#?}", outcome.findings));
    assert!(
        crash.message.contains("smallest failing depth: 5000"),
        "{}",
        crash.message
    );
    assert_eq!(crash.evidence.as_ref().unwrap().response, "500");
}

#[tokio::test(flavor = "multi_thread")]
async fn fuzzing_that_slows_valid_traffic_is_reported() {
    let _serial = common::serial().await;
    let poison = StressServer::start().await;
    let ws = workspace(
        &poison.origin(),
        "",
        "/fuzz/poison?base=200&slow=800&hold=2000",
    );
    let mut setup = Fuzz::new(&["json-syntax"]);
    setup.ratio = Some(0.3);
    setup.duration = Duration::from_secs(3);
    let outcome = fuzz(ws.path(), &poison.origin(), setup).await;
    assert!(
        rules(&outcome).contains(&"fuzz-degrades-valid-traffic"),
        "{:#?}",
        outcome.fuzz
    );
    let finding = outcome
        .findings
        .iter()
        .find(|f| f.rule == "fuzz-degrades-valid-traffic")
        .unwrap();
    assert!(finding.message.contains("baseline"), "{}", finding.message);

    let fine = StressServer::start().await;
    let ws = workspace(&fine.origin(), "", "/fuzz/strict?base=200");
    let mut setup = Fuzz::new(&["json-syntax"]);
    setup.ratio = Some(0.3);
    setup.duration = Duration::from_secs(3);
    let outcome = fuzz(ws.path(), &fine.origin(), setup).await;
    assert!(
        !rules(&outcome).contains(&"fuzz-degrades-valid-traffic"),
        "{:#?}",
        outcome.fuzz
    );
}

const SPEC: &str = r#"
openapi: 3.0.3
info: {title: Orders, version: "1"}
paths:
  /fuzz/accept:
    post:
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required: [qty, sku]
              properties:
                qty: {type: integer, minimum: 1, maximum: 10}
                sku: {type: string, maxLength: 4}
      responses:
        "201": {description: created}
"#;

#[tokio::test(flavor = "multi_thread")]
async fn spec_variants_break_the_schema_and_are_reported_when_accepted() {
    let _serial = common::serial().await;
    let server = StressServer::start().await;
    let ws = workspace(&server.origin(), "", "/fuzz/accept");
    let path = ws.path().join("openapi.yaml");
    std::fs::write(&path, SPEC).unwrap();
    let model = rustman_explore::import(path.to_str().unwrap(), &Default::default())
        .await
        .unwrap()
        .model;
    let mut setup = Fuzz::new(&[]);
    setup.spec = Some(model.clone());
    setup.ratio = Some(1.0);
    setup.rps = 20.0;
    let outcome = fuzz(ws.path(), &server.origin(), setup).await;
    let schema: Vec<&str> = outcome
        .findings
        .iter()
        .filter(|f| f.family.as_deref() == Some("schema"))
        .map(|f| f.rule)
        .collect();
    assert_eq!(
        schema,
        ["accepted-malformed-input", "oversized-input-accepted"],
        "{:#?}",
        outcome.findings
    );

    // A document without fuzz mixing is a usage error.
    let mut setup = Fuzz::new(&["json-syntax"]);
    setup.spec = Some(model);
    setup.ratio = None;
    let mut only_malformed = input(ws.path(), &server.origin(), setup);
    only_malformed.mode = ModeSpec::Soak(rustman_bench::stress::SoakSpec {
        rps: 1.0,
        duration: Duration::from_secs(1),
        ..Default::default()
    });
    let err = Stress::prepare(only_malformed).err().unwrap();
    assert_eq!(
        err.to_string(),
        "stress: --spec needs fuzz mixing (--fuzz-ratio or the fuzz mode)"
    );
}

/// A GET-only plan has no JSON body to mutate: nothing is fuzzed, so the
/// summary warns instead of claiming a share was replaced, and valid
/// traffic is never judged against fuzzing.
#[tokio::test(flavor = "multi_thread")]
async fn a_plan_without_a_json_body_fuzzes_nothing_and_says_so() {
    let _serial = common::serial().await;
    let server = StressServer::start().await;
    let ws = workspace(&server.origin(), "", "/fuzz/accept");
    std::fs::remove_file(ws.path().join("api/order.rman.yaml")).unwrap();
    write(
        ws.path(),
        "api/list.rman.yaml",
        &request("GET", "{{base_url}}/fuzz/accept", ""),
    );
    let outcome = fuzz(ws.path(), &server.origin(), Fuzz::new(&[])).await;
    let fuzzing = outcome.fuzz.as_ref().unwrap();
    assert_eq!(fuzzing.fuzzed.overall.requests, 0);
    assert!(outcome.summary.overall.requests > 0);
    assert!(fuzzing.impact().is_none());
    assert!(!rules(&outcome).contains(&"fuzz-degrades-valid-traffic"));
    let text = rustman_bench::stress::summary::render(&outcome);
    assert!(
        text.contains("fuzz: warning: no request was fuzzed (seed 42)"),
        "{text}"
    );
    assert!(!text.contains("of requests replaced"), "{text}");
}
