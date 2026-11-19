//! The descriptor cache (add-grpc-client task 2.3): reflection results are
//! reused until refreshed, and browsing works offline from the cache with a
//! stale notice.

use rustman_core::Redactor;
use rustman_grpc::{
    CachePolicy, DESCRIPTOR_CACHE_DIR, DescriptorCache, Remote, SchemaOrigin, reflect_schema,
};
use rustman_test_support::grpc::{GrpcServer, Reflection, UNREACHABLE_TARGET};

fn policy(root: &std::path::Path, refresh: bool) -> CachePolicy {
    CachePolicy {
        cache: Some(DescriptorCache::for_workspace(root)),
        refresh,
        redactor: Redactor::new(),
    }
}

/// Cache what `live` serves as the descriptors of `offline`, a target
/// that cannot be reached: the server "went away" without relying on a
/// freed port staying closed.
fn cache_as_offline(root: &std::path::Path, live: &Remote, offline: &Remote) {
    let cache = DescriptorCache::for_workspace(root);
    let (pool, _) = cache.read(&live.key()).expect("cached");
    cache
        .write(&offline.key(), &pool, &Redactor::new())
        .unwrap();
}

/// Spec "Offline browsing".
#[tokio::test(flavor = "multi_thread")]
async fn browsing_works_from_the_cache_with_the_server_stopped() {
    let workspace = tempfile::tempdir().unwrap();
    let server = GrpcServer::start(Reflection::V1).await;
    let remote = Remote::parse(&server.target()).unwrap();

    let fresh = reflect_schema(&remote, &policy(workspace.path(), false))
        .await
        .unwrap();
    assert_eq!(fresh.origin, SchemaOrigin::Reflection);
    assert_eq!(fresh.notice(), None);
    let dir = workspace.path().join(DESCRIPTOR_CACHE_DIR);
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        2,
        "a .bin and a .json"
    );

    let offline = Remote::parse(UNREACHABLE_TARGET).unwrap();
    cache_as_offline(workspace.path(), &remote, &offline);
    server.stop().await;

    let cached = reflect_schema(&offline, &policy(workspace.path(), false))
        .await
        .unwrap();
    assert!(matches!(cached.origin, SchemaOrigin::Cache { .. }));
    let notice = cached.notice().unwrap();
    assert!(notice.contains("may be stale"), "{notice}");
    let services = cached.services();
    let catalog = services
        .iter()
        .find(|s| s.name == "shop.v1.Catalog")
        .unwrap();
    let methods: Vec<&str> = catalog.methods.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(
        methods,
        ["GetProduct", "ListProducts", "UploadProducts", "Chat"]
    );
    let described = cached.describe("shop.v1.Catalog/GetProduct").unwrap();
    assert!(
        described.to_string().contains("\"id\": \"\""),
        "{described}"
    );

    // A refresh that cannot reach the server falls back to the cache.
    let refreshed = reflect_schema(&offline, &policy(workspace.path(), true))
        .await
        .unwrap();
    assert!(refreshed.notice().is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cache_is_reused_until_refreshed() {
    let workspace = tempfile::tempdir().unwrap();
    let server = GrpcServer::start(Reflection::Both).await;
    let remote = Remote::parse(&server.target()).unwrap();
    let first = reflect_schema(&remote, &policy(workspace.path(), false))
        .await
        .unwrap();
    assert_eq!(first.origin, SchemaOrigin::Reflection);
    let second = reflect_schema(&remote, &policy(workspace.path(), false))
        .await
        .unwrap();
    assert!(matches!(second.origin, SchemaOrigin::Cache { .. }));
    let refreshed = reflect_schema(&remote, &policy(workspace.path(), true))
        .await
        .unwrap();
    assert_eq!(refreshed.origin, SchemaOrigin::Reflection);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_server_without_a_cache_is_an_error() {
    let workspace = tempfile::tempdir().unwrap();
    let remote = Remote::parse(UNREACHABLE_TARGET).unwrap();
    let err = reflect_schema(&remote, &policy(workspace.path(), false))
        .await
        .unwrap_err();
    assert!(err.is_unreachable(), "{err}");
}
