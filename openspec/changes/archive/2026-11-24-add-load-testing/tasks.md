# Tasks

See design.md "Building on existing code" for the components each task reuses.

## 1. Crate and executor reuse

- [x] 1.1 Create `crates/rustman-bench` (adding `hdrhistogram` and, as a dev-dependency, `criterion` to `[workspace.dependencies]`) and the `rustman bench` subcommand (`--target`, `--env`, `--var`, `--vus`, `--duration`, `--iterations`). Verify: `cargo build`, and a `--help` snapshot.
- [x] 1.2 Reuse the runner in `crates/rustman-http/src/runner/` per VU (a `Runner` per VU, or a per-request entry point from `runner/execute.rs`, per the design open question) with a per-VU variable scope, capture scope and connection pool (`HttpEngine`). Verify: a unit test where two VUs with different data never share captured variables.
- [x] 1.3 Benchmark executor overhead per request with `criterion` against a local hyper server. Verify: the result is recorded in `docs/bench-internals.md`, and the task fails if overhead exceeds 50 µs at p50.

## 2. Safety and caps

- [x] 2.1 Make `--target` mandatory and build a `SafetyPolicy` whose allowlist is its origin only (`PolicyOptions { target, allow_hosts: &[] }`, ignoring `[safety].allowed_hosts`), statically resolve request URLs before load, and count runtime `blocked`; send through `RunOptions::gate` with a `GovernedSender`. Verify: tests for a missing target (exit 2) and a foreign host (exit 4, `blocked: host not allowed (other.example.com)`, zero requests sent, checked on a counting server).
- [x] 2.2 Apply the policy's method gating (`Permission::from_flags`) and production guard (`--production <host>`; `PolicyError::DestructiveOnProduction`). Verify: tests for POST without `--allow-mutations` (exit 4, nothing sent) and for `prod` without the flag.
- [x] 2.3 Add `[bench]` to `Settings` (`max_vus` 50, `max_duration` 5m, `max_rps` 200) so flags can't exceed it, and a global `rustman_core::safety::Governor` built from those caps rather than `[safety]` (429 handling per the design open question). Verify: tests for the exact error message, no `unknown key 'bench'` warning, and that the measured rps never exceeds `max_rps` by more than 5 %.
- [x] 2.4 Ensure the agent interface (`crates/rustman-mcp/src/server/tools/mod.rs`) registers no bench tool. Verify: a test lists the agent tools and asserts none starts a bench.

## 3. Executors and data

- [x] 3.1 Implement `constant-vus` with fixed or ranged think time and duration/iteration limits. Verify: a test with a counting server asserts VU concurrency and that iteration order matches collection order.
- [x] 3.2 Implement `ramping-vus` stages with graceful ramp-down (max 30s). Verify: a test samples active VUs at stage boundaries within ±1 VU tolerance.
- [x] 3.3 Implement `constant-arrival-rate` with a VU pool, `dropped` accounting and scheduled-time latency. Verify: a slow server test asserts the dropped count, and a stall test asserts the corrected p99 includes the stall.
- [x] 3.4 Implement CSV/JSON data feeding with `sequential`/`per-vu`/`random` modes and `stop`/`recycle` exhaustion. Verify: unit tests per mode.
  Note (add-runner-parity 1.2): read data files with the shared `rustman_core::data_rows` reader (`DataRows::read`, `DataRows::parse`), not a bench-specific parser, so `--data users.csv` means the same in `run` and `bench`; the bench data-feeding tests then run against it.
- [x] 3.5 Document executors, open versus closed models and data feeding in `docs/bench.md`. Verify: docs build.

## 4. Metrics and thresholds

