## ADDED Requirements

### Requirement: Monitor definition files
Each file `monitors/<name>.rman-monitor.yaml` SHALL define one monitor with:
- `version: 1`;
- `name`;
- `targets` (request files or collection paths, run in workspace order);
- `environment`;
- `schedule`, with exactly one of `cron` or `every`;
- `timeout` (default 60 s per run);
- `alerts`.

Invalid files SHALL fail to load with the file, line and reason, and exit code 2.

#### Scenario: Valid monitor
- **GIVEN** `monitors/api-health.rman-monitor.yaml` with `targets: [health/], environment: prod, schedule: {every: 5m}`
- **WHEN** the user runs `rustman monitor list`
- **THEN** `api-health` SHALL be listed with schedule `every 5m` and environment `prod`

#### Scenario: Both schedule kinds
- **GIVEN** a monitor with both `cron` and `every`
- **WHEN** monitors load
- **THEN** loading SHALL fail with `<path>:<line>: schedule must set exactly one of cron or every` and exit code 2

### Requirement: Schedules
`cron` SHALL accept standard 5-field expressions plus `@hourly`, `@daily` and `@weekly`, evaluated in the timezone `tz` (default: local). `every` SHALL accept durations from `30s` up to `24h`. Shorter intervals SHALL be rejected with exit code 2. Runs SHALL never overlap for the same monitor: a run due while the previous one is still going SHALL be skipped and recorded as `skipped: previous run still in progress`.

#### Scenario: Interval too short
- **GIVEN** `schedule: {every: 5s}`
- **WHEN** monitors load
- **THEN** loading SHALL fail with `every must be at least 30s` and exit code 2

#### Scenario: Overlap skipped
- **GIVEN** a monitor every 30 s whose run takes 45 s
- **WHEN** the next run comes due
- **THEN** it SHALL be skipped and recorded as `skipped`, and the following run SHALL start on its next due time

### Requirement: One-shot run
`rustman monitor run <name>` (or `--all`) SHALL execute the monitor's targets once, store the result, evaluate the alert rules and send any alerts. It SHALL exit `0` when the run passed, `1` when an assertion failed, and `3` when a request errored, following the ROADMAP exit codes. This lets external schedulers use it.

#### Scenario: Cron-driven run
- **GIVEN** a crontab line `*/5 * * * * cd /srv/ws && rustman monitor run api-health`
- **WHEN** cron runs it and all assertions pass
- **THEN** a result SHALL be stored in `.rustman/monitors/` and the exit code SHALL be 0

#### Scenario: Request error
- **GIVEN** the target host is unreachable
- **WHEN** `rustman monitor run api-health` runs
- **THEN** the result SHALL be stored as `error` and the exit code SHALL be 3

### Requirement: Local daemon
`rustman monitor daemon` SHALL schedule every monitor in the workspace and run until stopped, by SIGINT or SIGTERM, which let the in-flight run finish for up to 10 s. It SHALL reload monitor files when they change, log one line per run to stderr, and hold a lock file so only one daemon runs per workspace. It SHALL open no listening port.

#### Scenario: Second daemon refused
- **GIVEN** a daemon already running for the workspace
- **WHEN** a second `rustman monitor daemon` starts
- **THEN** it SHALL fail with `monitor daemon already running (pid <n>)` and exit code 2

#### Scenario: File edited while running
- **GIVEN** a running daemon
- **WHEN** the user changes a monitor's schedule from `every: 5m` to `every: 1m`
- **THEN** the daemon SHALL apply the new schedule without restarting

### Requirement: External scheduler recipes
`rustman monitor schedule-recipe <name> --for cron|systemd|launchd|github-actions` SHALL print a ready-to-use configuration that runs `rustman monitor run <name>` on the monitor's schedule. It SHALL write nothing to disk unless `--write <path>` is given.

#### Scenario: GitHub Actions recipe
- **GIVEN** a monitor with `cron: "*/15 * * * *"`
- **WHEN** the user runs `rustman monitor schedule-recipe api-health --for github-actions`
- **THEN** a workflow YAML SHALL be printed with `on.schedule.cron: "*/15 * * * *"` and a step running `rustman monitor run api-health`

