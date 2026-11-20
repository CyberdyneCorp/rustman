# Design

## Context

This change builds on three existing pieces:
- `load-testing` provides executors (`constant-vus`, `ramping-vus`, `constant-arrival-rate`), HDR metrics with coordinated-omission correction, thresholds, caps and live dashboards, all over the shared request executor.
- `api-exploration` provides the safety policy (host allowlist, method gating, production guard), seeded schema-aware fuzz generators, findings with severities, and exporting findings as regression requests.

Stress testing combines them. It adds an adaptive load controller, input mutation, raw wire faults, and a recovery monitor. See proposal.md for the motivation.

## Building on existing code

- **Ordering**: `rustman-bench` and its executors come from `add-load-testing`, which is not implemented yet; this change lands after it and reuses its `Runner`-per-VU execution, `[bench]`-style caps, HDR metrics and live dashboard (see that design's "Building on existing code").
- **Safety**: `SafetyPolicy`, `Permission`, `Blocked` and the production guard (`crates/rustman-core/src/safety/`), sent through `rustman_http::GovernedSender` (`crates/rustman-http/src/governed.rs`) via `RunOptions::gate`. `rustman_core::safety::Governor` halves its rate on every `429` (`Governor::observe`) and defaults to the explorer's `[safety]` limits (10 rps, concurrency 4, 2000 probes, 15 min), so stress builds its own `Limits` from `[stress]` and must not let the backoff hide `throttled` windows.
- **Fuzz generators**: `rustman_explore::explore::fuzz` (`proptest` strategies from JSON Schema, `Mode::Valid`/`Mode::Boundary`, seeded ChaCha `TestRng`) and `rustman_explore::synth` (`Synthesizer`, seeded SplitMix64 picks in `synth/vary.rs`). The new schema-less `mutate` module sits beside them and shares that seeding.
- **Findings, export and diff**: the exploration finding format is `crates/rustman-report/src/exploration.rs` (`ReportFinding`, `Severity`); `report diff` (`crates/rustman-report/src/diff.rs`) matches findings by rule id and operation for exploration reports only today; regression files are written by `crates/rustman-explore/src/explore/repro.rs` and `explore/export.rs` (`rustman explore export`).
- **Raw connections**: `rustman-http` already opens TCP, proxy tunnels and TLS with a pinned ALPN for WebSocket handshakes (the private `connect` in `crates/rustman-http/src/websocket.rs`, over `connect::Connector` and `TlsSettings::client_config`). Exposing that as a public "raw stream" helper gives the wire writer the workspace's DNS, connect timeout, `ca_bundle` and client certificates instead of a second `tokio-rustls` setup. `crates/rustman-test-support/src/raw.rs` is a bare hyper client for tests, not a raw writer.
- **Malformed bodies**: the runner has no `body_override`; `ResolvedRequest` bodies are typed (`ResolvedBody`), so raw bytes need a new body variant or an override on the send path. Gzip for `compression-bomb` is available through the workspace `async-compression` (`gzip` feature, as `rustman-http` enables it); `flate2` is only transitive.
- **Config and reports**: `[stress]` must be added to `Settings` (`crates/rustman-core/src/settings.rs`), where unknown keys only warn and the test `unknown_keys_warn_without_failing` uses `[stress]` as its unknown section. `kind: stress` needs a `rustman_report::Report` variant like `kind: bench`. Viz needs `View::Stress` in `crates/rustman-viz-protocol/src/message.rs`.

## Goals / Non-Goals

**Goals:**
- Answer "at what load does it break, how, and does it recover" in one command, with a reproducible result: the same seed and profile give the same plan.
- Never harm a host the user didn't explicitly list, and stop hammering a target that has already fallen over.
- Turn every crash-like finding into a minimal, replayable regression request.

**Non-Goals:**
- Replacing dedicated load generators for multi-machine runs.
- Proving the absence of bugs. Fuzzing here is black-box and bounded.

## Decisions

### D1. One controller over the arrival-rate executor
Every mode is a controller that sets the target rate of `constant-arrival-rate` once per second:
- breakpoint: a staircase;
- spike: baseline, then a step to a multiple, then baseline again;
- soak: flat;
- fuzz: flat.

An open model is required, because closed models hide saturation: the server slows down, VUs wait, and the offered load falls. Coordinated-omission correction stays on, so queuing shows up in the percentiles.
- *Rejected:* a separate stress engine (it would duplicate the executor and metrics), and `ramping-vus` for breakpoints (closed-model latency hides the knee).
- *Implementation note:* the bench's `Open` executor (`crates/rustman-bench/src/open.rs`) runs every VU inside the calling future on a fixed `interval_at` ticker, and its `LagProbe` reports lag per run. Stress needs a rate the controller changes every second (a `watch` channel), each VU as a tokio task of its own on dedicated worker threads for higher concurrency, and lag taken per tick for the client-saturation guard. Its scheduler (`stress/executor.rs`: `Scheduler`, `Pool`, `LagMeter`) is therefore a second open-model scheduler, kept small; the HDR metrics, coordinated-omission correction, thresholds, caps messages, safety gate and reports are the bench's. The `[stress]` governor allows a burst of a tenth of a second of `max_rps` (at least 2), so offering exactly the cap is not throttled below it by scheduler jitter.
- *Breakpoint cool-down:* after a broken step, the next step starts once a window closes with no iteration in flight (at most 6 windows); those windows belong to no step, so the broken step's late answers are not measured in the next one.

### D2. Breakpoint search: geometric steps, then bisection
Steps start at `--start-rps` (default 10) and multiply by `--step-factor` (default 1.5). Each step lasts `--step-duration` (default 30 s) and the first 5 s of each step aren't evaluated. SLOs are evaluated over 5 s windows. A step **breaks** when any SLO fails in 2 consecutive windows. After the first broken step, `--refine <n>` (default 3) bisection steps run between the last good and first broken rate.

The result is `max_sustainable_rps` (the highest good rate) and `breaking_rps`. Geometric steps find the order of magnitude quickly; bisection gets within about 10% in three more steps.
- *Rejected:* linear steps (too slow over several orders of magnitude), and continuous ramps (no steady state, so percentiles mix load levels).

### D3. Failure-mode classification
Each broken window is classified by its dominant symptom:
- `latency` (SLO on percentiles);
- `server-errors` (5xx);
- `timeouts`;
- `connection` (refused, reset, TLS failures);
- `throttled` (429 or 503 with `Retry-After`);
- `saturation` (achieved rate below 90% of offered while latency grows).

`throttled` is reported as **rate limited**, not as broken, unless `--treat-throttling-as-failure` is set. A rate limiter doing its job is a healthy outcome.

### D4. Fuzz mixing replaces requests rather than adding them
With `--fuzz-ratio r`, each request is replaced by a mutated variant with probability `r`, decided by the seeded RNG. Offered load stays the same, so the impact on valid traffic can be compared directly.

Valid and fuzzed exchanges are measured separately. Every mode with fuzzing first runs a clean baseline phase (`--baseline 30s`) at the same rate. `fuzz-degrades-valid-traffic` fires when valid p95 rises more than 50% above the baseline and by more than 10 ms (a floor that keeps a fast target's jitter out), or when the valid error rate rises more than 1 point.

Mutations come from:
- **Schema-aware generation** (`--spec openapi.yaml`): the existing `api-exploration` generators (`proptest` strategies from JSON Schema).
- **Schema-less mutation** (`rustman-explore::mutate`): it walks the saved JSON body and applies one mutation at a time. The mutations are flipping a type, null, removing a key, a boundary number, a long string, unicode edge cases, wrapping in an array, and duplicating a key.

### D5. Malformed families are generated as bytes, with fixed caps
Malformed bodies are raw `Bytes` sent through the normal executor with a `body_override`. That keeps auth, cookies and timing intact.

| Family | Bounded by |
|---|---|
| `json-syntax` | — |
| `deep-nesting` | depth up to 10 000 |
| `large-body` | `max_body`, default 10 MiB |
| `wide` | up to 100 000 keys or items |
| `encoding` (invalid UTF-8, NUL, overlong) | — |
| `content-type-mismatch` | — |
| `duplicate-keys` | — |
| `numbers` (`1e400`, 40-digit ints, `-0`) | — |
| `compression-bomb` (gzip with `Content-Encoding`) | expands to at most `max_bomb_ratio × max_body` |

`compression-bomb` and `large-body` above 1 MiB need opt-in in `[stress].families`, because they cost the target real memory.

### D6. Wire faults use a raw connection
`hyper` correctly refuses to send invalid framing, so wire faults use a small raw writer over a stream from `rustman-http`'s connector (TCP, plus TLS from `TlsSettings` with ALPN pinned to `http/1.1`, as WebSocket handshakes already do). It writes hand-built requests and reads the status line with a 10 s timeout. The writer never parses responses beyond the status line and connection state, which keeps the "never hand-roll HTTP parsing" lesson intact for real traffic.

The VU pool (`--max-vus`, default `max_rps`) is capped by `[stress] max_vus` (default 1000), and the large body-independent malformed payloads (`deep-nesting`, `large-body`, `wide`) are built once per run and shared as `Bytes`, so in-flight body memory does not grow with the VUs.

Wire faults run at a fixed low concurrency (`max_wire_connections`, default 20) regardless of mode. `slow-body` is bounded to 20 connections × 60 s, so it measures the server's slow-client timeouts without becoming a slowloris attack.
- *Rejected:* patching `hyper`, or using a C client such as libcurl (violates the build constraint).

### D7. Kill switch and recovery monitor
A target is considered **down** when any of these holds for 5 s:
- connection errors exceed 50% of attempts;
- timeouts exceed 50%;
- the health request (`--health <request-file>`, default the first `GET` in the plan) fails 3 times in a row.

On down, load stops at once and the report records `killed: target down at <rps>`.

After every mode, and after a kill, the recovery monitor sends the health request once per second. **Recovered** means 10 consecutive successes with latency within 20% of the pre-stress baseline. The monitor gives up after `--recovery-timeout` (default 2 m) and records `no-recovery`. Latency counts as within 20% of the baseline with a 20 ms floor, so a fast local target's jitter does not hold recovery back. A health request that already fails before load cannot judge recovery: the check is then skipped (and so is the kill switch's health rule). The health request, minimization replays and wire faults use a governor of their own built from the `[stress]` caps, so the load's run-time cap does not cut the recovery check short.

### D8. Stress has its own, stricter safety rules
- Targets are listed in `rustman.toml [stress] targets` and can't be added from the CLI.
- Production is refused unless the host is also in `[stress] allow_production` **and** `--production <host>` is given.
- Malformed, wire and fuzz families never run against production.
- Default caps are `max_rps = 500`, `max_duration = 10m`, `max_soak_duration = 2h` and `max_body = 10 MiB`, raised only in `rustman.toml`.
- Method gating follows the safety policy.

The general safety policy owned by `api-exploration` still applies first. These rules only add restrictions.
- *Rejected:* reusing bench caps only. Stress sends malformed data and pushes to failure on purpose, so its consent should live in versioned config that reviewers can see.

### D9. Minimization after load, at a low rate
Crash-like findings (5xx, timeout or reset caused by a mutated input) are minimized after load ends and recovery succeeds. Delta debugging removes keys, shortens strings and reduces nesting while the symptom still reproduces. It is limited to 50 replays per finding, at 2 req/s. A finding that doesn't reproduce in isolation is marked `load-dependent`, which is itself a useful signal.

### D10. Report kind `stress`, owned by `test-reports`
The report reuses the bench time series, the step table, `breakpoint`, `failure_mode`, `first_failing_endpoint`, `recovery`, and findings in the exploration finding format. That means `report diff`, SARIF and regression export work unchanged.

## Risks / Trade-offs

- **Users stressing shared infrastructure.** Targets come from config only, production needs two opt-ins, and the kill switch and caps apply. The README warns that only systems you own or are authorized to test should be stressed.
- **The client becomes the bottleneck.** A pre-flight check reads the file-descriptor limit and warns when it's below `max_rps × 2`. The report flags `client-saturated` when the client's own scheduler lags more than 100 ms, so a client limit isn't mistaken for a server break.
- **Non-deterministic servers.** Seeds make the inputs reproducible, not the outcomes. The report keeps both the seed and the observed results.

## Open Questions

- ~~Wire faults and proxies: an HTTP proxy would reject or normalize broken framing. Send wire faults only directly or through `CONNECT`/SOCKS5 tunnels, and refuse them when the target is reached through a forwarding HTTP proxy?~~ Resolved: the raw stream helper (`rustman_http::open_raw_stream`) always tunnels (`CONNECT` for HTTP proxies, as WebSocket handshakes do; SOCKS5), so no proxy ever forwards wire faults; a proxy the safety policy does not allow is refused (exit 4).
- ~~Malformed bodies: add a raw-bytes `ResolvedBody` variant (visible to history, snippets and redaction) or a send-path override that only stress uses?~~ Resolved: a send-path override (`SendOptions::body_override`, a `BodyOverride` of bytes, `Content-Type` and `Content-Encoding`) that only the stress VU gate sets. Headers, auth, cookies and timing are the request's; nothing else in the workspace can produce such a body. The recorded request keeps its own body; the finding's evidence holds a redacted preview of the variant.
- ~~`rustman.toml` unknown keys warn and never fail today. Should `[stress]` be stricter (an error for unknown keys, since it holds consent), or follow the warning rule?~~ Resolved: stricter. An unknown key in `[stress]` is an error naming the key and its line (as `[bench]` already does), since a misspelt cap must not silently fall back to its default.
