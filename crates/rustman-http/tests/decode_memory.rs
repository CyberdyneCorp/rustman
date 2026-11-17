//! The memory ceiling of decompression: a small, highly compressed body that
//! decodes to 96 MiB keeps less than 16 MiB of heap, whatever the ratio.
//!
//! The counting allocator sees the whole process, so this file holds a
//! single test and nothing else runs alongside it.

mod support;

use peak_alloc::PeakAlloc;
use rustman_http::{BodyOptions, HttpEngine, SendOptions};
use support::TestServer;
use support::request::get;

#[global_allocator]
static ALLOCATOR: PeakAlloc = PeakAlloc;

const DECODED: u64 = 96 * 1024 * 1024;
const CEILING: usize = 16 * 1024 * 1024;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decompression_bombs_decode_under_the_memory_ceiling() {
    let server = TestServer::http().await;
    let dir = tempfile::tempdir().unwrap();
    let options = SendOptions {
        body: BodyOptions {
            spill_dir: dir.path().join(".rustman/bodies"),
            ..BodyOptions::default()
        },
        ..SendOptions::default()
    };
    let engine = HttpEngine::new();
    for encoding in ["br", "zstd"] {
        let request = get(&server.url(&format!("/zeros/{encoding}/{DECODED}")));
        let baseline = ALLOCATOR.current_usage();
        ALLOCATOR.reset_peak_usage();
        let exchange = engine.send(&request, &options).await.unwrap();
        let used = ALLOCATOR.peak_usage().saturating_sub(baseline);

        assert!(used < CEILING, "{encoding}: peak memory {used} bytes");
        assert_eq!(exchange.size_bytes, DECODED, "{encoding}");
        let spill = exchange.spill.as_ref().unwrap();
        assert_eq!(std::fs::metadata(spill).unwrap().len(), DECODED);
        std::fs::remove_file(spill).unwrap();
    }
}
