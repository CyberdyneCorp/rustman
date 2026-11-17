# Proposal

## Why

`load-testing` answers "does it hold the load I chose?". `api-exploration` answers "does each endpoint handle bad input?". Neither answers the question teams ask before a launch or after an incident: **where does this backend break, how does it break, and does it recover?**

That question needs three things the existing capabilities don't combine:
- **Load that searches for its own limit** instead of a fixed profile.
- **Bad data under load**: malformed input mixed into real traffic, to see whether a bad client degrades service for good clients.
- **Malformed input below the schema level**: broken JSON, deep nesting, huge bodies, bad encodings, and broken HTTP framing.

The last item covers exactly the bugs found while baselining Ghostman: conflicting `Content-Length` and `Transfer-Encoding` accepted, garbage request lines accepted, and oversized headers dropping the connection.

## What Changes

- New `rustman stress` command with four modes. All of them reuse the `load-testing` engine and request files:
  - `breakpoint`: steps up the arrival rate until SLOs break, then narrows down the last sustainable rate.
  - `spike`: baseline, a sudden multiple of it, then back down, measuring recovery time.
  - `soak`: steady load for a long time, detecting latency and error drift that hints at leaks.
  - `fuzz`: a steady rate with a configurable share of mutated requests, comparing valid-traffic latency against a clean baseline.
- **Fuzz mixing** (`--fuzz-ratio`) works in every mode. It uses schema-aware generators from `api-exploration` when an OpenAPI document is given, and schema-less mutations of the saved request body otherwise. Fuzzing is seeded and reproducible.
- **Malformed payload families**: JSON syntax errors, deep nesting, oversized and very wide bodies, encoding errors, content-type mismatches, duplicate keys, extreme numbers and compression bombs. Each is bounded by caps.
- **Wire-fault families**, behind a separate opt-in: oversized and excessive headers, slow bodies, conflicting `Content-Length`/`Transfer-Encoding`, invalid chunked encoding and invalid request lines. They're sent over a raw connection, because `hyper` refuses to produce invalid HTTP.
- **Kill switch and recovery phase**: when the target stops answering, load stops immediately and a low-rate health probe measures time to recovery.
- **Breakage report** (`rustman.report/v1`, `kind: stress`):
  - a step table and the breaking point;
  - the failure mode (latency, 5xx, timeouts, resets, throttling) and the first endpoint to fail;
  - findings with stable rule ids;
  - minimized, replayable inputs for every crash-like finding;
  - export of findings as a regression collection.
- **Visuals**: a live TUI dashboard, a self-contained HTML report, and a Raven "load vs latency/errors" view.

Out of scope:
- Distributed or multi-machine load. One process only, as in `load-testing`.
- Testing hosts the user doesn't control. Targets come only from `rustman.toml`.
- Network-level faults (packet loss, latency injection, TCP resets). Those belong to a proxy such as toxiproxy.
- Coverage-guided fuzzing, which isn't possible black-box.
- Protocols other than HTTP/1.1, HTTP/2 and GraphQL-over-HTTP in v1.
- Agent-triggered stress runs. Agents can only read stress reports.

## Capabilities

### New Capabilities

- `stress-testing`: the `stress` command and its modes, fuzz mixing, malformed and wire-fault families, SLOs and breaking-point detection, kill switch and recovery, stress-specific safety rules, findings, minimization and the breakage report.

### Modified Capabilities

- `test-reports`: adds the report kind `stress`. This is additive, following the extension rule for `rustman.report/v1`.

It also reuses, without changing their requirements:
- the `load-testing` executors and metrics;
- the `api-exploration` safety policy, production guard and fuzz generators;
- `visual-dashboards` through a new `stress` view, negotiated in the handshake like `bench`.

## Impact

- New `stress` module in `rustman-bench` (mode controllers, SLO evaluator, breakpoint search, recovery monitor).
- New `rustman-bench::wire` raw writer on `tokio` and `tokio-rustls`, used only for wire faults.
- New `rustman-explore::mutate` module for schema-less mutations and the malformed families, sharing seeded RNG plumbing with the existing generators.
- New `rustman.toml [stress]` table: allowed targets, caps, and the allowed fault families.
- Report schema: new `kind: stress` and its fields.
- No new heavy dependencies. Gzip bombs use `flate2`, which the HTTP stack already pulls in for decompression.
