# load-testing Specification

## Purpose
Lets users load test their APIs by reusing existing request files, environments and assertions, with safe default caps, reproducible profiles, accurate percentiles, thresholds for CI, and live dashboards.

## Requirements

### Requirement: Bench command
`rustman bench <path>... --target <origin>` SHALL run the given request files or collections as virtual-user iterations, using the active environment (`--env`), variables (`--var`), captures and assertions exactly as `rustman run` does. Each VU SHALL execute the files in collection order once per iteration.

#### Scenario: Collection as an iteration
- **GIVEN** a collection `checkout/` with `login`, `add-item`, `pay` in order
- **WHEN** the user runs `rustman bench checkout/ --target https://staging.example.com --vus 5 --duration 30s`
- **THEN** each of 5 VUs SHALL repeatedly execute `login`, `add-item`, `pay` in that order for 30 seconds

### Requirement: Target is mandatory and pins the allowlist
`--target` SHALL be required. The safety policy host allowlist for the bench SHALL be exactly the target's origin. Any request resolving to another origin, including redirects, or routed through a proxy whose origin is not the target's, SHALL be refused before load starts (exit code 4) when known statically, or counted as a `blocked` error at runtime. A refusal of a foreign host SHALL name its bare host, with the port only when it is not the scheme's default.

#### Scenario: Missing target
- **WHEN** the user runs `rustman bench api/`
- **THEN** the command SHALL fail with exit code 2 and `bench: --target is required`

#### Scenario: Request outside the target
- **GIVEN** a request file whose URL resolves to `https://other.example.com/x`
- **WHEN** the user benches it with `--target https://staging.example.com`
- **THEN** the command SHALL exit with code 4 and `blocked: host not allowed (other.example.com)` before sending any request

#### Scenario: Proxy outside the target
- **GIVEN** a request file for the target whose `settings.proxy` is `http://proxy.example.com:3128`
- **WHEN** the user benches it with `--target https://staging.example.com`
- **THEN** the command SHALL exit with code 4 and `blocked: proxy not allowed (proxy.example.com)` before sending any request

### Requirement: Target is a bare origin
The target is shown in the report and the dashboards, so a `--target` holding credentials (userinfo), or with a path other than `/`, a query or a fragment, SHALL be refused with exit code 2 before any request is sent or report written, without echoing it.

#### Scenario: Target with a query
- **WHEN** the user runs `rustman bench api/ --target "https://staging.example.com/?token=abc"`
- **THEN** the command SHALL fail with exit code 2, naming `--target must be an origin`, and `abc` SHALL appear in no output

### Requirement: Method gating
Bench SHALL apply the safety policy's method gating: GET, HEAD and OPTIONS run by default, POST/PUT/PATCH need `--allow-mutations`, and DELETE needs `--allow-destructive`. A blocked method SHALL stop the bench before any load is generated, with exit code 4.

#### Scenario: Mutation without opt-in
- **GIVEN** a collection containing `POST /orders`
- **WHEN** the user benches it without `--allow-mutations`
- **THEN** no request SHALL be sent and the exit code SHALL be 4

### Requirement: Production guard
Benching a production target, as defined by the `api-exploration` production guard, SHALL be refused with exit code 4 unless `--production <host>` repeats the exact target host. Against production, `--allow-destructive` SHALL always be refused.

#### Scenario: Production refused
- **GIVEN** the environment `prod` is selected
- **WHEN** the user runs `rustman bench api/ --env prod --target https://api.example.com`
- **THEN** the command SHALL exit with code 4 and explain that `--production api.example.com` is required

### Requirement: Default load caps
Bench SHALL cap load at 50 VUs, 5 minutes total duration and 200 requests/second by default. Flags requesting more SHALL fail with exit code 2, naming the cap and the `rustman.toml [bench]` key (`max_vus`, `max_duration`, `max_rps`) that raises it. CLI flags SHALL NOT raise caps.

#### Scenario: Over the VU cap
- **WHEN** the user runs `rustman bench api/ --target … --vus 100`
- **THEN** the command SHALL exit with code 2 and `bench: 100 VUs exceeds max_vus = 50 (raise it in rustman.toml [bench])`

