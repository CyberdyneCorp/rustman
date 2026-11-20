# Stress testing

> **Only stress systems you own or are explicitly authorized to test.** A stress run pushes a backend until it fails, on purpose. Rustman only sends load to targets listed in `rustman.toml`, refuses production unless you opt in twice, and stops at its caps, but the consent is yours.

`rustman stress` answers the question [load testing](bench.md) does not: where does this backend break, how does it break, and does it recover? It runs the same request files and collections as `rustman run` and `rustman bench`, with the same environments, variables, captures and assertions, over the open-model (`constant-arrival-rate`) executor of `rustman bench`.

```text
rustman stress breakpoint api/catalog --target https://staging.example.com --slo "p95<300ms" --slo "error_rate<1%"
```

## Modes

| Mode | What it does | Flags |
|------|--------------|-------|
| `breakpoint` | Steps the arrival rate up until an SLO breaks, then narrows down the last sustainable rate | `--start-rps` (10), `--step-factor` (1.5), `--step-duration` (30s), `--refine` (3), `--require-rps` |
| `spike` | A baseline rate, a sudden multiple of it, then the baseline again; reports the errors during the spike and the recovery time | `--baseline-rps` (10), `--baseline` (30s), `--spike-factor` (10), `--spike-duration` (30s) |
| `soak` | A steady rate for a long time; fits a trend to latency and errors to spot leaks | `--rps`, `--duration`, `--drift-threshold` (5%) |
| `fuzz` | A steady rate with a share of mutated requests | `--rps`, `--duration` (1m), `--fuzz-ratio` (0.2) |

Rates count iterations per second; an iteration sends every request of the plan, so with three request files `--start-rps 10` offers 30 requests per second. Results (`max_sustainable_rps`, `breaking_rps`, step rates) are in requests per second.

