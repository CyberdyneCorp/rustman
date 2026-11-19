//! Rich error details (add-grpc-client task 4.3): `grpc-status-details-bin`
//! decoded as `google.rpc.Status` with the embedded descriptor set.

mod common;

use common::{Fixture, request_file};
use rustman_test_support::grpc::{GrpcServer, Reflection};

const GET: &str = "shop.v1.Catalog/GetProduct";

async fn status_of(id: &str) -> rustman_grpc::GrpcStatus {
    let server = GrpcServer::start(Reflection::None).await;
    let fixture = Fixture::new();
    let message = format!("message: {{id: {id}}}\n");
    let exchange = fixture
        .send(&request_file(&server.target(), GET, &message))
        .await
        .unwrap();
    exchange.status
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_request_shows_its_field_violations() {
    let status = status_of("invalid").await;
    assert_eq!(status.label(), "INVALID_ARGUMENT (3)");
    assert_eq!(
        status.render(),
        "INVALID_ARGUMENT (3): invalid product id\n  field violation: id: must start with p-"
    );
    let detail = &status.details[0];
    assert_eq!(detail.type_name(), "google.rpc.BadRequest");
    assert_eq!(
        detail.value.as_ref().unwrap()["fieldViolations"][0]["field"],
        "id"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn error_info_shows_reason_and_domain() {
    let status = status_of("quota").await;
    assert_eq!(status.label(), "RESOURCE_EXHAUSTED (8)");
    assert_eq!(
        status.details[0].lines(),
        ["error info: reason QUOTA_EXCEEDED, domain shop.example"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_detail_type_shows_its_url_and_base64() {
    let status = status_of("mystery").await;
    assert_eq!(status.label(), "INTERNAL (13)");
    let detail = &status.details[0];
    assert!(detail.value.is_none());
    assert_eq!(detail.base64.as_deref(), Some("AQID"));
    assert_eq!(detail.lines(), ["type.example.com/acme.Mystery: AQID"]);
    let shown = serde_json::to_value(detail).unwrap();
    assert_eq!(shown["type_url"], "type.example.com/acme.Mystery");
}
