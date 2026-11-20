# Design

## Context

See proposal.md, Why. The pieces already exist in `bootstrap-core-engine`:
- the HTTP transport with per-phase timings (`http-requests`);
- variable resolution and captures (`environments`, `assertions`);
- the runner's collection-ordered execution (`cli-runner`).

The safety policy and production guard come from `add-openapi-explorer`, and the report schema from `test-reports`. Bench adds load scheduling, statistics and caps on top, without a second execution path.

## Building on existing code

- **Executor**: the runner lives in `crates/rustman-http/src/runner/` (`Runner::new(ctx, options)`, `Runner::run(&plan)`; one request is resolved, sent, asserted and captured in `runner/execute.rs`). A `Runner` owns one `HttpEngine` (connection pools), one cookie jar and one capture/secret scope, so "a VU" maps to a `Runner` of its own, and `--shared-connections` to sharing one `HttpEngine`. `RunOptions` already carries `cancel` (Ctrl+C), `data` (`rustman_core::data_rows`), the GraphQL/JSON-RPC executors and `gate: Option<&dyn SendGate>`, the hook that sends every request through a `GovernedSender`.
- **Safety**: `SafetyPolicy::new(&settings.safety, PolicyOptions { target, permission, allow_hosts, production, environment })` (`crates/rustman-core/src/safety/policy.rs`) is the policy and production guard; `PolicyError::is_refusal` maps to exit 4. `Blocked` displays as `blocked: host not allowed` without the origin, so the bench front end appends `(<host>)`.
- **Rate limiting**: `rustman_core::safety::Governor` (GCRA from the workspace `governor` crate, a concurrency semaphore, probe/time budgets) is what `rustman_http::GovernedSender` enforces. Its defaults are the explorer's (`[safety] rate = 10`, `concurrency = 4`, 2000 probes, 15 min) and it slows down on `429` (`Governor::observe`), so bench builds its own `Limits` from `[bench]` instead of `[safety]`.
- **Config**: `Settings` (`crates/rustman-core/src/settings.rs`) warns `unknown key 'bench' ignored` today; `[bench]` must be added there.
- **Reports**: `rustman_report::Report` is a closed enum and `Report::parse` refuses unknown kinds; the test `unknown_major_versions_and_kinds_are_refused` (`crates/rustman-report/src/report.rs`) uses `bench` as its unknown kind and must change. New kinds get a `schemars`-generated schema part (`crates/rustman-report/src/schema.rs`, as `exploration`), the regenerated `docs/schemas/report.schema.json`, and HTML through the `minijinja` templates in `crates/rustman-report/src/render/templates`. The agent report tools (`crates/rustman-mcp/src/server/tools/reports.rs`) read reports through `Report::parse`, so they read bench reports once the variant exists.
- **Viz**: `rustman-viz-protocol` (`crates/rustman-viz-protocol/src/message.rs`) has `View::{RunDashboard, ExplorationDashboard, ApiGraph, ResponseInspector}`, `ReportKind::{Run, Exploration}` and `Message::is_priority`; bench needs `View::Bench`, `bench_tick`/`bench_finished` messages and `bench_finished` in `is_priority`. No CLI command streams to the companion yet: the only producer is the TUI hand-off in `crates/rustman-tui/src/viz.rs` (companion discovery and handshake), which `--viz` should share rather than copy. The `rustman-viz` companion itself (`add-raven-visualizer`) is not implemented.
- **Dependencies**: `csv`, `governor`, `tokio`, `insta` and `ratatui` are in `[workspace.dependencies]`; `hdrhistogram` and `criterion` are not.

## Goals / Non-Goals

**Goals:**
- Same request semantics as `rustman run`. Only the scheduling differs.
- Correct percentiles (HDR, plus coordinated-omission correction in the open model).
- Safe by default: a user can't accidentally DoS production or another host.

**Non-Goals:**
- Distributed generation. One process can typically drive several thousand rps over HTTP/1.1 keep-alive, which is enough for "quick check" load tests.
- Scripting, and protocols other than HTTP and GraphQL-over-HTTP in v1.

## Decisions