An unknown mode exits 2 and lists the valid ones. Every mode can mix in [fuzzed requests](#fuzz-mixing) and [malformed payloads](#malformed-payloads), and send [wire faults](#wire-faults) after its load; all three are checked by the [stress gate](#safety) first.

## Configuration

Everything that grants consent lives in `rustman.toml`, where reviewers see it:

```toml
[stress]
targets = ["https://staging.example.com"]   # the only origins a stress run may target
allow_production = []                       # production hosts, also confirmed with --production
families = []                               # costly families: compression-bomb, large-body
max_rps = 500                               # requests per second
max_duration = "10m"                        # breakpoint, spike and fuzz runs
max_soak_duration = "2h"                    # soak runs
max_body = "10MiB"                          # malformed payloads
max_bomb_ratio = 100                        # compression bombs expand to at most max_bomb_ratio × max_body
max_wire_connections = 20                   # wire faults
max_vus = 1000                              # VUs at once (--max-vus asks for fewer)
expected_max_body = "1MiB"                  # a 2xx to a larger body is a finding

[stress.slo]                                # SLOs of every stress run of the workspace
p95 = "<300ms"
error_rate = "<1%"
```

Durations take `ms`, `s`, `m` and `h`; sizes take bytes or `KiB`, `MiB` and `GiB`. Unlike the rest of `rustman.toml`, an unknown key in `[stress]` (such as a misspelt `max_rsp`) is an error, not a warning: a typo must not silently fall back to a default in the table that holds consent.

## Safety

The [safety policy](safety.md) of `rustman explore` and `rustman bench` applies first, pinned to `--target` as for `rustman bench`: no host besides the target, method gating (`--allow-mutations`, `--allow-destructive`), and the production guard (`--production <host>`). Every request is checked before any load. Then the stress gate adds its own refusals, all exiting 4 before anything is sent:

| Refusal | Message |
|---------|---------|
| The target is not in `[stress] targets` (no flag adds one) | `stress: target not listed in rustman.toml [stress].targets` |
| A production target whose host is not in `[stress] allow_production` | `stress: production host <host> is not listed in rustman.toml [stress].allow_production` |
| Fuzzing, malformed payloads or wire faults against production, whatever the opt-ins | `stress: fuzzing and malformed input are never sent to production` |
| `--malformed compression-bomb` without it in `[stress] families` | names `rustman.toml [stress].families` |
| `--wire` without `--allow-wire-faults` | `stress: --wire sends raw, invalid HTTP and needs --allow-wire-faults` |
| Requests or wire faults through a proxy the safety policy does not allow | names the proxy: `proxy not allowed (<host>)` |

The caps are checked next and exit 2 naming the key, such as `stress: a duration of 4h exceeds max_soak_duration = 2h (raise it in rustman.toml [stress])`. A breakpoint `--step-duration` longer than `max_duration` is refused the same way, since the run would end inside its first step. Flags only ask for less.

Every request goes through the same governed sender as exploration and `rustman bench`, with a governor built from the `[stress]` caps (`max_rps` as its rate, the VU pool as its concurrency). The governor lets a tenth of a second of `max_rps` (at least 2 requests) go out back to back, so a step offered at exactly the cap is not queued client-side by scheduler jitter; the average never exceeds `max_rps`. Unlike exploration, it never slows down on `429 Too Many Requests`: throttling is what a stress run measures. Agents cannot start a stress run: the MCP server has no stress tool.

## SLOs and windows

SLOs use the metrics of `rustman bench` thresholds: `p50`, `p90`, `p95`, `p99`, `max`, `avg`, `error_rate`, `rps` and `checks`. Give them with `--slo` (repeatable) or `[stress.slo]`; scope one to a request file with `file:<name> <expr>`, such as `--slo "file:checkout p99<800ms"`. Without any SLO, `breakpoint` and `spike` use `p95<1s` and `error_rate<5%`.

SLOs are judged over 5-second windows. Latency counts from each iteration's scheduled start (coordinated-omission correction), so queuing in the target shows up in the percentiles.

## Breakpoint

Steps start at `--start-rps` and multiply by `--step-factor`: with the defaults, 10, 15, 22.5, 33.75, 50.6, 75.9 req/s and so on, 30 s each. The first window of a step is a warm-up and is not judged. A step **breaks** when SLOs fail in 2 consecutive windows (one bad window is not a break), and ends there. After the first broken step, `--refine` bisection steps run between the last good and the first broken rate, which brings the two within about 10%:

```text
step  offered   achieved  p95        errors   verdict
   1  10        10        14ms       0.00%    ok
   ...
   5  50.62     50.6      182ms      0.00%    ok
   6  75.94     68.2      1420ms     0.00%    broken
   7  63.28     61.9      1105ms     0.00%    broken
   8  56.95     56.9      260ms      0.00%    ok
   9  60.12     59.8      1012ms     0.00%    broken
max_sustainable_rps: 56.95
breaking_rps: 60.12 (latency; first failing endpoint: search, p95 1890ms, errors 0.00%)
```

- **Failure mode**: each broken window is classified by its dominant symptom: `server-errors` (5xx), `timeouts`, `connection` (refused, reset, TLS), `throttled` (429, or 503 with `Retry-After`) when an error SLO failed; otherwise `saturation` (achieved below 90% of offered while latency is up) or `latency`.
- **Rate limiting** is not a break: throttled responses are not errors for the SLOs, and the run reports `rate limited at <rps> req/s` and climbs on, unless `--treat-throttling-as-failure` makes them errors.
- **First failing endpoint**: for a broken step, the request file whose latency or error rate crossed its SLO first, from the per-file windows, with its p95 and error rate.
- **Cool-down**: after a broken step, nothing is offered until a window closes with no request in flight (at most 6 windows), so the late answers of the broken step are not measured in the next step. Those windows show in the series at 0 req/s and belong to no step.
- **The cap**: the rate never exceeds `max_rps`. A step at the cap that holds ends the search with `no break found up to max_rps = 500` and `max_sustainable_rps` at the cap. The whole run also stops at `max_duration`.

A broken breakpoint is a result, not a failure: the run exits 0 unless `--require-rps` is given and `max_sustainable_rps` falls short of it, or a finding reaches `--fail-on`.

## Spike

The baseline rate runs for `--baseline`, then `--spike-factor` times it for `--spike-duration`, then the baseline again for another `--baseline`. The summary gives the baseline p95, the requests and errors during the spike, the peak p95 and the recovery time: how long after the spike the per-second p95 came back within 20% of the baseline p95 (or within 10 ms of it, for very fast targets) and stayed there for 3 seconds, without errors.

```text
baseline p95 31ms; during the spike 6000 requests, 42 errors; peak p95 2210ms
recovered in 14s
```

## Soak

`--rps` is held for `--duration` (up to `max_soak_duration`). A linear trend is fitted to the p95 and error rate of each minute. A p95 slope above `--drift-threshold` (default 5% of the median p95 of the first 10 minutes, per hour) is the `medium` finding `latency-drift`, with the slope in ms per hour; an error rate rising by more than one point per hour is the `medium` finding `error-drift`. Both hint at leaks: memory, connections, queues. With fewer than three minutes of data, no trend is fitted: the summary says `p95 trend not available: too little data` instead of a slope.

## Fuzz mixing

`--fuzz-ratio r` (`0 < r <= 1`; the `fuzz` mode defaults it to 0.2) replaces each request with a mutated variant with probability `r`. Requests are replaced, not added: offered load stays the same, so valid traffic can be compared with and without fuzzing. Fuzzed exchanges are measured apart: the step table, the SLO windows and the bench metrics count valid traffic only, and the summary shows fuzzed traffic on a line of its own.

```text
rustman stress fuzz api/orders --target https://staging.example.com --rps 20 --duration 5m --allow-mutations --seed 42
```

- **Where variants come from.** Without `--spec`, one mutation of the saved JSON body per variant: a type flipped (`2` becomes `"2"`), a value nulled, a key removed, a number at the 64-bit boundary (`9223372036854775808`), a 10 000-character string, unicode edge cases (NUL, a byte order mark, a right-to-left override), a value wrapped in an array, or a key duplicated. With `--spec openapi.yaml`, a request whose method and path match an operation gets variants that break exactly one constraint of its body schema (a required field missing, a wrong type, a bound, a length, a pattern, an enum), drawn with the seeded generators of `rustman explore`. A request without a JSON body is sent as is. When no request was fuzzed (a plan without a JSON body to mutate, such as `GET`s only), the summary prints `fuzz: warning: no request was fuzzed` instead of the share replaced, and `fuzz-degrades-valid-traffic` is not judged: a drift then is not fuzzing's doing.
- **Reproducible.** Every run has a seed, printed at start (`stress: seed 42`) and in the summary; `--seed` replays it. A slot is one request of one iteration of the fuzz phase, and the same seed and plan fuzz the same slots with the same variants. Outcomes may still differ: servers are not deterministic.
- **Baseline.** A run with fuzzing first runs a clean phase at the mode's first rate for `--baseline` (default 30s, rounded up to whole windows; it counts toward `max_duration`). If valid traffic at that rate is then more than 50% slower at p95 (and more than 10 ms slower, so a fast target's jitter does not count), or its error rate more than 1 point higher, and at least one request was fuzzed, the run reports the `high` finding `fuzz-degrades-valid-traffic` with both values: bad clients hurt good ones.
- **Findings.** A 5xx answer to a variant is the `high` finding `server-error-on-malformed-input`, and no answer in time is `timeout-on-malformed-input`. A 2xx answer to a variant that breaks the schema is `accepted-malformed-input`, and to one over a documented `maxLength` or `maxItems` (or over `[stress] expected_max_body`) is `oversized-input-accepted`. Each finding names its source (`mutation`, `schema` or the malformed family) and keeps the first exchange as evidence, redacted: the request with the `Content-Type` the variant was sent with, the body sent, the status and the start of the response body (at most 4 KiB). Without a response, the evidence body says what happened instead (such as a timeout).

