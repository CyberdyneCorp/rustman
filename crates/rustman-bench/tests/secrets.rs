//! `{{secret:...}}` references in a bench (add-secret-providers, "References
//! SHALL be valid anywhere variable interpolation is allowed"): every VU
//! resolves them through the providers it is given, and the value reaches
//! the wire only.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{Setup, bench, constant, request, workspace, write};
use rustman_bench::Limit;
use rustman_core::secret::Secret;
use rustman_core::secrets::provider::{
    DEFAULT_TIMEOUT, DEFAULT_TTL, FetchFuture, Origin, ProviderInfo, SecretProvider,
    SecretProviders,
};
use rustman_test_support::BenchServer;

const CANARY: &str = "canary-bench-provider-3e1a";

#[derive(Default)]
struct Counting(AtomicUsize);

impl SecretProvider for Counting {
    fn fetch<'a>(&'a self, path: &'a str) -> FetchFuture<'a> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(Secret::new(path, CANARY.to_owned())) })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vus_resolve_secret_references_through_the_providers() {
    let server = BenchServer::start().await;
    let ws = workspace(&server.origin(), "");
    write(
        ws.path(),
        "a.rman.yaml",
        &request("GET", "{{base_url}}/reflect?k={{secret:ci/KEY}}", ""),
    );
    let spy = Arc::new(Counting::default());
    let mut providers = SecretProviders::new();
    let info = ProviderInfo {
        name: "ci".into(),
        kind: "spy".into(),
        origin: Origin::Workspace,
        auth: None,
        timeout: DEFAULT_TIMEOUT,
        ttl: DEFAULT_TTL,
    };
    providers.insert(info, spy.clone());
    let mut setup = Setup::new(&server.origin(), constant(2, Limit::Iterations(6)));
    setup.secret_providers = Some(Arc::new(providers));
    let (outcome, _) = bench(ws.path(), setup, &[]).await.unwrap();
    assert_eq!(outcome.summary.overall.requests, 6);
    assert!(
        outcome.error_samples.is_empty(),
        "{:?}",
        outcome.error_samples
    );
    let hits = server.hits();
    assert_eq!(hits.len(), 6);
    assert!(hits.iter().all(|h| h.param("k") == CANARY));
    assert!(spy.0.load(Ordering::SeqCst) >= 1);
}