### Requirement: Result store
Each run SHALL append a result to `.rustman/monitors/<name>/`:
- timestamp;
- outcome (`pass`, `fail`, `error` or `skipped`);
- total and per-request latency;
- status codes;
- failed assertion messages (redacted).

Writes SHALL be atomic and safe with concurrent writers. The directory SHALL be git-ignored, and the system SHALL warn when it isn't.

#### Scenario: Result appended
- **WHEN** a monitor run finishes with a failed assertion
- **THEN** a `fail` result with the assertion message SHALL be appended to `.rustman/monitors/api-health/`

#### Scenario: Store not ignored
- **GIVEN** `.rustman/` is not covered by `.gitignore`
- **WHEN** a monitor stores a result
- **THEN** the warning `.rustman/ is not git-ignored` SHALL be printed

### Requirement: Retention and compaction
Raw results SHALL be kept for 7 days, then compacted into hourly aggregates (runs, passes, failures, errors, latency p50/p95/max). Aggregates SHALL be kept up to `[monitors].retention`, which defaults to 30 days, and deleted after that. Compaction SHALL run at daemon start, once a day, and at the end of a one-shot run when it's overdue.

#### Scenario: Old points compacted
- **GIVEN** raw results older than 7 days
- **WHEN** compaction runs
- **THEN** they SHALL be replaced by hourly aggregates with the same pass/fail counts

#### Scenario: Retention expiry
- **GIVEN** aggregates older than 30 days with default settings
- **WHEN** compaction runs
- **THEN** they SHALL be deleted

### Requirement: Alert rules
`alerts.rules` SHALL support:
- `consecutive_failures: N` (default 2);
- `failures_in_window: {count, window}`;
- `latency_p95_gt: <ms>` over `window` (default 15m);
- `assertion_failed: <request>`;
- `status_not: <code|class>`.

A rule becomes *firing* when its condition holds after a run, and *resolved* when it no longer holds.

#### Scenario: Consecutive failures
- **GIVEN** `consecutive_failures: 3`
- **WHEN** three runs in a row fail
- **THEN** the rule SHALL fire after the third run and not before

#### Scenario: Latency rule
- **GIVEN** `latency_p95_gt: 800` with `window: 15m`, and runs in the last 15 minutes with p95 950 ms
- **WHEN** the next run finishes
- **THEN** the latency rule SHALL be firing

### Requirement: Deduplication, cooldown and recovery
A firing rule SHALL notify once, not after every failing run. While it keeps firing, it SHALL notify again only after `cooldown` (default 1h) as a reminder. When it resolves, a recovery notice SHALL be sent with the downtime duration. Alert state SHALL persist across daemon restarts and one-shot runs.

#### Scenario: No repeat spam
- **GIVEN** a rule that fired 10 minutes ago and a 1h cooldown
- **WHEN** another failing run finishes
- **THEN** no new notification SHALL be sent

#### Scenario: Recovery notice
- **GIVEN** a firing rule since 10:00
- **WHEN** a passing run at 10:42 resolves it
- **THEN** a recovery notification SHALL be sent stating the outage lasted 42 minutes

### Requirement: Notification channels
`alerts.notify` SHALL support:
- `desktop` (an OS notification);
- `webhook` (`url`, plus `template` `slack`, `discord` or `generic`, the default);
- `email` (SMTP `host`, `port`, `from`, `to`, with STARTTLS or TLS required), in builds with the `smtp` cargo feature.

Credentials SHALL be referenced as secrets. A channel's failure SHALL be logged and SHALL NOT stop the other channels.

#### Scenario: Email without the smtp feature
- **GIVEN** a build without the `smtp` feature and an `email` channel
- **WHEN** an alert is sent
- **THEN** only that channel SHALL fail, with `email notifications need rustman built with the smtp feature`

#### Scenario: Slack webhook
- **GIVEN** `notify: [{webhook: {url: "{{slack_hook}}", template: slack}}]`
- **WHEN** a rule fires
- **THEN** a Slack-compatible JSON payload naming the monitor, the rule, the environment and the latest failure SHALL be POSTed

#### Scenario: One channel down
- **GIVEN** desktop and webhook channels, and the webhook URL returns 500
- **WHEN** a rule fires
- **THEN** the desktop notification SHALL still be shown and the webhook failure SHALL be logged