## Malformed payloads

`--malformed <families>` adds raw-byte payloads to the fuzz pool (with `--fuzz-ratio` 0.2 unless given). They replace the request's body on the normal send path, so the request's headers, auth, cookies and timing apply as usual.

| Family | Payload | Bounded by |
|--------|---------|------------|
| `json-syntax` | the body cut in half, a trailing comma, unquoted keys, single quotes, an unterminated string, `NaN` | |
| `deep-nesting` | arrays nested 16, 128, 1024, 5000 or 10 000 deep | depth 10 000, `max_body` |
| `large-body` | a JSON string of 64 KiB up to `max_body` | 1 MiB unless `large-body` is in `[stress] families` |
| `wide` | an object of 1000, 10 000 or 100 000 keys | 100 000 keys, `max_body` |
| `encoding` | invalid UTF-8, a raw NUL, an overlong encoding, a lone surrogate | |
| `content-type-mismatch` | JSON sent as XML, XML or a form sent as JSON | |
| `duplicate-keys` | the body's first key repeated with another value | |
| `numbers` | `1e400`, a 40-digit integer, `-0`, `1e-400` | |
| `compression-bomb` | gzip with `Content-Encoding: gzip` | expands to at most `max_bomb_ratio × max_body` (64 MiB at most); needs `[stress] families` |