- [x] 4.1 Implement per-second HDR histograms per file and metric, plus RPS, error classes, phase timings (from `rustman_http::Timings`) and bytes, merged at the end. Verify: unit tests against known latency distributions (percentile error below 0.1 %).
- [x] 4.2 Implement the threshold parser and end-of-run evaluation with exit code 1 on breach. Verify: table-driven parser tests, plus an end-to-end test of breached p95 (exit 1) and passing `error_rate` (exit 0).
- [x] 4.3 Implement abort-on-fail `!` thresholds (10s warm-up, checked each second, stop within 2s). Verify: an error-storm test asserts an early stop and the `aborted: threshold` marker.
- [x] 4.4 Implement Ctrl+C handling (drain 10s, second Ctrl+C cancels through `RunOptions::cancel`, partial report, exit `INTERRUPTED` = 130 from `crates/rustman-cli/src/exit.rs`). Verify: an integration test sends SIGINT and asserts report contents and the exit code.
- [x] 4.5 Add generator-lag measurement and the "generator-bound" warning. Verify: a test with an artificially throttled runtime triggers the warning.

## 5. Reports and redaction

- [x] 5.1 Add `kind: bench` as a `rustman_report::Report` variant (additive) with profile, metrics, time series, thresholds and error samples (max 20 per class), its schema part generated in `crates/rustman-report/src/schema.rs`, and `docs/schemas/report.schema.json` regenerated; replace `bench` as the unknown kind in `report.rs`'s refusal test. Verify: the report validates against `rustman report schema`, `rustman report render` and the agent report tools read it, and a reader without the kind refuses it with exit 2.
- [x] 5.2 Add the offline HTML bench template beside the existing `minijinja` templates in `crates/rustman-report/src/render/templates` (latency-over-time and RPS charts). Verify: a snapshot test, plus a check that the HTML has no external URLs.
- [x] 5.3 Apply redaction (`rustman_core::Redactor`) to output, samples, reports and viz events, including `--secret-columns` and auto-marked `password`/`token` columns. Verify: a test greps all artifacts for a planted secret.
  Note: viz events do not exist yet; task 6.2 builds them from the same redacted metrics and adds them to the canary test.

## 6. Live dashboards

- [x] 6.1 Implement the TUI/TTY live dashboard (time, VUs, RPS, percentiles, error rate, sparklines, threshold status, per-file table, 4 Hz max) and the non-TTY 10-second summary lines. Verify: ratatui `TestBackend` snapshots, plus a non-TTY output test.
- [x] 6.2 Add `View::Bench`, `ReportKind::Bench` and `bench_tick`/`bench_finished` (priority in `Message::is_priority`) to `rustman-viz-protocol`, and implement `--viz` streaming with the companion discovery and handshake shared with `crates/rustman-tui/src/viz.rs`, plus the missing-`bench`-view warning. Verify: a test with a stub companion advertising and omitting the view, asserting the bench result is unchanged.
- [x] 6.3 Add the bench dashboard view to `rustman-viz` (behind feature `raven`) and advertise `bench` in `hello_ack`. Blocked on `add-raven-visualizer` (the companion crate doesn't exist yet; see design open questions). Verify: a companion handshake test plus a headless render smoke test on the lavapipe CI job from `add-raven-visualizer`. Moved to #123 at archive time.
  Note: still blocked. Bench ships without it: `--viz` against a companion without the view warns `viz companion has no bench view` and runs unchanged (6.2), and the `bench` view, `bench_tick` and `bench_finished` are specified in `docs/viz-protocol.md` for the companion to implement.

## 7. Integration

- [x] 7.1 End-to-end: a ramping bench of a two-file collection against a local test API with thresholds, JSON and HTML reports, and `--viz` with a stub companion. Verify: the CI job passes on Linux, macOS and Windows. Done: `bench_viz` runs in the `test` job on Linux, macOS and Windows; passed in CI run 45089029769 on main.
  Note: the test is `crates/rustman-cli/tests/bench_viz.rs` (no libtest harness, so its own executable is the stub companion on every platform); it passes locally on macOS, and the box stays open until CI has passed on Linux and Windows.