### D1. Reuse the runner's request executor (rejected: a separate lean HTTP path)
Each VU is a tokio task holding its own variable scope and cookie jar, and it calls the same executor `rustman run` uses. A lean path would be faster but would drift from real behavior (auth refresh, captures, assertions). The executor overhead is measured in task 1.3, and we only optimize if it exceeds about 50 µs per request.

### D2. Connection reuse per VU
Each VU owns a connection pool, like a real client, so keep-alive and HTTP/2 behave realistically. `--shared-connections` is a documented option to share one pool across VUs.

### D3. Statistics with `hdrhistogram` (rejected: storing every sample, t-digest)
- **Histograms:** one per (file, metric) per second, merged at the end. Range 1 µs to 10 min, 3 significant digits, which bounds memory regardless of request count.
- **Time series:** per-second snapshots, which feed the TUI, viz and the report.
- **Why not t-digest:** it gives approximate high percentiles. HDR is exact within its precision and standard in load tools.

### D4. Scheduler
- **Closed models (`constant-vus`, `ramping-vus`):** a controller task adjusts the VU set every 100 ms toward the target count, and stopped VUs finish their current iteration.
- **Open model (`constant-arrival-rate`):** a `tokio::time::interval` ticks at the rate and hands scheduled timestamps to a bounded VU pool. If no VU is free, the iteration is counted as `dropped`. Latency is measured from the scheduled timestamp, which is the coordinated-omission correction.
- **Global ceiling:** a `rustman_core::safety::Governor` (on the `governor` crate) built from `[bench]` caps enforces `max_rps` across all executors as a hard limit.

### D5. Caps live in config, not flags
This mirrors the agent-permission principle in `agent-mcp-interface`. Raising load is a reviewed change to `rustman.toml [bench]`, not a flag someone can append in a hurry. Flags may only lower limits.

### D6. Agents excluded entirely (rejected: config opt-in plus elicitation)
Load generation is the most direct way an agent could cause an outage, and the value of agent-triggered load is low. Reading bench reports covers the analysis use case. This can be revisited in its own change.

### D7. Reports and viz
`kind: bench` is an additive extension of `rustman.report/v1`, allowed by `test-reports`' versioning rule. The HTML renderer reuses the offline template with two extra charts. Viz events are `bench_tick` (once per second) and `bench_finished`, and `bench_finished` is never dropped, matching `visual-dashboards`' never-drop rule for final events.

## Risks / Trade-offs

- [The load generator itself is the bottleneck, which skews results] → The dashboard and report show the generator's CPU time and scheduler lag. A warning is printed when lag exceeds 10 ms at p99 ("results may be generator-bound").
- [A user misreads closed-model latency as user-perceived latency] → Percentiles are labeled `service` or `corrected`, and the docs explain open versus closed models.
- [Assertions per request add CPU] → `--no-assertions` skips them and reports `checks` as `n/a`.
- [Data file rows contain secrets] → `--secret-columns` marks them for redaction. The docs warn that columns named `password` and `token` are auto-marked.

## Migration Plan

Additive. `[bench]` defaults apply when absent. There are no changes to existing commands.

## Open Questions

- Whether to support a profile file (`bench.toml`) alongside flags in v1, or only `[bench]` in `rustman.toml`. Resolved for v1: no separate file; ramping stages live in `rustman.toml` as `[[bench.stages]]` (next to the caps, so a ramp is reviewed like a cap) and `--stage` flags replace them.
- `Governor` halves its rate on `429` responses. Should bench keep that backoff (safer for the target, but it silently lowers offered load and skews results) or count `429`s as errors at the configured rate? Resolved for v1: bench keeps the backoff (it sends through `GovernedSender`), counts `429`s as `4xx` errors, and warns in the summary when the rate was halved.
- A `Runner` per VU reuses the whole executor but also re-runs per-`Runner` setup (persisted captures, login, seeding secrets). Is that cost acceptable at 50 VUs, or should `runner/execute.rs` expose a per-request entry point with an explicit VU scope? Resolved: a `Runner` per VU, built with `in_workspace: false` (no persisted captures or cookie jar to load, nothing written), so the per-VU setup is only seeding the redactor; the measured overhead is in `docs/bench-internals.md`.
- Task 6.3 needs the `rustman-viz` companion from `add-raven-visualizer`, which hasn't landed. Ship bench without it (warning `viz companion has no bench view`) and move 6.3 to that change?
