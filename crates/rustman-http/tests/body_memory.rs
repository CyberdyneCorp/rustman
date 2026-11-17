//! The memory ceiling of a spilled download (task 4.10): a 50 MiB response
//! received with defaults keeps less than 16 MiB of heap for its body.
//!
//! The counting allocator sees the whole process, so this file holds a
//! single test and nothing else runs alongside it.

mod support;

use peak_alloc::PeakAlloc;
use rustman_http::{BodyOptions, DEFAULT_MAX_BODY_BYTES, HttpEngine, SendOptions};
use support::TestServer;
use support::request::get;

#[global_allocator]
static ALLOCATOR: PeakAlloc = PeakAlloc;

const FIFTY_MIB: u64 = 50 * 1024 * 1024;
const CEILING: usize = 16 * 1024 * 1024;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_50_mib_download_spills_under_the_memory_ceiling() {
    let server = TestServer::http().await;
    let dir = tempfile::tempdir().unwrap();
    let options = SendOptions {
        body: BodyOptions {
            spill_dir: dir.path().join(".rustman/bodies"),
            ..BodyOptions::default()
        },
        ..SendOptions::default()
    };
    assert_eq!(options.body.max_in_memory, DEFAULT_MAX_BODY_BYTES);
    let engine = HttpEngine::new();
    let request = get(&server.url(&format!("/bytes/{FIFTY_MIB}")));

    let baseline = ALLOCATOR.current_usage();
    ALLOCATOR.reset_peak_usage();
    let exchange = engine.send(&request, &options).await.unwrap();
    let used = ALLOCATOR.peak_usage().saturating_sub(baseline);

    assert!(used < CEILING, "peak body memory {used} bytes");
    assert_eq!(exchange.size_bytes, FIFTY_MIB);
    let spill = exchange.spill.as_ref().unwrap();
    assert_eq!(std::fs::metadata(spill).unwrap().len(), FIFTY_MIB);
    assert!(spill.starts_with(dir.path().join(".rustman/bodies")));
    assert!(exchange.binary);
}
