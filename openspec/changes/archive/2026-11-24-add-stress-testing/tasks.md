# Tasks

Implement after `add-load-testing` (this change reuses `rustman-bench`). See design.md "Building on existing code" for the components each task reuses.

## 1. Safety and configuration

- [x] 1.1 Parse `rustman.toml [stress]`: `targets`, `allow_production`, `families`, the caps (`max_rps`, `max_duration`, `max_soak_duration`, `max_body`, `max_bomb_ratio`, `max_wire_connections`, `max_vus`), `expected_max_body` and `slo`, as a section of `Settings` (`crates/rustman-core/src/settings.rs`), replacing `[stress]` as the unknown-section example in `unknown_keys_warn_without_failing`. Verify: config fixture tests, including unknown-key handling (warning or error, per the design open question).
- [x] 1.2 Implement the stress gate: target listed in config, production double opt-in, no fuzz, malformed or wire input against production, costly families opt-in, `--allow-wire-faults`, caps raised only in config. It runs after the shared `SafetyPolicy` (`crates/rustman-core/src/safety/`), and requests go through `GovernedSender` with a `Governor` built from `[stress]` caps, not `[safety]`. Verify: `assert_cmd` tests for each refusal (exit 4 or 2), each asserting the stub server received zero requests.
- [x] 1.3 Add the pre-flight file-descriptor check and warning. Verify: a unit test with an injected limit.

## 2. Controller and modes

- [x] 2.1 Build the mode controller that drives the `constant-arrival-rate` executor's target rate every second. Verify: a unit test with a fake clock checks the rate schedule.
- [x] 2.2 Add the SLO evaluator over 5 s windows, with per-file scoping and defaults. Verify: table tests over synthetic window metrics.
- [x] 2.3 Implement breakpoint: geometric steps, 5 s warm-up, a two-window break rule, bisection refinement, and cap handling. Verify: tests against a stub server whose latency rises above a configured rate assert the staircase rates and the `max_sustainable_rps`/`breaking_rps` bounds.
- [x] 2.4 Implement failure-mode classification (`latency`, `server-errors`, `timeouts`, `connection`, `saturation`, `throttled`), with `--treat-throttling-as-failure`. Verify: one stub behavior per mode.
- [x] 2.5 Identify the first failing endpoint from the per-file time series. Verify: a three-endpoint stub test where `search` degrades first.
- [x] 2.6 Implement spike mode with recovery-time measurement. Verify: a stub with a scripted slowdown asserts `recovered in ~Ns` within ±1 s.
- [x] 2.7 Implement soak mode with linear-trend drift detection (`latency-drift`, `error-drift`). Verify: unit tests on synthetic series, plus a 2-minute accelerated soak against a stub that leaks latency.
- [x] 2.8 Add the client-saturation guard (scheduler lag over 100 ms for 3 s marks windows `client-saturated`). Verify: a test with an artificially throttled scheduler checks that the step never becomes `breaking_rps`.

## 3. Fuzz mixing and mutation

- [x] 3.1 Replace requests by seeded slot selection at `--fuzz-ratio`, and measure valid and fuzzed traffic separately. Verify: a statistical test over 10 000 slots (within ±10%); the same seed gives the same slots.
- [x] 3.2 Add `rustman-explore::mutate` schema-less mutations. Verify: unit tests per mutation, and a property test that each variant differs from the original in exactly one place.
- [x] 3.3 Wire in the schema-aware generators from `api-exploration` (`rustman_explore::explore::fuzz`, seeded `TestRng`) with `--spec`. Verify: a fixture spec produces variants that violate exactly one schema constraint.
- [x] 3.4 Add the baseline phase and the `fuzz-degrades-valid-traffic` rule. Verify: a stub that slows all traffic when it receives malformed input triggers the finding; a well-behaved stub doesn't.

## 4. Malformed and wire families

