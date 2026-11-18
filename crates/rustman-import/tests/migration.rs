//! Script migration (add-runner-parity group 6): trivial `pm.*` statements
//! become declarative assertions and captures, the rest stays comment-only,
//! and the import report and Markdown migration report count them.

mod common;

use common::{file, plan_fixture};
use rustman_core::model::{Assertion, CaptureSource, KindRegistry, Request, StatusExpectation};
use rustman_import::migration::markdown;

const FIXTURE: &str = "postman/migration.postman_collection.json";
const SECRET: &str = "sk_live_CANARY_mig_77aa";

fn http(text: &str) -> rustman_core::model::HttpRequest {
    match KindRegistry::default()
        .load(std::path::Path::new("r.rman.yaml"), text)
        .unwrap()
    {
        Request::Http(r) => *r,
        Request::Opaque(_) => panic!("not an http request"),
    }
}

#[test]
fn the_summary_counts_converted_statements() {
    let plan = plan_fixture(FIXTURE);
    let summary = plan.report.summary_text();
    assert!(
        summary.contains("scripts: 31/40 statements converted (77%)\n"),
        "{summary}"
    );
    let m = &plan.report.migration;
    assert_eq!(
        (m.found, m.mapped, m.unconverted, m.percent),
        (40, 31, 9, 77)
    );
    let per_request: Vec<(&str, &str, usize, usize)> = m
        .requests
        .iter()
        .map(|r| (r.request.as_str(), r.path.as_str(), r.found, r.mapped))
        .collect();
    assert_eq!(
        per_request,
        [
            ("Login", "shop-scripts/login.rman.yaml", 9, 7),
            ("List orders", "shop-scripts/list-orders.rman.yaml", 11, 10),
            ("Get user", "shop-scripts/get-user.rman.yaml", 9, 7),
            ("Checkout", "shop-scripts/checkout.rman.yaml", 11, 7),
        ]
    );
}

#[test]
fn mapped_statements_become_assertions_and_captures() {
    // The whole tree is in the `postman-migration` snapshot (postman.rs).
    let plan = plan_fixture(FIXTURE);
    let login = http(file(&plan, "shop-scripts/login.rman.yaml"));
    let assertions = login.assertions.unwrap();
    assert!(matches!(
        &assertions[1],
        Assertion::Latency(l) if l.total_lt == Some(300)
    ));
    let captures = login.captures.unwrap();
    let etag = &captures[1];
    assert_eq!(
        (
            etag.var.as_str(),
            etag.from,
            etag.name.as_deref(),
            etag.persist
        ),
        ("etag", CaptureSource::Header, Some("ETag"), Some(true))
    );
    let session = &captures[2];
    assert_eq!(
        (session.from, session.name.as_deref(), session.persist),
        (CaptureSource::Cookie, Some("sid"), Some(true))
    );

    let list = http(file(&plan, "shop-scripts/list-orders.rman.yaml"));
    let assertions = list.assertions.unwrap();
    assert_eq!(
        assertions[0],
        Assertion::Status(StatusExpectation::Class(2))
    );
    assert!(matches!(
        &assertions[1],
        Assertion::Header(h) if h.name == "Content-Type" && h.equals.as_deref() == Some("application/json")
    ));
    assert!(matches!(
        &assertions[3],
        Assertion::Json(j) if j.path == "$.items[0].tags" && j.length == Some(2.into())
    ));

    let checkout = http(file(&plan, "shop-scripts/checkout.rman.yaml"));
    let order_id = &checkout.captures.unwrap()[0];
    assert_eq!(
        (
            order_id.var.as_str(),
            order_id.path.as_deref(),
            order_id.persist
        ),
        ("orderId", Some("$.order.id"), None)
    );
}

#[test]
fn the_migration_report_suggests_equivalents_and_redacts_secrets() {
    let plan = plan_fixture(FIXTURE);
    let report = markdown(&plan.report);
    assert!(!report.contains(SECRET), "{report}");
    assert!(report.contains("«redacted:token»"), "{report}");
    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    settings.bind(|| insta::assert_snapshot!("migration-report", report));
}
