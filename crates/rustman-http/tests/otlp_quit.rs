//! Flushing every exporter of the process when a front end quits (the
//! TUI), against the in-process collector of rustman-test-support only.
//! A test binary of its own: the exporters are process-wide, so another
//! test's collector must not share them.
#![cfg(feature = "otlp")]

mod support;

use std::time::Duration;

use rustman_core::run::plan;
use rustman_core::{SecretStore, Workspace};
use rustman_http::runner::{RunContext, RunOptions, Runner, flush_all_exports};
use rustman_test_support::otlp::OtlpCollector;
use support::collection::write;
use support::recorder::Recorder;

#[tokio::test(flavor = "multi_thread")]
async fn flushing_all_exports_sends_what_dropped_runners_queued() {
    let server = Recorder::start().await;
    let collector = OtlpCollector::start().await;
    let dir = tempfile::tempdir().unwrap();
    let settings = format!(
        "[workspace]\nname = \"t\"\nversion = 1\n[trace.export]\nenabled = true\nendpoint = \"{}\"\n",
        collector.endpoint()
    );
    let env = format!("variables:\n  base_url: {}\n", server.url(""));
    write(
        dir.path(),
        &[
            ("rustman.toml", &settings),
            ("environments/dev.yaml", &env),
            (".gitignore", ".rustman/\n"),
            (
                "r.rman.yaml",
                "version: 1\nkind: http\nname: r\nurl: '{{base_url}}/ok'\n",
            ),
        ],
    );
    let ws = Workspace::load(dir.path()).unwrap();
    let env = ws.active_environment(Some("dev"), None).unwrap();
    let store = SecretStore::with_keychain(dir.path(), None);
    let ctx = RunContext {
        root: &ws.root,
        settings: &ws.settings,
        environment: env,
        secrets: &store,
        in_workspace: true,
    };
    let runner = Runner::new(ctx, RunOptions::default()).unwrap();
    runner.run(&plan(&ws, &[], &[])).await;
    drop(runner);
    let warnings = tokio::task::spawn_blocking(|| flush_all_exports(Duration::from_secs(20)))
        .await
        .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    // Flushed: the span is there without waiting for the 5 s batch delay.
    assert_eq!(collector.spans().len(), 1);
}
