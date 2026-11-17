# Design

## Context

See proposal.md for the motivation. `cli-runner` already executes collections with assertions and produces run results, and the safety policy (`GovernedSender`, from `add-openapi-explorer`) already gates automated senders. Monitors add three things on top: a scheduler, durable state (results and alert state), and notifiers.

## Goals / Non-Goals

**Goals:**
- One monitor definition that works the same under the built-in daemon and under any external scheduler.
- Alerting that is calm: deduplication, cooldown and recovery by default.
- Bounded disk use with no database server and no C dependency.

**Non-Goals:**
- High availability, clustering or exactly-once execution.
- Sub-30-second checks (this is not a load test; see `load-testing`).

## Decisions

### D1. Run engine reuse
A monitor run is a `cli-runner` run (same loader, variable resolution, assertions and captures) whose sender is a `GovernedSender` built from the monitor's policy. The policy is: allowlist from the environment's hosts, read-only unless `allow_mutations`, and DELETE always denied. The per-run `timeout` is a deadline around the whole run, and the per-request timeouts still apply.

### D2. Scheduling with `croner` and a tokio timer wheel
`croner` parses cron expressions, including `@daily`-style macros and timezones through `chrono-tz`. The daemon keeps a min-heap of next-due instants and sleeps until the earliest one. Each monitor has a "running" flag that implements the overlap skip.
*Alternatives rejected:* `tokio-cron-scheduler`, which hides the overlap policy and pulls in its own store, and the `cron` crate, which has weaker macro and timezone support.

### D3. Store: append-only JSON Lines per day, plus an aggregate file
Layout: `.rustman/monitors/<name>/raw/YYYY-MM-DD.jsonl`, `.rustman/monitors/<name>/hourly.jsonl` and `.rustman/monitors/<name>/state.json` (alert state, last run). Appends hold an advisory file lock (`fs4`) and write one line with a single write. `state.json` is replaced atomically. Compaction rewrites the files for one day into hourly aggregates and then removes them.
*Alternatives rejected:* SQLite (a bundled C build, against the build constraint) and `redb`/`sled` (opaque binary formats users can't inspect or `grep`). JSONL stays readable, which suits a local-first tool.

### D4. Alert state machine
Per rule: `ok → firing` (notify), `firing → firing` (notify only if `now - last_notified >= cooldown`), `firing → ok` (send recovery with the duration since `firing_since`). The state lives in `state.json`, so one-shot runs under cron share it with the daemon. A unit-tested pure function `evaluate(rules, history_window, prev_state) -> (new_state, notifications)` holds the whole policy.

### D5. Notifiers
- Desktop: `notify-rust`. On Linux it talks D-Bus in pure Rust (zbus). On macOS and Windows it uses OS APIs through crates compiled by the platform toolchain, with no system libraries to install.
- Webhook: our HTTP engine (`rustman-http`), with built-in `minijinja` payload templates (`slack`, `discord`, `generic`). Users can override templates under `monitors/templates/`.
- Email: `lettre` with the `rustls` feature and STARTTLS or implicit TLS required (plain-text SMTP is refused).

Notifier sends have a 10 s timeout and one retry. Failures are logged and never block other channels or future runs.

### D6. Recipes rather than installers
`schedule-recipe` prints configuration and never installs anything on its own. Installing a systemd unit or a launchd agent changes the user's system outside the workspace, so it stays an explicit `--write` to a path the user chose.

### D7. Views
`monitor status` reads only the store, with no network. The TUI view subscribes to store updates through the same runner/event channel the TUI already uses for sends. The viz view adds a `monitor_trend` message type to the `rustman-viz/1` protocol (additive: older companions reject it with `hello_ack` views not including it, and Rustman falls back to text).

## Risks / Trade-offs

- [The laptop sleeps, leaving gaps] → Gaps appear as missing points, not failures. Uptime counts only executed runs, and `status` shows `coverage` (runs executed / runs expected).
- [Alert storms after a long outage] → Deduplication and cooldown, plus one recovery message.
- [Webhook URLs are themselves secrets] → They must be referenced as secrets and are redacted in every log line.
- [Clock changes / DST with cron] → `croner` with explicit `tz` handles DST. Tests cover the spring-forward and fall-back days.

## Migration Plan

Additive. `rustman init` adds `monitors/` to the scaffold only when the user asks (`rustman init --with-monitors`).

## Open Questions

- Whether a future change should add a read-only local status page (HTML) served by the daemon. That would require a listening port and is deliberately left out of v1.
