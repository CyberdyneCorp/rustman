# Load testing internals

How `rustman bench` ([Load testing](bench.md)) is built on the rest of Rustman, for contributors and for the changes that extend it (stress testing, thresholds, reports, live dashboards).

## One execution path

A VU is a `rustman_http::runner::Runner` of its own and sends every request through `Runner::send`, the executor of `rustman run`: the same resolution, auth helpers, assertions, captures and redaction. A runner per VU gives each VU its own capture scope, cookie jar and connection pool. Runners are built with `in_workspace: false`, so VUs never write to the workspace, and with `SnapshotMode::Ci`.

Every request goes through `RunOptions::gate`: each VU has a `BenchGate` wrapping a `GovernedSender` (its own connection pool) created with `GovernedSender::with_policy`, so all VUs share one `Governor` built from the `[bench]` caps (`max_rps` as the rate, the VU pool as the concurrency, no probe budget) rather than from `[safety]`.

The crate `rustman-bench` holds everything but the flags and the printing:

| Module | Role |
|--------|------|
| `profile` | `Profile` (one variant per executor), `ProfileSpec` (flags to a profile), stages and think time |
| `caps` | the `[bench]` caps check and the run time |
| `safety` | the pinned policy, the static preflight check, `BenchGate` |
| `data` | `Feeder`: the row of each iteration per `--data-mode` and `--data-exhaust` |
| `metrics` | `Metrics`: per-second windows of HDR histograms and counters, the time series, error samples, `BenchSummary` |
| `threshold` | `Threshold`: parsing `<metric><op><value>[!]` and evaluating it against a `BenchSummary` |
| `lag` | `LagProbe`: the generator's scheduling lag and the generator-bound warning |
| `report` | `bench_report`: the `kind: bench` report of a `BenchOutcome` |
| `summary` | the plain-text end-of-run summary |
| `closed`, `open` | the schedulers of the closed models and the open model |
| `bench` | `Bench::prepare` (checks, nothing sent) and `Bench::run`: the executor, the run controller, the monitor and the lag probe |

A new executor (such as a stress profile) is a new `Profile` variant, its flags in `ProfileSpec`, and a scheduler beside `closed` and `open` that runs `Vu::iteration`.

## Metrics

Each finished request becomes a `Sample` (latency, error class, assertion checks, phase timings, bytes) recorded into the current one-second window: an `hdrhistogram` latency histogram in microseconds (1 µs to 10 minutes, 3 significant digits) per request file plus one overall, with counters and phase sums beside it. Once a second, `Metrics::tick` turns the window into a `BenchSecond` of the time series, adds it into the totals (histograms merge exactly) and resets it, keeping its memory. The summary is the totals plus the current window, so percentiles are within 0.1 % of the exact ones whatever the request count (a unit test checks this on uniform, exponential and bimodal distributions).

Memory is bounded: two histograms (window and totals, about 170 KB each) per request file plus two overall, a time series of one small point per second (capped at 24 hours), and at most 20 error samples per class. No latency sample is kept.

The latency of a request is its transport time (`timings.total`), which excludes the time spent waiting for the rate limiter. In the open model it also includes the wait between the iteration's scheduled start and the start of its first request (coordinated-omission correction). Bytes sent are estimated from the recorded request (request line, headers and text body); bytes received are the response body.

Error samples are described from the request's record (its error, else its first failed assertion, else `METHOD url -> status`), all of which the runner already redacted, and redacted once more with the exchange's own `Redactor` before they are kept. In `METHOD url -> status` the values of query parameters named like a secret (`password`, `token`, `secret`, `api_key`, the names that make a data column secret, `rustman_core::data_rows::secret_by_name`) are also replaced with `«redacted:<name>»`, since a plain `--var` there is not a known secret.

## Run control