A 5xx answer names the family and, for families that grow, the smallest input that failed, such as `smallest failing depth: 5000`. A 2xx answer to `json-syntax`, `encoding`, `content-type-mismatch`, `duplicate-keys` or `numbers` is `accepted-malformed-input`; the well-formed families (`deep-nesting`, `large-body`, `wide`, `compression-bomb`) are judged by size only, against `expected_max_body`.

## Wire faults

`--wire <families> --allow-wire-faults` sends raw HTTP/1.1 that a correct client never sends, after the load phase (not after Ctrl+C). It opens its connections with the engine's connector (DNS, connect timeout, `ca_bundle`, client certificates, TLS with ALPN `http/1.1`); proxies are always tunnelled, never asked to forward, and a proxy outside the safety policy is refused (exit 4). At most `max_wire_connections` (default 20) sockets are open at once, whatever the mode. Only the status line and the connection state of an answer are read, within 10 s.

| Family | Sent | Finding |
|--------|------|---------|
| `oversized-headers` | a 70 KiB header | `connection-dropped-without-response` (low) when the server closes without answering; 431 expected |
| `many-headers` | 1000 headers | the same |
| `cl-te-conflict` | `Content-Length` and `Transfer-Encoding: chunked`, with a body that is complete only by `Content-Length` | `ambiguous-framing-accepted` (high) on a 2xx: the server framed the body by `Content-Length` (a request-smuggling desync); 400 expected |
| `invalid-chunked` | a chunk size that is not hexadecimal | `ambiguous-framing-accepted` on a 2xx |
| `invalid-request-line` | a garbage request line | `ambiguous-framing-accepted` on a 2xx |
| `slow-body` | `max_wire_connections` connections sending a body at 1 byte per second, for 60 s at most | `no-slow-client-timeout` (low) when the server keeps every one open for the whole 60 s |

Each family except `slow-body` sends 3 requests. A server that times slow clients out (with a 408 or a close) before the 60 s are up gets no finding. Wire faults are never sent to production, and agents cannot send them.

## The client as the bottleneck

- Before load starts, the open-file limit is read (on Unix) and a warning is printed when it is below `max_rps × 2`: every connection is a file descriptor.
- During the run, the scheduler measures how late it dispatches each iteration. When it lags more than 100 ms behind schedule for 3 consecutive seconds, the windows holding those seconds are marked `client-saturated`: they never count as server breaks, a breakpoint step made of them never sets `breaking_rps`, and the search stops there.
- Each VU is a task of its own on a pool of worker threads (one per core, 2 to 16), so load does not share one thread. The VU pool grows on demand up to `--max-vus` (default: `max_rps`; never above the `max_vus` cap, default 1000, so in-flight memory stays bounded); a scheduled iteration with no VU free is dropped, which shows as achieved below offered.

## Kill switch and recovery

While load runs, the **health request** goes out once per second: `--health <file>`, or by default the first `GET` request of the plan. The target is declared **down**, and load stops at once (requests in flight are cancelled), when for 5 s connection errors (refused, reset, TLS) exceed half of the attempts, or timeouts do, or when the health request gets no answer 3 times in a row. The summary then says `killed: target down at <rps> req/s (<reason>)`; a breakpoint run's step breaks there with the failure mode `connection` (or `timeouts`).