#### Scenario: Raised cap
- **GIVEN** `rustman.toml` contains `[bench] max_vus = 200`
- **WHEN** the user runs the bench with `--vus 100`
- **THEN** the bench SHALL start

### Requirement: Constant VUs executor
`--executor constant-vus` (the default) SHALL run `--vus N` (default 1) VUs in a closed loop for `--duration` (default 30s) or `--iterations` total, whichever is given. Each VU SHALL wait `--think-time` (default 0, accepting a fixed value or a `min..max` uniform range) between iterations.

#### Scenario: Think time range
- **WHEN** the user runs with `--vus 2 --think-time 1s..3s`
- **THEN** each VU SHALL pause a uniformly random 1–3 seconds between iterations

### Requirement: Ramping VUs executor
`--executor ramping-vus` SHALL follow stages given as `--stage <duration>:<target_vus>` (repeatable) or, when no `--stage` is given, as `[[bench.stages]]` (`duration`, `target`) in `rustman.toml`, linearly changing the VU count during each stage. VUs removed during ramp-down SHALL finish their current iteration (graceful stop, max 30s).

#### Scenario: Ramp up and down
- **WHEN** the user runs `--executor ramping-vus --stage 30s:20 --stage 1m:20 --stage 30s:0`
- **THEN** VUs SHALL grow linearly from 0 to 20 over 30s, hold 20 for 1 minute, and drop to 0 over 30s

#### Scenario: Stages from rustman.toml
- **GIVEN** `rustman.toml` with `[[bench.stages]]` entries and no `--stage` flag
- **WHEN** the user runs `--executor ramping-vus`
- **THEN** the VU count SHALL follow those stages, and any `--stage` flag SHALL replace them all

### Requirement: Constant arrival rate executor
`--executor constant-arrival-rate --rate R` SHALL start R iterations per second regardless of response times (open model), using a pre-allocated VU pool (`--max-vus`, default 50). When no VU is free, the iteration SHALL be counted as `dropped` and reported.

#### Scenario: Pool exhausted
- **GIVEN** `--rate 100 --max-vus 10` and responses taking 1s
- **WHEN** the bench runs
- **THEN** about 90 iterations per second SHALL be reported as `dropped`

### Requirement: Coordinated-omission correction
In the arrival-rate executor, latency SHALL be measured from each iteration's scheduled start time, not its actual start, so queuing delay is included. Reports SHALL label these percentiles `corrected`. Closed-model executors SHALL report plain service latency.

#### Scenario: Server stall
- **GIVEN** `--rate 10` and the server stalls for 2 seconds once
- **WHEN** the bench completes
- **THEN** the corrected p99 SHALL reflect the 2-second stall for every iteration scheduled during it

### Requirement: Data feeding
`--data <file.csv|file.json>` SHALL feed rows as variables, with column or key names as variable names. `--data-mode` SHALL be `sequential` (default, shared cursor across VUs), `per-vu` (each VU gets its own row index) or `random`. `--data-exhaust stop|recycle` (default `recycle`) SHALL control what happens at the end.

#### Scenario: Users from CSV
- **GIVEN** `users.csv` with columns `email,password` and 3 rows
- **WHEN** the user benches with `--data users.csv --data-mode per-vu --vus 3`
- **THEN** VU 1 SHALL use row 1, VU 2 row 2 and VU 3 row 3 for `{{email}}` and `{{password}}`

### Requirement: Metrics
Bench SHALL record, per request file and overall:
- latency min, avg, p50, p90, p95, p99 and max, from HDR histograms with 3 significant digits;
- requests per second;
- iteration count and duration;
- errors by class: `4xx`, `5xx`, `timeout`, `connect`, `tls`, `blocked`, `assertion` and `other` (any other failure, such as an unresolved variable);
- per-phase timings from `http-requests`;
- bytes sent and received.

#### Scenario: Unresolved variable
- **GIVEN** a request file using `{{token}}`, which no environment, `--var` or data row defines
- **WHEN** it is benched
- **THEN** each of its requests SHALL count as an `other` error, with the resolution error as the error sample

#### Scenario: Per-request breakdown
- **WHEN** a bench of `login` and `list-items` completes
- **THEN** the summary SHALL show separate latency percentiles and error counts for each file and an overall row

