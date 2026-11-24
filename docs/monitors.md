# Monitors

A monitor runs some of a workspace's requests on a schedule and keeps the results locally, so "is this still working right now?" has an answer without a SaaS. Monitors are plain files in the workspace; their results stay on the machine in `.rustman/monitors/`, which Git must ignore.

The behavior is specified in [`openspec/specs/monitors`](https://github.com/CyberdyneCorp/rustman/blob/main/openspec/specs/monitors/spec.md). This page covers monitor files, schedules, one-shot runs and their safety policy, alerts and notification channels, the daemon and external scheduler recipes, the status and history views (CLI and TUI), and the result store with its retention.

## Monitor files

Each file `monitors/<name>.rman-monitor.yaml` defines one monitor:

```yaml
# monitors/api-health.rman-monitor.yaml
version: 1
name: api-health
targets: [health/]
environment: prod
schedule: {every: 5m}
timeout: 60s
```

| Field | Meaning |
|-------|---------|
| `version` | Always `1`. |
| `name` | The monitor's name: letters, digits, `-` and `_`, the same as the file name without `.rman-monitor.yaml`. The result store is named after it. |
| `targets` | Request files or collection folders, relative to the workspace root, run in workspace order (like `rustman run`). At least one. |
| `environment` | The environment the requests resolve with. |
| `schedule` | Exactly one of `cron` or `every`; see [Schedules](#schedules). |
| `timeout` | The deadline of a whole run (default `60s`); every request's own timeout still applies. |
| `allow_mutations` | `true` lets POST, PUT and PATCH requests be sent; see [Read-only by default](#read-only-by-default). |
| `alerts` | Alert rules and notification channels; see [Alerts](#alerts). |

Durations are a whole number followed by `ms`, `s`, `m`, `h` or `d`: `500ms`, `30s`, `5m`, `1h`, `30d`.

An invalid file stops every monitor command with exit code 2 and the file, line and reason:

```text
error: monitors/api-health.rman-monitor.yaml:5: schedule must set exactly one of cron or every
```

`rustman monitor list` lists the monitors (`--json` for a JSON array):

```text
NAME        SCHEDULE  ENVIRONMENT  TARGETS
api-health  every 5m  prod         health/
```

## Schedules

```yaml
schedule: {every: 5m}
```

`every` takes an interval from `30s` to `24h`; `every: 5s` fails with `every must be at least 30s`.

```yaml
schedule:
  cron: "*/15 * * * *"
  tz: Europe/Lisbon
```

`cron` takes a standard 5-field expression (minute, hour, day of month, month, day of week) or `@hourly`, `@daily` or `@weekly`. It is evaluated in `tz`, an IANA timezone name, and in the machine's local timezone without one. `tz` applies to `cron` only.

Daylight saving time follows the usual cron rules:

- On the spring-forward day, a fixed time inside the skipped hour (`30 2 * * *` in New York) runs right after the gap, at 03:00, once.
- On the fall-back day, a fixed time inside the repeated hour runs once, on the first pass.
- Wildcard and step fields (`0 * * * *`, `*/15 * * * *`) run in real time: an hourly monitor runs every 60 minutes through both transitions, so it skips the missing hour and runs twice in the repeated one.

## One-shot runs

`rustman monitor run <name>` runs a monitor once; `--all` runs every monitor of the workspace, one after the other. It is the command external schedulers call:

```text
*/5 * * * * cd /srv/ws && rustman monitor run api-health
```

A run uses the same engine as `rustman run`: the same request loading, variable resolution, assertions and captures. Monitor runs do not write [history](history.md). Each run prints one line, then one line per failure:

```text
api-health: fail in 123 ms
  health/status: status: expected 200, got 503
```

| Exit code | Outcome stored | When |
|-----------|----------------|------|
| 0 | `pass` | Every request passed. |
| 1 | `fail` | An assertion failed and no request errored. |
| 2 | (none) | An invalid monitor file, an unknown monitor, a missing target or environment, or a target that is not an HTTP request. |
| 3 | `error` | A request errored (network, TLS, timeout, unresolved variable) or the run passed its `timeout`. |
| 4 | `error` | A request was blocked by the monitor policy. |
| 130 | `skipped` | Interrupted by Ctrl+C (message `interrupted`). |

With `--all` the exit code is the most severe one: 130, then 4, 3, 1 and 0.

The `timeout` is a deadline for the whole run: when it passes, the requests in flight are cancelled, the rest are skipped, and the result is `error` with the message `run exceeded its timeout of 60s`.

Monitors run HTTP requests (`kind: http`). A target that holds another kind fails with exit code 2.

## Read-only by default

Every monitor request goes through the [safety policy](safety.md) with a `GovernedSender`:

- **Allowlist.** Requests may go only to the hosts of the monitor's environment: the origins of its URL-valued variables (such as `base_url`) and of the monitor's requests as they resolve with it. A redirect to another host, or a URL built from a capture that points elsewhere, is not sent: `blocked: host not allowed (https://other.example.com:443)`. `[safety].allowed_hosts` does not widen it. Because the origins of the requests count, a target request file with a hard-coded URL (not built from an environment variable) allows that URL's host: review the request files a monitor targets as part of its boundary.
- **Methods.** GET, HEAD and OPTIONS are allowed. POST, PUT and PATCH need `allow_mutations: true` in the monitor file: `blocked: method POST requires allow_mutations`. DELETE is never sent, whatever the file says: `blocked: method DELETE is never allowed for monitors`. Other methods are refused too. A request of a [plugin kind](plugins.md) is never run either (its plugin would send its own requests outside these rules): `blocked: plugin request kinds are not allowed for monitors`.
- **Rate.** `[safety].rate` and `concurrency` apply.

A blocked request makes the run an `error` and the exit code 4. The production guard does not ask for `--production`: the environment named in the monitor file is the confirmation, and the method rules hold on every host.

## Alerts

```yaml
alerts:
  cooldown: 1h
  rules:
    - consecutive_failures: 3
    - failures_in_window: {count: 5, window: 1h}
    - latency_p95_gt: 800
      window: 15m
    - assertion_failed: health/status
    - status_not: 2xx
  notify:
    - desktop: true
    - webhook: {url: "{{slack_hook}}", template: slack}
    - email: {host: smtp.example.com, port: 587, from: monitor@example.com, to: [ops@example.com], username: "{{smtp_user}}", password: "{{smtp_password}}", tls: starttls}
```

Each rule sets exactly one of `consecutive_failures` (default rule: `consecutive_failures: 2`), `failures_in_window`, `latency_p95_gt` (with an optional `window`, default `15m`), `assertion_failed` (a request id) or `status_not` (a code such as `200` or a class such as `2xx`). `cooldown` defaults to `1h`.

Notifier credentials never live in monitor files: a webhook `url` and an email `password` must be exactly one `{{name}}` reference to an environment variable or secret, and a literal value is refused when the file loads (without echoing it). Declare them as secrets of the environment (`rustman secret set slack_hook --env prod`), so they stay in the keychain or `.rustman/secrets.yaml`.

### Rules

Rules are evaluated after every run, one-shot or daemon, against the stored results. Skipped runs are ignored.

| Rule | Fires when |
|------|------------|
| `consecutive_failures: N` | The last N executed runs failed or errored. With `N: 3`, it fires after the third failing run and not before. |
| `failures_in_window: {count, window}` | At least `count` runs failed or errored within `window` before the latest run. |
| `latency_p95_gt: <ms>` | The p95 latency of the runs within `window` (default `15m`) is above the threshold. |
| `assertion_failed: <request>` | The assertions of that request (its id, such as `health/status`) failed in the latest run. |
| `status_not: <code or class>` | A response of the latest run has another status (`200`, or a class such as `2xx`). |

A rule *fires* when its condition holds after a run and *resolves* when it no longer does:

- **Deduplication.** A firing rule notifies once, not after every failing run.
- **Cooldown.** While it keeps firing, it sends a reminder (`[STILL FIRING]`) once `cooldown` (default `1h`) has passed since its last notification.
- **Recovery.** When it resolves, a `[RESOLVED]` notice says how long the outage lasted: a rule firing since 10:00 and resolved by a passing run at 10:42 reports `the outage lasted 42 minutes`.

The alert state lives in `state.json`, under the same lock the daemon and one-shot runs share, so a rule fires once whichever of them runs the monitor, and restarts do not resend it.

### Notification channels

Every alert goes to every channel of `notify`, all at once. Each channel gets 10 seconds per attempt and one retry; a channel that fails is reported (on stdout after `rustman monitor run`, in the daemon's log) and never stops the others or later runs.

- **`desktop`** shows an OS notification with the system's own tool: `osascript` on macOS, `notify-send` on Linux and the BSDs, and the Windows toast API through PowerShell (under Windows PowerShell's registered app id). Nothing is linked or installed. A tool that hangs is killed after 10 seconds.
- **`webhook`** POSTs JSON through the same HTTP engine as requests (proxies and `[http] ca_bundle` apply). A response outside 2xx is a failure, and redirects are not followed. The URL must be `https://`; plain `http://` is accepted only to `localhost` or a loopback address. The `slack` template sends `text` plus an attachment naming the monitor, the environment, the rule and the latest failure; `discord` sends an embed with the same fields; `generic` sends a flat object.
- **`email`** sends a plain-text mail over SMTP with `lettre` on rustls: STARTTLS (`tls: starttls`, port 587) or TLS from the first byte (`tls: tls`, port 465). Plain-text SMTP is never used: a server that does not offer STARTTLS fails the delivery before any credential is sent. Certificates are checked against the OS roots plus `[http] ca_bundle`. Email needs a build with the `smtp` feature (`cargo install --path crates/rustman-cli --features smtp`); without it the channel fails with `email notifications need rustman built with the smtp feature`.

Notifications are titled `[FIRING] api-health: consecutive_failures: 3`, `[STILL FIRING] …`, `[RESOLVED] …` or `[TEST] api-health`.

A workspace can replace a built-in webhook template with `monitors/templates/<slack|discord|generic>.json.j2`, a [minijinja](https://docs.rs/minijinja) template that must render JSON. It sees `monitor`, `environment`, `status` (`firing`, `reminder`, `resolved` or `test`), `rule`, `title`, `text`, `failure`, `at` (RFC 3339), `timestamp` (Unix seconds), `duration_seconds`, `color` (`#rrggbb`) and `color_int`, and a `tojson` filter:

```jinja
{"summary": {{ title | tojson }}, "env": {{ environment | tojson }}, "details": {{ text | tojson }}}
```

### Redaction

Payloads, emails, desktop notifications, stored results, logs and status output go through the same redaction as every other output: the values of the environment's secrets and of the channel's credentials are replaced with `«redacted:name»`, even when a server echoed one into an assertion message. A webhook URL is never printed: channels are named `webhook (slack)` or `email (smtp.example.com)`, and an error such as an unreachable host shows the URL redacted.

### Testing the channels

`rustman monitor test-alert <name>` sends `[TEST] <name>` through every channel and prints each outcome:

```text
webhook (slack): ok
email (smtp.example.com): error: smtp: permanent error (535): authentication failed
```

It exits 0 only when every channel succeeded, 3 otherwise, and 2 when the monitor has no channels.

## The result store

Each run appends one JSON line to `.rustman/monitors/<name>/`:

```text
.rustman/monitors/api-health/
  raw/2026-03-01.jsonl   one result per line, one file per UTC day
  hourly.jsonl           hourly aggregates of compacted days
  state.json             the last run, the last compaction and the alert state
```

A result holds the time, the outcome (`pass`, `fail`, `error` or `skipped`), the total latency, the run's counts (the `summary` fields of a [run report](reports.md)), each request's id, status, response code and latency, and the failure messages, redacted like every other output: a secret in an assertion message is stored as `«redacted:name»`.

```json
{"at":"2026-03-01T10:00:00Z","outcome":"fail","duration_ms":123.4,"summary":{"total":1,"passed":0,"failed":1,"errored":0,"skipped":0},"requests":[{"id":"health/status","status":"failed","code":503,"duration_ms":123.1}],"failures":["health/status: status: expected 200, got 503"]}
```

Writes are safe with concurrent writers, such as the daemon and a one-shot run under cron: an append holds an exclusive lock on the day file and writes its line in one write, and `state.json` and `hourly.jsonl` are replaced atomically (a temporary file renamed over them) under a lock file. A line that does not parse is skipped when reading.

The store must not be committed. When `.rustman/` is not git-ignored (Git decides inside a repository; otherwise the workspace's `.gitignore` must list it), storing a result prints:

```text
warning: .rustman/ is not git-ignored
```

`rustman init` adds `.rustman/` to `.gitignore`.

## Retention and compaction

Raw results are kept for 7 days. After that, compaction replaces each whole UTC day older than 7 days with hourly aggregates: the number of runs, passes, failures, errors and skipped runs, and the p50, p95 and maximum latency of the runs that executed. The counts stay the same. Aggregates are kept for `[monitors].retention`, 30 days by default, and deleted after that:

```toml
[monitors]
retention = "90d"   # at least 7d
```

Compaction runs when the daemon starts, then after runs (one-shot or daemon) when the last one was a day ago or more. A compaction cut short is safe to run again: the aggregates of a day replace any earlier ones for the same hours.

## The daemon

`rustman monitor daemon` runs every monitor of the workspace on its schedule until it is stopped. It logs one line per run, skip and reload to stderr:

```text
2026-03-01T10:00:00Z monitor daemon started: 2 monitor(s), no listening port
2026-03-01T10:00:00Z api-health: pass in 87 ms
2026-03-01T10:00:30Z api-health: skipped: previous run still in progress
2026-03-01T10:01:00Z api-health: fail in 120 ms (health/status: status: expected 200, got 503)
2026-03-01T10:01:00Z api-health: alert: [FIRING] api-health: consecutive_failures: 2
2026-03-01T10:01:00Z api-health: webhook (slack): ok
```

- **Schedule.** An `every` monitor runs at start, then every interval; a `cron` monitor at its next occurrence. A daemon that wakes up late (the laptop slept) runs once and carries on from now: missed runs leave a gap, not a burst, and `status` shows it as coverage.
- **No overlap.** A run that comes due while the monitor's previous run is still going is skipped and stored as `skipped`; the next run starts on its own due time.
- **Reload.** Monitor files are re-read every 2 seconds. A changed schedule applies at once without a restart (`every: 5m` to `every: 1m` runs a minute later), new monitors start, removed ones stop. An edit that does not load is logged and the previous monitors keep running.
- **One per workspace.** The daemon holds `.rustman/monitors/daemon.lock` (its pid is in `daemon.pid`). A second one fails with `monitor daemon already running (pid <n>)` and exit code 2.
- **Stopping.** Ctrl+C or SIGTERM starts no new run and gives the runs in flight up to 10 seconds to finish; then they are cancelled and stored as `skipped` (`interrupted`), so a restart counts toward neither uptime nor the alert rules and sends no notice. A second Ctrl+C cancels them at once. The daemon exits 0.
- **No port.** The daemon opens no listening socket; it only sends.

## External schedulers

The daemon is optional: `rustman monitor run <name>` does the same work once, so cron, systemd timers, launchd or CI schedules can drive monitors, sharing the store and the alert state. `rustman monitor schedule-recipe <name> --for cron|systemd|launchd|github-actions` prints a ready-to-use configuration on the monitor's schedule; it writes nothing unless `--write <path>` is given, and never installs anything. There is no Windows Task Scheduler recipe: on Windows, use `rustman monitor daemon`, `github-actions`, or a scheduled task that runs `rustman monitor run <name>` in the workspace folder.

| Scheduler | `every` | `cron` |
|-----------|---------|--------|
| `cron` | Mapped to cron when the interval divides an hour or a day (`5m` is `*/5 * * * *`, `6h` is `0 */6 * * *`) | As written, with `CRON_TZ` for `tz` |
| `systemd` | `OnUnitActiveSec` | `OnCalendar`, with `tz` |
| `launchd` | `StartInterval` | `StartCalendarInterval` (the Mac's timezone) |
| `github-actions` | Mapped to cron as above | As written, macros expanded (GitHub uses UTC) |

An interval with no cron equivalent (`every: 45s`), or a cron expression a calendar cannot express (a day of month and a day of week together, a stepped range), is refused with a hint. For `monitors/api-health.rman-monitor.yaml` with `schedule: {cron: "*/15 * * * *", tz: UTC}`, `--for github-actions` prints:

```yml
# .github/workflows/monitor-api-health.yml: runs the api-health monitor (cron */15 * * * * (UTC)).
# GitHub runs scheduled workflows at most every 5 minutes, sometimes late.
name: monitor api-health
on:
  schedule:
    - cron: "*/15 * * * *"
  workflow_dispatch: {}
jobs:
  monitor:
    runs-on: ubuntu-latest
    timeout-minutes: 16
    env:
      # Secrets come from .rustman/secrets.yaml: add a step that runs
      # printf '%s' "${{ secrets.NAME }}" | rustman secret set NAME --env prod
      RUSTMAN_SECRET_STORE: file
    steps:
      - uses: actions/checkout@v4
      - name: Install rustman
        run: cargo install --git https://github.com/CyberdyneCorp/rustman rustman-cli --locked
      - name: Run the monitor
        run: rustman monitor run api-health
```

Runs from a scheduled workflow keep their store in the job's checkout, so alert deduplication only holds across runs that share `.rustman/` (a self-hosted runner, or a cache step).

## Status and history

`rustman monitor status` reads the store only (no network) and shows, for every monitor, its last outcome, uptime over 24 hours, 7 days and 30 days, the p95 latency of the last 24 hours with a sparkline of the 24 hourly p95 values, coverage, and the firing rules:

```text
api-health (prod, every 5m)
  last      fail 3 minutes ago (2026-03-01T10:00:00Z)
  uptime    98.95% 24h · 99.71% 7d · 99.90% 30d
  p95 24h   123 ms  ▁▂▂▃▃▂▁▁▂▃▅▇█▆▄▃▂▂▁▁▂▂▃▃
  coverage  99.65%
  firing    consecutive_failures: 2 (since 2026-03-01T09:55:00Z)
```

- **Uptime** is passes over executed runs: skipped runs do not count. 288 runs in a day with 3 failures and 1 skipped is `98.95%` (284/287). Older periods count the hourly aggregates of compacted days.
- **Coverage** is executed runs over the runs the schedule expected in the last 24 hours (or since the first run): gaps from a sleeping laptop or a stopped daemon lower it, not the uptime.
- `--json` prints the same data: `{"monitors": [{"name", "environment", "schedule", "last", "uptime": {"24h", "7d", "30d"}, "p95_24h_ms", "hourly_p95_ms", "sparkline", "firing", "coverage_24h"}]}`.
- `--viz` also opens the trend view in the [viz companion](viz.md): uptime gauges, the hourly p95 latency over the last 30 days, and a heatmap of outcomes by UTC hour of day (the `monitor_trend` message of the [viz protocol](viz-protocol.md)), and prints `monitor trends opened in <companion>` on stderr. Without a companion, or with one that has no `monitor_trend` view (the current `rustman-viz` does not draw it yet), it prints `note: the viz companion is unavailable for monitor trends (<why>); showing the text status` on stderr. The text status is printed either way, and the exit code is 0.

`rustman monitor history <name>` lists the stored results, newest first, with their outcome, latency and failure messages. `--since 24h` keeps recent ones, `--outcome pass|fail|error|skipped` one outcome, and `--json` prints JSON lines (`"kind": "run"` or `"hour"`). Compacted days show as hourly aggregates:

```text
2026-03-01T10:00:00Z  fail       123 ms
    health/status: status: expected 200, got 503
2026-02-20T10:00:00Z  hourly  12 runs: 11 pass, 1 fail, 0 error, 0 skipped; p95 120 ms
```

## The TUI Monitors view

In the TUI, **Monitors** in the command palette (`monitors.open`) lists each monitor with its last outcome, 24-hour uptime, latency sparkline and whether a rule is firing, with the selected monitor's firing rules and latest results below. `j`/`k` select, `r` runs the selected monitor now, and `Esc` closes. The run goes through the same cycle as `rustman monitor run` (the monitor policy, the store, alerts and notifications) in the background: the UI keeps responding, the row shows `running`, and the list refreshes when it finishes. `v` opens the same trend view as `rustman monitor status --viz` in the background, or says why the companion cannot show it.
