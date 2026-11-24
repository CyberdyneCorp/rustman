# Load testing

`rustman bench` runs the request files and collections you already have as load: many virtual users (VUs) repeat them, with the same environments, variables, captures and assertions as [`rustman run`](cli.md#rustman-run). It is meant for quick, safe checks of how an endpoint behaves under load, not for distributed or scripted load generation.

```sh
rustman bench checkout/ --target https://staging.example.com --env staging --vus 5 --duration 30s
```

The behavior is specified in [`openspec/specs/load-testing`](https://github.com/CyberdyneCorp/rustman/blob/main/openspec/specs/load-testing/spec.md). On a terminal a bench draws a live dashboard (see [Live progress](#live-progress)), prints a summary at the end and can write JSON and HTML reports. With `--viz` the [viz companion](viz.md#bench-dashboard) shows it as a live dashboard.

## Iterations and virtual users

An **iteration** runs every selected request once, in collection order (file-name order, or the folder's `_collection.yaml` `order:`). For `checkout/` with `login`, `add-item` and `pay`, each iteration sends `login`, then `add-item`, then `pay`.

Each **VU** is an independent client, like a separate user of the API:

- its own variables and captures: a value one VU captures (a token, an order id) is never seen by another;
- its own cookie jar, which starts empty and is not saved;
- its own connection pool, so keep-alive behaves as it would for real clients.

Captures carry over from one iteration of a VU to the next. Unlike `rustman run`, a bench never writes to the workspace: captures are not persisted (and persisted captures are not read), nothing is recorded in history, and `snapshot` assertions compare only (a missing snapshot fails, as in CI mode).

A bench runs `kind: http` requests only; any other kind is refused before it starts. Requests are sent through the [safety policy](#safety), so OAuth2 tokens are not acquired and Digest auth is refused (as for agents); use a token from a variable or a capture instead.

## Executors

`--executor` picks how iterations are scheduled.

| Executor | Model | Flags |
|----------|-------|-------|
| `constant-vus` (default) | closed | `--vus <n>` (default 1), `--duration <d>` (default `30s`) or `--iterations <n>`, `--think-time` |
| `ramping-vus` | closed | `--stage <duration>:<vus>` (repeatable) or `[[bench.stages]]`, `--think-time` |
| `constant-arrival-rate` | open | `--rate <n>` (required), `--duration <d>` (default `30s`), `--max-vus <n>` (default 50) |

A flag of another executor is a usage error (exit code 2), such as `error: bench: --vus does not apply to the ramping-vus executor`.

### constant-vus

`--vus N` VUs each run iterations back to back until `--duration` is over, or until `--iterations` iterations (in total, across every VU) have started. `--think-time` pauses each VU between two of its iterations: a fixed duration (`500ms`) or a uniform random one in a range (`1s..3s`); the default is `0`.

```sh
rustman bench api/ --target http://localhost:8080 --vus 2 --think-time 1s..3s --duration 1m
rustman bench api/ --target http://localhost:8080 --vus 4 --iterations 200
```

### ramping-vus

The VU count follows the stages, starting from 0: during each `--stage <duration>:<vus>` it moves linearly from the previous stage's target to its own, and the target is adjusted every 100 ms.

```sh
rustman bench api/ --target http://localhost:8080 --executor ramping-vus \
  --stage 30s:20 --stage 1m:20 --stage 30s:0
```

This grows from 0 to 20 VUs over 30 seconds, holds 20 for a minute, and drops to 0 over 30 seconds.

Without `--stage`, the stages come from `[[bench.stages]]` in `rustman.toml`, so a reviewed ramp can live with the workspace; any `--stage` replaces them all:

```toml
[[bench.stages]]
duration = "30s"
target = 20

[[bench.stages]]
duration = "1m"
target = 20

[[bench.stages]]
duration = "30s"
target = 0
```

A VU removed by a ramp-down finishes its current iteration first (graceful stop); an iteration still running 30 seconds after its VU was removed is cancelled and counted as `interrupted`.

### constant-arrival-rate

`--rate R` starts `R` iterations per second whatever the response times, each on a free VU of a pool of `--max-vus`. When every VU of the pool is busy, the iteration is not started: it is counted as **dropped**. With `--rate 100 --max-vus 10` and responses taking one second, about 90 iterations per second are dropped.

```sh
rustman bench api/ --target http://localhost:8080 --executor constant-arrival-rate --rate 50 --max-vus 20 --duration 1m
```

When the duration is over (for every executor), no new iteration starts and those in flight get 30 seconds to finish before they are cancelled.

### Request options

`--timeout <duration>` replaces every request's total timeout (`settings.timeout_ms`); a request that takes longer counts as a `timeout` error. `-k`/`--insecure` skips TLS certificate verification for every request, as for `rustman run`.

## Open and closed models

In a **closed model** (`constant-vus`, `ramping-vus`), a VU starts its next iteration only when the previous one is done. When the server slows down, the VUs send less: load adapts to the server, as a fixed group of users waiting for their pages would. The latency reported is **service** latency, the time each request took.

In the **open model** (`constant-arrival-rate`), iterations arrive at a fixed rate whatever happens, as independent users would. A slow server does not slow the arrivals, so queuing shows up in the latency.

Closed-model latencies hide queuing: during a two-second stall, a VU waits for one slow response and simply sends nothing else, so most of the stall never appears in the percentiles (coordinated omission). In the open model, latency is measured from each iteration's **scheduled** start instead of its actual start, so every iteration scheduled during a stall reports the time it waited. These percentiles are labeled **corrected**. For "what latency do users see at this traffic level", use the open model; for "how fast is one request", either works.

## Data feeding

`--data <file>` feeds each iteration a row of a CSV or JSON file, with its columns (or keys) as variables, read exactly as in [data-driven runs](data-runs.md). `--data-mode` chooses the row:

| Mode | Row of each iteration |
|------|-----------------------|
| `sequential` (default) | The next row of one cursor shared by every VU: each row is used once per pass. |
| `per-vu` | VU 1 always uses row 1, VU 2 row 2, and so on. |
| `random` | A uniformly random row. |

`--data-exhaust` says what happens at the end of the rows: `recycle` (default) starts over; `stop` ends the VU (`sequential`) or leaves VUs without a row of their own idle (`per-vu`). `random` never runs out.

```sh
# users.csv: email,password with 3 rows
rustman bench login/ --target http://localhost:8080 --data users.csv --data-mode per-vu --vus 3
```

Here VU 1 uses row 1, VU 2 row 2 and VU 3 row 3 for `{{email}}` and `{{password}}`.

Values of secret columns are redacted from every output: columns listed with `--secret-columns a,b` or in a `_collection.yaml` `secret_columns`, and columns named like `password`, `token`, `secret` or `api_key`, which are marked automatically.

## Safety

A bench always runs under the [safety policy](safety.md):

- **`--target` is required** (`error: bench: --target is required`, exit code 2), and its origin is the only one requests may go to. `[safety].allowed_hosts` is ignored and there is no `--allow-host`. The target is shown in the report and the dashboards, so it must be a bare origin (`scheme://host[:port]`, an optional trailing `/`): a target holding credentials (`http://user:pass@host`) or with a path, query or fragment (`http://host/v1`, `http://host/?token=...`) is refused with exit code 2, and the message does not repeat it. Put credentials in the request's `auth` and paths in the request files.
- **Proxies**: a proxy is where the load actually goes, so a request whose proxy (its `settings.proxy`, `[http] proxy`, or `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`) is not the target origin is refused like a foreign host (`blocked: proxy not allowed (<proxy host>)`): before any load when it is known statically, else as a `blocked` error when it is sent. Set the request's `proxy: none` (or `NO_PROXY`) to bypass it, or bench the proxy itself as the target.
- Before any load, every request is resolved with the environment, `--var` and the first data row. A request whose origin is not the target's stops the bench with exit code 4 and, for example, `error: api/report.rman.yaml: blocked: host not allowed (other.example.com)`, before anything is sent. The refusal names the bare host, with the port only when it is not the scheme's default (`127.0.0.1:8081`). A URL that depends on a capture is checked when it is sent; a request (or redirect hop) to another origin is then not sent and counts as a `blocked` error.
- **Method gating**: GET, HEAD and OPTIONS run by default; POST, PUT and PATCH need `--allow-mutations`, DELETE `--allow-destructive`. A blocked method stops the bench before any load, with exit code 4.
- **Production guard**: benching a production target (as the [production guard](safety.md#production-guard) defines it) needs `--production <host>` repeating the exact target host, and `--allow-destructive` is always refused against production.
- **Agents can't start a bench**: the [MCP server](agent.md) has no tool that generates load.

### Load caps

A bench is capped at 50 VUs, 5 minutes and 200 requests per second by default. Flags asking for more fail with exit code 2 and name the cap:

```text
error: bench: 100 VUs exceeds max_vus = 50 (raise it in rustman.toml [bench])
```

Only `rustman.toml` raises a cap, so that more load is a reviewed change rather than a flag:

```toml
[bench]
max_vus = 200          # VUs at once (--vus, the largest --stage, --max-vus)
max_duration = "10m"   # --duration, or the sum of the stages; also bounds --iterations runs
max_rps = 500          # --rate × requests per iteration, and a hard limit on requests per second
```

An arrival rate asks for `--rate` iterations per second, each sending every request of the collection, so `--rate 150` over a 2-request collection asks for 300 requests per second and is refused under `max_rps = 200`.

`max_rps` is enforced across every executor by one rate limiter shared by all VUs, with evenly spaced requests and no bursts, so a closed model with fast responses never exceeds it either. When the target answers `429 Too Many Requests`, the limiter halves its rate (and honors `Retry-After`), as for exploration; the summary warns when that happened, since it lowers the offered load. `429` responses also count as `4xx` errors. Requests wait for the limiter in arrival order, so no VU is starved. The wait is in no latency of a closed model, so the summary also warns when the limit, not the target, set the throughput (over one request in ten waited at least half a request interval for it, at a rate near `max_rps`): raise `max_rps` to go beyond. The limiter spaces requests evenly and keeps no credit, so a sender that wakes up late for its slot loses that time: a fast sender stays below `max_rps`, never above it (on macOS the timer wakes a waiting sender about 2 ms late, so a single fast VU under the default 200 rps gets about 150 rps). In the open model the wait counts in the corrected latency, from the scheduled start.

## Summary and errors

At the end, the bench prints a summary on stdout: iterations (completed and interrupted) and how long completed ones took (avg, p95, max; think time excluded), scheduled and dropped iterations (open model), requests per second, the error rate with errors by class, and latency percentiles (min, avg, p50, p90, p95, p99, max) per request file and overall, then the [thresholds](#thresholds) and, when the run stopped early, `aborted: threshold` or `aborted: interrupted`. Latency is the transport time of each request, from HDR histograms with 3 significant digits.

```text
iterations: 120 completed, 0 interrupted
iteration duration (ms): avg 38.40, p95 71.20, max 130.10
requests: 240 (7.9/s), errors 1.25% (5xx 3)
latency (service, ms):
                                 min      avg      p50      p90      p95      p99      max  requests  errors
  checkout/login.rman.yaml      3.10    12.48    11.20    18.90    22.40    35.10    41.70       120  0.00%
  checkout/pay.rman.yaml        5.02    25.91    21.30    44.80    52.10    88.60   102.40       120  2.50% (5xx 3)
  overall                       3.10    19.20    15.90    38.20    46.70    80.30   102.40       240  1.25% (5xx 3)
```

Error classes: `4xx` and `5xx` (by response status, even when the request's assertions pass), `timeout`, `connect` (refused or unreachable connections, DNS, proxy and other network failures), `tls`, `blocked` (refused by the safety policy at runtime), `assertion` (a failed assertion or required capture) and `other` (a request that could not be built, such as an unresolved variable).

## Metrics

Besides the summary, a bench measures, per request file and overall: assertion checks passed and failed, the average time of each phase (`dns`, `connect`, `tls`, `ttfb`, `download`, over the requests that went through it), bytes sent (estimated from the request line, headers and text body) and received (response bodies). Each second also becomes a point of a time series: requests, errors, requests per second, p50/p95/p99 and active VUs (closed model: VUs running or thinking; open model: VUs of the pool busy with an iteration). Requests per second count from the start of the load to its end, over at least one second, so a run shorter than a second shows its request count. The [report](#reports) holds all of it.

Memory stays bounded whatever the number of requests: latencies and iteration durations go into fixed-size histograms, one per second merged into the totals, and no individual request is kept. Each VU still reads each response body as `rustman run` does, up to `[http] max_body_bytes` (10 MiB by default) in memory before spilling to disk, so very large responses with many VUs need up to VUs × that limit; lower `max_body_bytes` for such benches. Secret values a VU resolves or captures (a token from a login request, say) are kept for one iteration only, and the process-wide set that log lines are redacted with keeps at most about 32,000 of the most recent values, so a long bench that logs in on every iteration does not grow either.

## Thresholds

A threshold makes a bench a pass/fail check for CI. `--threshold "<metric><op><value>"` (repeatable) is checked when the bench ends; if any is breached, the bench still writes its reports, then exits with code 1.

```sh
rustman bench api/ --target http://localhost:8080 --vus 10 --duration 1m \
  --threshold "p95<300ms" --threshold "error_rate<1%" --threshold "checks>99%"
```

| Metric | Value | Example |
|--------|-------|---------|
| `p50`, `p90`, `p95`, `p99`, `max`, `avg` | latency, with `us`, `ms`, `s` or `m` (milliseconds when bare) | `p95<300ms`, `p99<=1.5s` |
| `error_rate` | errors over requests, as a percentage or a fraction | `error_rate<1%` |
| `checks` | assertions passed over assertions run | `checks>99%` |
| `rps` | requests per second over the run | `rps>=50` |

Operators are `<`, `<=`, `>` and `>=`. `file:<name>:` scopes a threshold to one request file, named by its path, its path without `.rman.yaml` or its file name: `file:checkout/login:p95<200ms`. A scope that names no file of the bench is a usage error (exit 2). `checks` without any assertion is `n/a` and passes, with a warning in the summary.

The summary shows each threshold with what was measured:

```text
thresholds:
  ✗ p95<300ms (412ms)
  ✓ error_rate<1% (0.20%)
```

Thresholds every bench of the workspace must keep go in `rustman.toml`, each metric with its condition; they are checked before those of `--threshold`:

```toml
[bench.thresholds]
p95 = "<300ms"
error_rate = "<5%!"
"file:checkout/pay:p99" = "<1s"
```

### Abort on fail

A threshold ending in `!` (`error_rate<5%!`) also stops the bench as soon as it is breached: after a 10-second warm-up it is checked every second against everything measured so far, and when it fails, no new iteration starts, those in flight get one second to finish, and the bench writes a partial report marked `aborted: threshold` and exits with code 1. Load stops within 2 seconds of the check.

## Stopping with Ctrl+C

The first Ctrl+C stops starting iterations and waits up to 10 seconds for those in flight (the dashboard says so in its title; without it, a line on stderr does); the second cancels them at once. Either way the bench then prints its summary (ending in `aborted: interrupted`), writes its reports, which cover what ran, and exits with code 130.

## Generator lag

If the machine running the bench is the bottleneck, every latency it measures is inflated. A bench measures its own scheduling lag (how late its tasks run) and, when the 99th percentile is above 10 ms, warns:

```text
warning: the load generator lagged 23.4 ms at p99 (over 10 ms): results may be generator-bound
```

Fewer VUs, a lower rate or a faster machine then give truer numbers. The report keeps the lag as `generator_lag_p99_ms`.

## Live progress

While a bench runs, stdout shows how it goes:

- **On a terminal**, a live dashboard fills the screen: elapsed and remaining time (remaining only for a bench with a fixed duration), active VUs, requests per second, p50/p95/p99 latency (labelled `service` or `corrected`), the error rate, sparklines of p95 and requests per second over the last two minutes, each threshold as it stands so far, and a table per request file with an `overall` row. It redraws at most 4 times a second from a thread of its own, so drawing never slows the load. Ctrl+C works as below; when the bench ends the screen is restored and the summary printed.
- **Otherwise** (stdout piped, redirected or in CI), no dashboard is drawn: one progress line is printed every 10 seconds, and a bench shorter than that prints none.

```text
bench 20s/1m00s: 10 VUs, 412.0 req/s, 8240 requests, p50 18.20ms, p95 61.00ms, p99 120.40ms, errors 0.12%, thresholds 2/2 passing
```

A report written to stdout (`--report json`) never gets the dashboard; the progress lines and the summary then go to stderr.

### Viz companion

`--viz` also streams the bench to the viz companion (`rustman-viz`, found as `RUSTMAN_VIZ`, then on `PATH`, then next to `rustman`) over the [viz protocol](viz-protocol.md): it opens the `bench` view, sends a `bench_tick` per second and a final `bench_finished` with the totals and thresholds. The companion draws them as the [bench dashboard](viz.md#bench-dashboard), and `rustman viz <report.json>` opens a saved bench report in the same view. These events hold counts, timings and threshold expressions only, never request or response content.

Viz never changes a bench: a companion that is missing, refuses the handshake or stops reading gets one warning (`warning: --viz: Viz companion not found: ...`), and the bench runs, reports and exits as without `--viz`. A companion built before the `bench` view gets:

```text
warning: viz companion has no bench view
```

A slow companion misses ticks rather than slowing the bench; `bench_finished` waits for it at most 5 seconds.

## Reports

`--report <format>=<path>` (repeatable) writes a `rustman.report/v1` report with `kind: bench` ([Bench reports](reports.md#bench-reports)): the profile and target, metrics per file and overall, the per-second series, the threshold results and up to 20 error samples per error class.

```sh
rustman bench api/ --target http://localhost:8080 --duration 1m \
  --report json=out/bench.json --report html=out/bench.html
```

| Format | Content |
|--------|---------|
| `json` | The canonical report; validate it with `rustman report schema`, render it later with `rustman report render` |
| `html` | A self-contained page with latency-over-time (p50, p95, p99) and requests-per-second charts, the thresholds, the latency table and the error samples; it loads nothing from the network |
| `markdown` | The same, as tables, for a pull request comment |
| `junit` | One test case per threshold, for CI test tabs |
| `sarif` | One result per breached threshold |

A format without a path goes to stdout, and the summary then goes to stderr. Agents can read a bench report through the `get_report` tool by its path (`out/bench.json`); they cannot start a bench.

## Redaction

Every output of a bench applies the [environment's redaction](environments.md): the summary, the warnings, the progress lines and dashboard, the viz events, the error samples and every report format. Values of secret variables (captures of a secret variable included), of the request's auth, and of secret data columns (`--secret-columns`, a collection's `secret_columns`, and columns named like `password`, `token`, `secret` or `api_key`) appear as `«redacted:<name>»`. An assertion that fails on a password shows `«redacted:password»`, never the password. An error sample that shows the request (`GET <url> -> 500`) also replaces the value of every query parameter named like a secret (`?password=...`), even one set with a plain `--var`.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | The bench ran and every threshold passed. |
| 1 | A threshold failed, or an abort-on-fail threshold stopped the bench (`aborted: threshold`). |
| 2 | Usage error: a missing `--target`, a cap exceeded, a flag of another executor, a request kind other than `http`, an unreadable data file. |
| 4 | Refused by the safety policy before any load: a host outside the target, a method not allowed, an unconfirmed production target. |
| 130 | Interrupted by Ctrl+C: the summary and reports cover what ran, marked `aborted: interrupted`. |

See also [executor internals and overhead](bench-internals.md).