- [x] 4.1 Implement the malformed byte generators (`json-syntax`, `deep-nesting`, `large-body`, `wide`, `encoding`, `content-type-mismatch`, `duplicate-keys`, `numbers`, `compression-bomb`), each respecting its cap. Verify: unit tests check sizes, depths and bomb ratios never exceed the caps; insta snapshots of small samples.
- [x] 4.2 Send malformed bodies through the normal executor, adding the raw-bytes body path the runner lacks today (a `ResolvedBody` variant or a send-path override, per the design open question), keeping auth and cookies; `compression-bomb` uses the workspace `async-compression` gzip encoder. Verify: the stub sees the auth header on malformed requests.
- [x] 4.3 Add the `accepted-malformed-input` and `oversized-input-accepted` rules. Verify: stubs that accept duplicate keys or oversized bodies trigger the findings.
- [x] 4.4 Expose a raw-stream helper in `rustman-http` (the connector path the private `connect` in `crates/rustman-http/src/websocket.rs` uses: TCP, tunnels, `TlsSettings` with ALPN `http/1.1`) and build the raw wire writer on it, reading only the status line and connection state, with a 10 s timeout. Verify: tests against a hyper server and a deliberately lax raw server.
- [x] 4.5 Implement the wire families (`oversized-headers`, `many-headers`, `slow-body`, `cl-te-conflict`, `invalid-chunked`, `invalid-request-line`) and the connection bound. Verify: the lax server triggers `ambiguous-framing-accepted` and `connection-dropped-without-response`; a hyper server triggers neither; never more than `max_wire_connections` sockets are open (asserted in the test).
- [x] 4.6 Add the `slow-body` bounds (1 byte/s, 60 s) and the `no-slow-client-timeout` rule. Verify: tests with servers that have 15 s and infinite body timeouts.
- [x] 4.7 Add a regression corpus from the Ghostman baseline: CL+TE accepted, garbage request line, a 64 KiB header dropping the connection. Verify: a Python fixture server reproducing the old Ghostman behavior triggers all three findings.

## 5. Kill switch, recovery, minimization

- [x] 5.1 Implement the kill switch (connection and timeout ratios over 5 s, 3 health failures). Verify: a stub that starts refusing connections at a given rate is stopped within 6 s.
- [x] 5.2 Implement the recovery monitor (1 req/s, 10 successes within 20% of baseline, timeout, `no-recovery`). Verify: stubs that recover after N seconds or never.
- [x] 5.3 Implement delta-debugging minimization (at most 50 replays, 2 req/s, `load-dependent` marking). Verify: a stub failing on `qty` being a string minimizes to `{"qty": "2"}`; a load-only failure gets marked.
- [x] 5.4 Implement `rustman stress export`, writing regression request files with the expected-status assertions (reusing `crates/rustman-explore/src/explore/repro.rs` and `explore/export.rs`), and wire faults as comments. Verify: exported files pass `rustman run` against a fixed stub and fail against the buggy one.

## 6. Reports, UI, agents

- [x] 6.1 Add `kind: stress` as a `rustman_report::Report` variant with its `schemars`-generated schema part (`crates/rustman-report/src/schema.rs`), and publish it through `rustman report schema` and `docs/schemas/report.schema.json`. Verify: JSON schema validation tests; an older-reader test where `Report::parse` without the kind reports `unknown report kind 'stress'` (exit 2) without crashing.
- [x] 6.2 Render stress findings through SARIF, Markdown, HTML and `report diff`, extending `crates/rustman-report/src/diff.rs` (exploration-only findings matching today) to stress reports. Verify: insta snapshots; the diff test exits 1 on a new `no-recovery`.
- [x] 6.3 Build the live TUI dashboard (valid and fuzzed split, steps, SLO status, kill-switch state) and the non-TTY step lines. Verify: ratatui `TestBackend` snapshots, and a non-TTY output snapshot.
- [x] 6.4 Write the HTML report (latency and errors against offered load, breakpoint marker, recovery timeline) and the viz `stress` view handshake (`View::Stress` in `rustman-viz-protocol`). Verify: an offline HTML snapshot test (no external URLs); the missing-view warning test.
- [x] 6.5 Make sure the agent interface exposes no stress tool, and that the report tools can read stress reports. Verify: an MCP tool-list test.
- [x] 6.6 Redact evidence, minimized inputs and exports. Verify: an end-to-end test greps every output for a known secret and finds nothing.
- [x] 6.7 Write `docs/stress.md`, with an authorization warning, a CI recipe using `--require-rps`, and a worked breakpoint example. Verify: a doc test parses every command and config example.

## 7. Review fixes

- [x] 7.1 Judge `fuzz-degrades-valid-traffic` only when at least one request was fuzzed, and warn in the summary when none was (a plan without a JSON body). Verify: unit tests on the impact and the summary line; a GET-only fuzz run against the stub.
- [x] 7.2 Keep the evidence exchange as it happened in the report (the body sent, the response status and body), the minimized input in `minimized` only. Verify: a unit test on the report evidence; the stub's response body in a fuzz finding's evidence.
- [x] 7.3 Export regressions with the `Content-Type` the input was sent with, else the request file's own. Verify: unit tests on the evidence headers and the exported file.
- [x] 7.4 Say a soak trend is not available with fewer than three per-minute points. Verify: a summary unit test.
- [x] 7.5 Stop minimization on the first Ctrl+C, also when it lands during the recovery check. Verify: a run interrupted during recovery sends no replay and reports no minimized input.
- [x] 7.6 Make the spike recovery test independent of wall-clock noise. Verify: it checks recovery against the stub's ground truth (never early) and against the run's own series.
