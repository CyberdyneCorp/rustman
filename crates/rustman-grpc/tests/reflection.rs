//! Server reflection (add-grpc-client task 2.1): v1, the v1alpha fallback,
//! and a server without reflection, against in-process tonic servers.

mod common;

use std::time::{Duration, Instant};

use common::{Fixture, request_file};
use rustman_grpc::{
    CachePolicy, CallType, EXIT_CONFIG, EXIT_REQUEST, GrpcError, Remote, SchemaOrigin,
    SchemaSource, load_schema, reflect_schema, schema_for,
};
use rustman_test_support::grpc::{GrpcServer, Reflection, UNREACHABLE_TARGET};

async fn schema_of(server: &GrpcServer) -> Result<rustman_grpc::Schema, GrpcError> {
    let remote = Remote::parse(&server.target()).unwrap();
    reflect_schema(&remote, &CachePolicy::default()).await
}

fn catalog_methods(schema: &rustman_grpc::Schema) -> Vec<(String, CallType)> {
    let services = schema.services();
    let catalog = services
        .iter()
        .find(|s| s.name == "shop.v1.Catalog")
        .expect("shop.v1.Catalog is listed");
    catalog
        .methods
        .iter()
        .map(|m| (m.name.clone(), m.call_type))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn v1_reflection_lists_services_and_resolves_dependencies() {
    let server = GrpcServer::start(Reflection::V1).await;
    let schema = schema_of(&server).await.unwrap();
    assert_eq!(schema.origin, SchemaOrigin::Reflection);
    assert_eq!(
        catalog_methods(&schema),
        [
            ("GetProduct".to_owned(), CallType::Unary),
            ("ListProducts".to_owned(), CallType::ServerStreaming),
            ("UploadProducts".to_owned(), CallType::ClientStreaming),
            ("Chat".to_owned(), CallType::Bidi),
        ]
    );
    // Money and Category live in an imported file; Timestamp is well-known.
    let pool = &schema.pool;
    for name in ["shop.v1.Money", "google.protobuf.Timestamp"] {
        assert!(pool.get_message_by_name(name).is_some(), "{name}");
    }
    assert!(pool.get_enum_by_name("shop.v1.Category").is_some());
}

/// Spec "v1alpha fallback".
#[tokio::test(flavor = "multi_thread")]
async fn v1alpha_only_servers_are_listed() {
    let server = GrpcServer::start(Reflection::V1Alpha).await;
    let schema = schema_of(&server).await.unwrap();
    assert_eq!(catalog_methods(&schema).len(), 4);
}

/// Spec "No reflection": the error names the target, exit code 2.
#[tokio::test(flavor = "multi_thread")]
async fn servers_without_reflection_fail_with_exit_code_2() {
    let server = GrpcServer::start(Reflection::None).await;
    let remote = Remote::parse(&server.target()).unwrap();
    let err = load_schema(&SchemaSource::Reflection, &remote, &CachePolicy::default())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "grpc: server reflection unavailable at {}; set schema.protos or schema.descriptor_set",
            server.target()
        )
    );
    assert_eq!(err.exit_code(), EXIT_CONFIG);
    assert_eq!(err.exit_code(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_server_is_a_request_error() {
    let target = UNREACHABLE_TARGET;
    let remote = Remote::parse(target).unwrap();
    let err = reflect_schema(&remote, &CachePolicy::default())
        .await
        .unwrap_err();
    assert!(err.is_unreachable(), "{err}");
    assert!(
        err.to_string()
            .starts_with(&format!("grpc: cannot connect to {target}: "))
    );
    assert_eq!(err.exit_code(), 3);
}

/// A listener that accepts connections and never answers: grpc://addr.
async fn silent_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    format!("grpc://{addr}")
}

/// A server that never answers cannot hold reflection past its timeout.
#[tokio::test(flavor = "multi_thread")]
async fn reflection_gives_up_on_a_silent_server() {
    let target = silent_server().await;
    let remote = Remote::parse(&target)
        .unwrap()
        .with_schema_timeout(Duration::from_millis(300));
    let started = Instant::now();
    let err = reflect_schema(&remote, &CachePolicy::default())
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(
        err.to_string(),
        format!("grpc: reflection at {target} failed: no answer within 300 ms")
    );
    assert_eq!(err.exit_code(), EXIT_REQUEST);
}

/// Requests (`rustman run`, `grpc list`, agent tools) bound reflection by
/// the run's timeout.
#[tokio::test(flavor = "multi_thread")]
async fn request_schemas_use_the_run_timeout() {
    let target = silent_server().await;
    let f = Fixture::new();
    f.write(
        "r.rman.yaml",
        &request_file(&target, "shop.v1.Catalog/GetProduct", "message: {}\n"),
    );
    let resolved = f.resolve("r").unwrap();
    let mut options = f.send_options(&resolved);
    options.timeout = Duration::from_millis(250);
    let started = Instant::now();
    let err = schema_for(&resolved, &options, None, false)
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(20));
    assert!(
        err.to_string().ends_with("no answer within 250 ms"),
        "{err}"
    );
}