After every mode, and after a kill, the recovery check sends the health request once per second. The target has **recovered** after 10 consecutive 2xx or 3xx answers whose latency is within 20% of the pre-stress baseline (the median of 3 health requests sent just before load; a 20 ms floor keeps a fast local target's jitter from counting against it). Without recovery within `--recovery-timeout` (default 2m), the run reports the `high` finding `no-recovery`, with the last status and latency:

```text
recovery: not recovered (no-recovery)
findings:
  high no-recovery: api/health.rman.yaml had not recovered 2m after load stopped: the last health request answered 503 in 12ms
```

The check is skipped (`recovery: not checked (...)`) when the plan has no `GET` and no `--health` is given, or when the health request already failed before load: recovery cannot be judged against a target that was never healthy. The health request goes through the same safety policy, with a governor of its own built from the `[stress]` caps.

## Minimization

Once load has stopped and the target has recovered (or recovery was not checked), the input behind each crash-like finding (a 5xx or a timeout caused by a variant) is replayed alone, at 2 requests per second and at most 50 replays per finding, and reduced by delta debugging while the failure still reproduces: keys and items are removed, nesting is unwrapped and strings are halved. The smallest input is shown under the finding and kept in the report:

```text
  high server-error-on-malformed-input: mutation input on api/orders.rman.yaml answered 500 (12 times)
    minimized (6 replays): {"qty":"2"}
```

A finding whose input does not fail when replayed alone is marked `load-dependent`: it only fails under load, which is itself a useful signal. Inputs that are not JSON (or larger than 64 KiB) are replayed once, not reduced. Minimized inputs are redacted like every other output. Ctrl+C stops minimization: after a first Ctrl+C (during load or during the recovery check) no replay is sent, and a minimization it interrupts is dropped rather than reported half done.

## Reports and the live view

- **On a terminal** the run shows a dashboard, redrawn at most 4 times a second: the mode, phase and step; offered and achieved rate; p50, p95 and p99 of valid and fuzzed traffic apart; error classes and the SLO status of the last windows; the kill-switch state and findings as they arrive. Drawing never holds up load generation.
- **Off a terminal** (CI) it prints one line per step instead: `step 3: offered 22.5 req/s, achieved 22.4 req/s, p95 31ms, errors 0.00%: ok`. A step is a breakpoint step; in the other modes it is a stretch at one rate (a spike's phases), at most a minute long.
- **The report** (`rustman.report/v1`, `kind: stress`) is always kept as `.rustman/stress/<id>/report.json` (the run prints its id), and `--report <format>[=<path>]` also writes it as `json`, `html`, `markdown`, `junit` or `sarif`. It holds the mode, profile, seed, target and SLOs, the per-second series, the step table, `breakpoint` (`max_sustainable_rps`, `breaking_rps`, `failure_mode`, `first_failing_endpoint`), `rate_limited_at`, `killed`, `recovery` and the findings in the exploration finding format, so `rustman report render`, SARIF upload and `rustman report diff` treat them like exploration findings (a new `no-recovery` in the later report exits 1). The HTML page is self-contained and offline: latency and errors against offered load with the breakpoint marked, and the recovery timeline.
- **`--viz`** streams per-second points to the viz companion's `stress` view; a companion without it is a warning (`viz companion has no stress view`), shown once, and never changes the result.
- **Agents** read stress reports through the report tools (`get_report` with a report file), but no MCP tool starts a stress run or sends malformed or wire-fault traffic.

## Regression export

`rustman stress export <id> --findings <ids|all> --into <dir>` writes one file per finding from the kept report:

```text
rustman stress export str-20261120-3fa9c1 --findings all --into regressions/stress
```

A finding with an input becomes a request file: the finding's own request file (its URL, headers and auth, as templates) with the minimized input as its body and the assertion a correct API passes, `status: "4xx"`. Secret placeholders in an input become `{{name}}` references again, so no export holds a secret. `rustman run regressions/stress` then fails while the bug is there and passes once it is fixed. Wire faults cannot be written as request files: they are exported as `.wire.txt` files of comments with the request line and the expected status (`431` for oversized headers, `400` for ambiguous framing). Findings without an input (`no-recovery`, drift, `fuzz-degrades-valid-traffic`) are skipped by `all` and refused by id.

## CI recipe

A capacity gate for a staging deployment: fail the job when the API no longer sustains 100 requests per second within its SLOs, or when a high finding appears, and keep the reports.

```yaml
- name: Capacity gate
  run: rustman stress breakpoint api/catalog --target https://staging.example.com --env staging --slo "p95<300ms" --slo "error_rate<1%" --require-rps 100 --report html=out/stress.html --report sarif=out/stress.sarif
- uses: actions/upload-artifact@v4
  if: always()
  with:
    name: stress-report
    path: out/
```

The output is one line per step, and the exit code is 1 when `max_sustainable_rps` is below 100. Compare two runs with `rustman report diff old.json new.json` (`--report json=out/stress.json`).

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | The run completed with no finding at or above `--fail-on` (default `high`), and `max_sustainable_rps` reached `--require-rps` when given. |
| 1 | A finding at or above `--fail-on`, or `max_sustainable_rps` below `--require-rps`. |
| 2 | A usage error or a cap. |
| 4 | A refusal of the safety policy or the stress gate; nothing was sent. |
| 130 | Ctrl+C: load stops at once and the recovery check still runs (a second Ctrl+C cancels requests in flight and the check). |