`Bench::run` races four futures: the executor, the controller, the monitor and the lag probe. The controller stops new iterations (the `stop` token) at the end of the profile, on a first Ctrl+C (`BenchInput::interrupt`) or on an abort-on-fail breach, then waits a drain period (the graceful stop of 30 s, 10 s after Ctrl+C, 1 s after a breach) before it cancels the iterations still in flight (the `hard` token, also cancelled at once by `BenchInput::cancel`, the second Ctrl+C). The monitor ticks the metrics every second and, after the warm-up (10 s, `Bench::with_abort_warmup` in tests), evaluates the abort-on-fail thresholds against the summary so far. The lag probe yields to the runtime every 20 ms and records how long it waits to be polled again, which does not depend on the timer resolution of the operating system.

The outcome carries the summary, the series, the threshold results, the error samples, `aborted` (`threshold` or `interrupted`) and the warnings; `rustman_bench::report::bench_report` turns it into the report, and the CLI maps it to its exit code.

## Live view

`Bench::live` (taken before `Bench::run`) returns a `rustman_bench::Live`: the shared `Metrics`, the thresholds and the planned duration. `Live::snapshot` copies the totals and the current second under the metrics lock and merges them, computes percentiles and evaluates the thresholds after releasing it, so a VU recording a request waits at most for that copy (a few hundred KB per request file); iteration durations have a lock of their own. One lock for every VU is ample under the default `max_rps` caps; a change raising them by orders of magnitude (add-stress-testing) should shard the current window per VU. The front ends live in `rustman-tui`:

- `bench_view::render` draws a `Snapshot` with ratatui (pure; snapshot-tested on a `TestBackend`);
- `bench_live::spawn` runs a thread that, until `LiveHandle::finish`, draws the dashboard every 250 ms on the alternate screen (no raw mode, so SIGINT still reaches the bench) or prints `live::progress_line` every 10 s, and forwards each newly closed second to the viz stream;
- `bench_viz` opens the stream after the shared companion handshake (`companion::connect`, also used by the TUI's response hand-off), and writes through a bounded queue on a writer thread: ticks are dropped when it is full, `bench_finished` and `close` wait at most 5 s, then the companion is killed.

A stress executor gets all of this for free as long as it records into the same `Metrics`.

## Threading

Every VU runs on the thread that calls `Bench::run`. The executor is one future: `Closed::run` drives all VUs with `join_all`, and `Open::run` keeps the in-flight iterations in a `FuturesUnordered` polled from its own `select!` loop; both are raced against the controller, the monitor and the lag probe in one `tokio::select!`, which the CLI drives with `Runtime::block_on`. Nothing is `tokio::spawn`ed, so request resolution, assertions and metrics recording for every VU share that thread, and the multi-thread runtime's workers only run the I/O the engine hands them. This is ample under the default caps (50 VUs, 200 rps), and the lag probe warns when the generator falls behind. It is the main scaling limit, though: add-stress-testing should spawn each VU (or each open-model iteration) as its own task, with `Arc`-shared `Shared` state instead of borrows, before sharding the metrics lock.

## Executor overhead

`cargo bench -p rustman-bench --bench executor` measures what the VU path adds per request: one GET to a local hyper server sent by the bare `HttpEngine`, against the same request sent as a VU sends it (`Runner::send` through a `BenchGate` and its governor, then classified and recorded in the metrics). It reports both with criterion, then times 3000 interleaved samples of each and fails when the VU path's p50 exceeds the engine's by more than 50 µs (design D1: optimize only beyond that). The failure message names the measured overhead and both p50s. CI runs it in the `bench-overhead` job, so a regression fails the build.

Last recorded result (release build, macOS on Apple silicon, loopback):

| Path | criterion mean | p50 |
|------|----------------|-----|
| `HttpEngine::send` | 55.6 µs | 39 µs |
| VU (`Runner::send` + gate + metrics) | 56.6 µs | 48 µs |
| Overhead | | **9 µs** (limit 50 µs) |
