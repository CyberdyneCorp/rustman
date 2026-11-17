# Design

## Context

This change builds on three existing pieces:
- `load-testing` provides executors (`constant-vus`, `ramping-vus`, `constant-arrival-rate`), HDR metrics with coordinated-omission correction, thresholds, caps and live dashboards, all over the shared request executor.
- `api-exploration` provides the safety policy (host allowlist, method gating, production guard), seeded schema-aware fuzz generators, findings with severities, and exporting findings as regression requests.

Stress testing combines them. It adds an adaptive load controller, input mutation, raw wire faults, and a recovery monitor. See proposal.md for the motivation.

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

Valid and fuzzed exchanges are measured separately. Every mode with fuzzing first runs a clean baseline phase (`--baseline 30s`) at the same rate. `fuzz-degrades-valid-traffic` fires when valid p95 rises more than 50% above the baseline, or when the valid error rate rises more than 1 point.

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
`hyper` correctly refuses to send invalid framing, so wire faults use a small raw writer: `tokio::net::TcpStream`, plus `tokio-rustls` for HTTPS with ALPN pinned to `http/1.1`. It writes hand-built requests and reads the status line with a 10 s timeout. The writer never parses responses beyond the status line and connection state, which keeps the "never hand-roll HTTP parsing" lesson intact for real traffic.

Wire faults run at a fixed low concurrency (`max_wire_connections`, default 20) regardless of mode. `slow-body` is bounded to 20 connections × 60 s, so it measures the server's slow-client timeouts without becoming a slowloris attack.
- *Rejected:* patching `hyper`, or using a C client such as libcurl (violates the build constraint).

### D7. Kill switch and recovery monitor
A target is considered **down** when any of these holds for 5 s:
- connection errors exceed 50% of attempts;
- timeouts exceed 50%;
- the health request (`--health <request-file>`, default the first `GET` in the plan) fails 3 times in a row.

On down, load stops at once and the report records `killed: target down at <rps>`.

After every mode, and after a kill, the recovery monitor sends the health request once per second. **Recovered** means 10 consecutive successes with latency within 20% of the pre-stress baseline. The monitor gives up after `--recovery-timeout` (default 2 m) and records `no-recovery`.

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
