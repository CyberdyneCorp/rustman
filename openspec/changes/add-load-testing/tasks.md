# Tasks

## 1. Crate and executor reuse

- [ ] 1.1 Create `crates/rustman-bench` and the `rustman bench` subcommand (`--target`, `--env`, `--var`, `--vus`, `--duration`, `--iterations`). Verify: `cargo build`, and a `--help` snapshot.
- [ ] 1.2 Expose the `cli-runner` request executor for reuse with a per-VU variable scope, capture scope and connection pool. Verify: a unit test where two VUs with different data never share captured variables.
- [ ] 1.3 Benchmark executor overhead per request with `criterion` against a local hyper server. Verify: the result is recorded in `docs/bench-internals.md`, and the task fails if overhead exceeds 50 µs at p50.

## 2. Safety and caps

- [ ] 2.1 Make `--target` mandatory, pin the allowlist to its origin, statically resolve request URLs before load, and count runtime `blocked`. Verify: tests for a missing target (exit 2) and a foreign host (exit 4 with zero requests sent, checked on a counting server).
- [ ] 2.2 Apply safety-policy method gating and the production guard (`--production <host>`, destructive always refused). Verify: tests for POST without `--allow-mutations` (exit 4, nothing sent) and for `prod` without the flag.
- [ ] 2.3 Implement the `[bench]` caps (`max_vus` 50, `max_duration` 5m, `max_rps` 200), so flags can't exceed them, plus a global `governor` limiter. Verify: tests for the exact error message and that the measured rps never exceeds `max_rps` by more than 5 %.
- [ ] 2.4 Ensure the agent interface registers no bench tool. Verify: a test lists the agent tools and asserts none starts a bench.

## 3. Executors and data

- [ ] 3.1 Implement `constant-vus` with fixed or ranged think time and duration/iteration limits. Verify: a test with a counting server asserts VU concurrency and that iteration order matches collection order.
- [ ] 3.2 Implement `ramping-vus` stages with graceful ramp-down (max 30s). Verify: a test samples active VUs at stage boundaries within ±1 VU tolerance.
- [ ] 3.3 Implement `constant-arrival-rate` with a VU pool, `dropped` accounting and scheduled-time latency. Verify: a slow server test asserts the dropped count, and a stall test asserts the corrected p99 includes the stall.
- [ ] 3.4 Implement CSV/JSON data feeding with `sequential`/`per-vu`/`random` modes and `stop`/`recycle` exhaustion. Verify: unit tests per mode.
  Note (add-runner-parity 1.2): read data files with the shared `rustman_core::data_rows` reader (`DataRows::read`, `DataRows::parse`), not a bench-specific parser, so `--data users.csv` means the same in `run` and `bench`; the bench data-feeding tests then run against it.
- [ ] 3.5 Document executors, open versus closed models and data feeding in `docs/bench.md`. Verify: docs build.

## 4. Metrics and thresholds

- [ ] 4.1 Implement per-second HDR histograms per file and metric, plus RPS, error classes, phase timings and bytes, merged at the end. Verify: unit tests against known latency distributions (percentile error below 0.1 %).
- [ ] 4.2 Implement the threshold parser and end-of-run evaluation with exit code 1 on breach. Verify: table-driven parser tests, plus an end-to-end test of breached p95 (exit 1) and passing `error_rate` (exit 0).
- [ ] 4.3 Implement abort-on-fail `!` thresholds (10s warm-up, checked each second, stop within 2s). Verify: an error-storm test asserts an early stop and the `aborted: threshold` marker.
- [ ] 4.4 Implement Ctrl+C handling (drain 10s, second Ctrl+C cancels, partial report, exit 130). Verify: an integration test sends SIGINT and asserts report contents and the exit code.
- [ ] 4.5 Add generator-lag measurement and the "generator-bound" warning. Verify: a test with an artificially throttled runtime triggers the warning.

## 5. Reports and redaction

- [ ] 5.1 Add `kind: bench` to the `rustman.report/v1` types (additive) with profile, metrics, time series, thresholds and error samples (max 20 per class). Verify: the report validates against `rustman report schema`, and older readers accept it.
- [ ] 5.2 Add the offline HTML bench template (latency-over-time and RPS charts). Verify: a snapshot test, plus a check that the HTML has no external URLs.
- [ ] 5.3 Apply redaction to output, samples, reports and viz events, including `--secret-columns` and auto-marked `password`/`token` columns. Verify: a test greps all artifacts for a planted secret.

## 6. Live dashboards

- [ ] 6.1 Implement the TUI/TTY live dashboard (time, VUs, RPS, percentiles, error rate, sparklines, threshold status, per-file table, 4 Hz max) and the non-TTY 10-second summary lines. Verify: ratatui `TestBackend` snapshots, plus a non-TTY output test.
- [ ] 6.2 Implement `--viz` streaming (`bench_tick`, `bench_finished` never dropped) and the missing-`bench`-view warning. Verify: a test with a stub companion advertising and omitting the view, asserting the bench result is unchanged.
- [ ] 6.3 Add the bench dashboard view to `rustman-viz` (behind feature `raven`) and advertise `bench` in `hello_ack`. Verify: a companion handshake test plus a headless render smoke test on the lavapipe CI job from `add-raven-visualizer`.

## 7. Integration

- [ ] 7.1 End-to-end: a ramping bench of a two-file collection against a local test API with thresholds, JSON and HTML reports, and `--viz` with a stub companion. Verify: the CI job passes on Linux, macOS and Windows.
