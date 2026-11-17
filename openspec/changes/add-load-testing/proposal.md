# Proposal

## Why

Teams that already have Rustman request files still reach for k6, JMeter or Locust to answer "how does this endpoint behave under load?" That means rewriting the same requests in another tool and language. Reusing request files, environments and assertions for quick, safe load tests closes that gap. Live charts make it far more useful than a wall of numbers.

## What Changes

- New `rustman bench <request-file|collection>` command that reuses request files, environments, variables, captures and assertions.
- Executors:
  - `constant-vus`
  - `ramping-vus` (stages)
  - `constant-arrival-rate` (an open model, with coordinated-omission-aware latency)
  - `iterations`
  - Duration or iteration limits and think time.
- Per-virtual-user data from CSV or JSON files.
- Metrics: latency min/avg/p50/p90/p95/p99/max from HDR histograms, RPS, errors by class, per-phase timings, bytes sent and received.
- Thresholds (`p95<300ms`, `error_rate<1%`, …) that fail the command with exit code 1 when breached.
- A live TUI dashboard, plus live streaming to the viz companion via the `rustman-viz/1` protocol.
- Reports in `rustman.report/v1` with a new additive `kind: bench`, rendered as JSON and HTML.
- Safety: always under the safety policy (host allowlist pinned to `--target`, method gating, production guard). Default caps of 50 VUs, 5 minutes and 200 rps that only `rustman.toml [bench]` can raise. Agents can't start a bench.
- Graceful stop on Ctrl+C with a partial report.

Out of scope: distributed or multi-machine load generation, scripted (JavaScript or Lua) scenarios, browser-level load, gRPC/WebSocket/messaging load (those protocols may plug in later through the request-kind registry), and cloud result storage.

## Capabilities

### New Capabilities
- `load-testing`: the bench command, executors and load profiles, data feeding, metrics and thresholds, safety caps, live dashboards and bench reports.

### Modified Capabilities
None. It reuses:
- `http-requests` for execution and timings;
- `assertions` and captures;
- the `api-exploration` safety policy and production guard;
- the `test-reports` schema (adding `kind: bench` additively, as its versioning rules allow);
- the `visual-dashboards` protocol;
- `tui`.

## Impact

- New crate `rustman-bench` (tokio, `hdrhistogram`, `csv`).
- New CLI subcommand `rustman bench` and a new `[bench]` section in `rustman.toml`.
- `rustman-report` gains the `bench` report kind and its HTML template.
- `rustman-viz` gains a bench dashboard view, advertised in its `hello_ack` views list.