### Requirement: Test alerts
`rustman monitor test-alert <name>` SHALL send a clearly labeled test notification through every channel of the monitor, report the outcome per channel, and exit `0` only if all channels succeed.

#### Scenario: Verify channels
- **WHEN** the user runs `rustman monitor test-alert api-health`
- **THEN** each channel SHALL receive a message titled `[TEST] api-health` and the CLI SHALL print `ok` or the error per channel

### Requirement: Redaction in alerts and results
Alert payloads, emails, desktop notifications, stored results and status output SHALL go through the `environments` redaction step. Notification webhook URLs taken from secrets SHALL never be printed in logs.

#### Scenario: Secret in a failure message
- **GIVEN** an assertion failure message that contains a secret header value
- **WHEN** the alert is sent
- **THEN** the payload SHALL show `«redacted:name»` instead of the value

### Requirement: Monitors are read-only by default
Monitor requests SHALL pass the safety policy from `api-exploration`. The allowlist SHALL be the origins of the environment's URL-valued variables and of the targets' resolved URLs; other origins SHALL be blocked. Only GET, HEAD and OPTIONS are allowed unless the monitor file sets `allow_mutations: true`. DELETE SHALL always be refused for monitors. A blocked request SHALL record an `error` result and exit with code 4.

#### Scenario: POST without opt-in
- **GIVEN** a monitor whose target includes a POST request and no `allow_mutations`
- **WHEN** it runs
- **THEN** the request SHALL be blocked with `blocked: method POST requires allow_mutations` and the exit code SHALL be 4

#### Scenario: DELETE never allowed
- **GIVEN** `allow_mutations: true` and a DELETE request among the targets
- **WHEN** the monitor runs
- **THEN** the DELETE SHALL be blocked

### Requirement: Status command
`rustman monitor status` SHALL list every monitor with:
- its last outcome and time;
- uptime % over 24 h, 7 d and 30 d (passes over non-skipped runs);
- p95 latency over 24 h;
- a text sparkline of the last 24 hourly p95 values;
- firing rules.

`--json` SHALL output the same data.

#### Scenario: Uptime calculation
- **GIVEN** 288 runs in the last 24 h with 3 failures and 1 skipped
- **WHEN** the user runs `rustman monitor status`
- **THEN** 24 h uptime SHALL be shown as `98.95%` (284/287)

### Requirement: Result history command
`rustman monitor history <name> [--since 24h] [--outcome fail]` SHALL list stored results, newest first, with outcome, latency and failure messages. Points that have already been compacted SHALL be shown as hourly aggregates.

#### Scenario: Filter failures
- **WHEN** the user runs `rustman monitor history api-health --since 24h --outcome fail`
- **THEN** only failed results from the last 24 hours SHALL be listed

### Requirement: TUI monitors view
The TUI SHALL offer a Monitors view, opened from the command palette, listing each monitor's status, uptime, firing rules and a latency sparkline. Selecting a monitor SHALL show recent results and allow a "run now", which goes through the same safety policy.

#### Scenario: Run now from the TUI
- **GIVEN** the Monitors view open with `api-health` selected
- **WHEN** the user triggers "run now"
- **THEN** the run SHALL execute in the background without blocking the UI and the list SHALL update when it finishes

### Requirement: Trend charts in the viz companion
When the viz companion from `visual-dashboards` lists the `monitor_trend` view, `rustman monitor status --viz` and `v` in the Monitors view SHALL open it with one `monitor_trend` message per monitor: uptime gauges, hourly p95 latency over time, and outcome counts by UTC hour of day for the heatmap. Otherwise the CLI SHALL fall back to the text status and the TUI SHALL say why.

#### Scenario: Companion with the trend view
- **GIVEN** a companion whose `hello_ack` lists `monitor_trend`
- **WHEN** the user runs `rustman monitor status --viz`
- **THEN** the companion SHALL receive `open_view` for `monitor_trend`, one `monitor_trend` per monitor and `close`, the text status SHALL still be printed, and the exit code SHALL be 0

#### Scenario: Companion absent
- **GIVEN** `rustman-viz` is not installed
- **WHEN** the user runs `rustman monitor status --viz`
- **THEN** the text status SHALL be printed with a note that the viz companion is unavailable, and the exit code SHALL be 0