### Requirement: Thresholds
Thresholds SHALL be given as `--threshold "<metric><op><value>"` (repeatable) or `[bench.thresholds]`. Metrics: `p50|p90|p95|p99|max|avg` latency, `error_rate`, `rps`, `checks` (assertion pass rate), optionally scoped `file:<name>`. Any breach SHALL make the command exit with code 1 after the report is written.

#### Scenario: Breached p95
- **GIVEN** `--threshold "p95<300ms"`
- **WHEN** the measured p95 is 412 ms
- **THEN** the summary SHALL show `✗ p95<300ms (412ms)` and the exit code SHALL be 1

#### Scenario: All thresholds pass
- **GIVEN** `--threshold "error_rate<1%"` and 0.2 % errors
- **WHEN** the bench completes
- **THEN** the exit code SHALL be 0

### Requirement: Abort-on-fail thresholds
A threshold suffixed with `!` (e.g. `error_rate<5%!`) SHALL be evaluated every second after a 10-second warm-up. When it's breached, the bench SHALL stop early, write a partial report marked `aborted: threshold`, and exit with code 1.

#### Scenario: Error storm
- **GIVEN** `--threshold "error_rate<5%!"`
- **WHEN** the error rate reaches 40 % at second 15
- **THEN** load SHALL stop within 2 seconds and the report SHALL be marked `aborted: threshold`

### Requirement: Graceful interruption
Ctrl+C SHALL stop starting new iterations, wait up to 10 seconds for in-flight requests, then write a partial report marked `aborted: interrupted` and exit with code 130. A second Ctrl+C SHALL cancel in-flight requests immediately.

#### Scenario: Interrupted bench
- **WHEN** the user presses Ctrl+C 20 seconds into a 5-minute bench
- **THEN** a report covering the first ~20 seconds SHALL be written and the exit code SHALL be 130

### Requirement: Bench reports
Bench SHALL write a `rustman.report/v1` report with `kind: bench`, containing the profile, the target, per-file and overall metrics, a per-second time series (RPS, p95, errors, active VUs), threshold results and redacted error samples (max 20 per class). `--report json=<f>` and `--report html=<f>` SHALL be supported.

#### Scenario: HTML report
- **WHEN** the user runs a bench with `--report html=out/bench.html`
- **THEN** `out/bench.html` SHALL be a self-contained page with latency-over-time and RPS charts that works offline

### Requirement: Live TUI dashboard
In the TUI (or `rustman bench` on a TTY), a live dashboard SHALL show elapsed and remaining time, active VUs, RPS, p50/p95/p99, error rate, a per-second sparkline of p95 and RPS, threshold status, and a per-file table, refreshed at most 4 times per second. The run SHALL never block on rendering.

#### Scenario: Non-TTY output
- **GIVEN** stdout is not a TTY
- **WHEN** a bench runs
- **THEN** progress SHALL be printed as one summary line every 10 seconds and the dashboard SHALL NOT be drawn

### Requirement: Live viz streaming
`rustman bench --viz` SHALL stream per-second metric events to the viz companion over the `rustman-viz/1` protocol. If the companion doesn't advertise a `bench` view in its handshake, Rustman SHALL warn `viz companion has no bench view` and continue without it. Viz problems SHALL never change the bench result or exit code.

#### Scenario: Companion lacks bench view
- **GIVEN** a companion whose `hello_ack` views omit `bench`
- **WHEN** the user runs `rustman bench … --viz`
- **THEN** the bench SHALL run normally and the warning SHALL be shown once

### Requirement: Agents cannot start a bench
The agent interface SHALL NOT expose any tool that starts a bench. Agents MAY read existing bench reports through the report tools.

#### Scenario: Agent tool list
- **WHEN** an MCP client lists Rustman's tools
- **THEN** no tool SHALL start a bench

### Requirement: Redaction
Bench output, error samples, reports and viz events SHALL apply the `environments` redaction rules, including data-file values from columns declared with `--secret-columns` or named like `password`, `token`, `secret` or `api_key` (auto-marked).

#### Scenario: Password column
- **GIVEN** `users.csv` with a `password` column declared secret through `--secret-columns password`
- **WHEN** an assertion fails and its error sample is recorded
- **THEN** the sample SHALL show `«redacted:password»`
