//! Resource limits and fault isolation (plugins spec, "Resource limits"
//! and "Plugin fault isolation"; task 3.5): a busy loop or an allocation
//! bomb fails its own call, quickly, and the next call works on a fresh
//! instance.

mod common;

use std::time::{Duration, Instant};

use common::{plugin, verified, verified_with};
use rustman_plugin::fixtures;
use rustman_plugin::limits::Limits;
use rustman_plugin::plugin::Exchange;
use rustman_plugin::sandbox::HostContext;

fn exchange() -> Exchange {
    Exchange {
        method: "GET".into(),
        url: "https://api.example.com/orders".into(),
        request_headers: Vec::new(),
        request_body: Vec::new(),
        status: 200,
        response_headers: Vec::new(),
        response_body: b"{}".to_vec(),
        duration_ms: 3,
    }
}

#[test]
fn infinite_loop() {
    let manifest = fixtures::manifest("my-check", &["assertion"], &[]);
    let verified = verified("my-check", fixtures::busy_loop(), &manifest, &[]);
    let plugin = plugin(&verified, HostContext::new("."));
    // Compile and instantiate before timing the call.
    assert_eq!(plugin.assertion_functions().unwrap(), ["spin"]);

    // Each call is stopped after its budget, and the run continues: the
    // next call gets a new instance. The upper bound takes the fastest of
    // three calls, so a briefly overloaded CI machine does not fail it.
    let mut fastest = Duration::MAX;
    for _ in 0..3 {
        let started = Instant::now();
        let err = plugin.check("spin", &exchange(), "{}").unwrap_err();
        let elapsed = started.elapsed();
        assert_eq!(
            err.to_string(),
            "plugin my-check: execution time limit exceeded"
        );
        assert!(elapsed >= Duration::from_millis(90), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
        fastest = fastest.min(elapsed);
        assert_eq!(plugin.assertion_functions().unwrap(), ["spin"]);
    }
    // 100 ms budget; loaded macOS runners have taken ~670 ms to stop a call.
    assert!(fastest < Duration::from_millis(2000), "{fastest:?}");
}

#[test]
fn a_raised_cpu_limit_is_honored() {
    let manifest = fixtures::manifest("my-check", &["assertion"], &[]);
    let limits = Limits {
        cpu_ms: 300,
        ..Limits::default()
    };
    let verified = verified_with("my-check", fixtures::busy_loop(), &manifest, &[], limits);
    let plugin = plugin(&verified, HostContext::new("."));
    plugin.assertion_functions().unwrap();
    let started = Instant::now();
    plugin.check("spin", &exchange(), "{}").unwrap_err();
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(290), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
}

#[test]
fn allocation_bomb() {
    let manifest = fixtures::manifest("bomb", &["assertion"], &[]);
    let verified = verified("bomb", fixtures::allocation_bomb(), &manifest, &[]);
    let plugin = plugin(&verified, HostContext::new("."));
    let err = plugin.check("grow", &exchange(), "{}").unwrap_err();
    assert_eq!(err.to_string(), "plugin bomb: memory limit exceeded");
    assert_eq!(plugin.assertion_functions().unwrap(), ["grow"]);
}

#[test]
fn a_lowered_memory_limit_is_honored() {
    // The fixtures start with 2 pages (128 KiB) and grow 1 MiB at a time.
    let manifest = fixtures::manifest("bomb", &["assertion"], &[]);
    let limits = Limits {
        memory_mib: 1,
        ..Limits::default()
    };
    let verified = verified_with("bomb", fixtures::allocation_bomb(), &manifest, &[], limits);
    let plugin = plugin(&verified, HostContext::new("."));
    let err = plugin.check("grow", &exchange(), "{}").unwrap_err();
    assert_eq!(err.to_string(), "plugin bomb: memory limit exceeded");
}

#[test]
fn a_trap_fails_only_its_call() {
    let manifest = fixtures::manifest("boom", &["assertion"], &[]);
    let verified = verified("boom", fixtures::trapping(), &manifest, &[]);
    let plugin = plugin(&verified, HostContext::new("."));
    let err = plugin.check("boom", &exchange(), "{}").unwrap_err();
    let message = err.to_string();
    assert!(message.starts_with("plugin boom trapped: "), "{message}");
    assert!(message.contains("unreachable"), "{message}");
    assert_eq!(plugin.assertion_functions().unwrap(), ["boom"]);
}

#[test]
fn calls_on_many_threads_are_serialized() {
    let manifest = fixtures::manifest("boom", &["assertion"], &[]);
    let verified = verified("boom", fixtures::trapping(), &manifest, &[]);
    let plugin = plugin(&verified, HostContext::new("."));
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for _ in 0..5 {
                    assert!(plugin.check("boom", &exchange(), "{}").is_err());
                    assert_eq!(plugin.assertion_functions().unwrap(), ["boom"]);
                }
            });
        }
    });
}

/// A plugin limited to 16 MiB cannot make one host call copy more than
/// that: 50 headers aliasing one 1 MiB value (50 MiB to copy, under
/// wasmtime's 128 MiB default) fail the call before the host sees them.
#[test]
fn a_host_call_copies_at_most_the_memory_limit() {
    let manifest = fixtures::manifest("flood", &["generator"], &["net"]);
    let limits = Limits {
        memory_mib: 16,
        ..Limits::default()
    };
    let grants = ["net:flood.example.com"];
    let flood = fixtures::header_flood(50, 1024 * 1024);
    let verified = verified_with("flood", flood, &manifest, &grants, limits);
    let flooding = plugin(&verified, HostContext::new("."));
    let err = flooding.generate("flood", &[]).unwrap_err();
    assert!(
        err.to_string()
            .contains("fuel allocated for hostcalls has been exhausted"),
        "{err}"
    );
    // Within the memory limit, the host's own header caps refuse the list.
    let flood = fixtures::header_flood(101, 1);
    let verified = verified_with("flood", flood, &manifest, &grants, limits);
    let flooding = plugin(&verified, HostContext::new("."));
    let err = flooding.generate("flood", &[]).unwrap_err();
    assert_eq!(err.to_string(), "plugin flood: at most 100 request headers");
}
