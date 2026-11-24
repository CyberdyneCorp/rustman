# Tasks

## 1. Crate scaffold and definitions

- [x] 1.1 Create `crates/rustman-monitor` and add `rustman monitor run|daemon|status|list|history|test-alert|schedule-recipe --help` to `rustman-cli`; verify `cargo build` and help output.
- [x] 1.2 Implement `monitors/*.rman-monitor.yaml` loading and validation (exactly one of cron/every, `every` ≥ 30s, `timeout` default 60 s, line-numbered errors); verify unit tests for each scenario and `rustman monitor list`.
- [x] 1.3 Implement cron/interval parsing with `croner` on `jiff` time zones, including macros and DST; verify tests for spring-forward and fall-back days.

## 2. Execution and safety

- [x] 2.1 Implement one-shot `rustman monitor run <name>|--all` on the `cli-runner` engine with a whole-run deadline and exit codes 0/1/3; verify integration tests against a local fixture server for pass, fail and unreachable host.
- [x] 2.2 Build the monitor `GovernedSender` policy (environment hosts allowlist, read-only default, `allow_mutations`, DELETE always denied, exit 4); verify tests for the POST-without-opt-in and DELETE scenarios.

## 3. Store and retention

- [x] 3.1 Implement the JSONL store (raw per day, hourly aggregates, `state.json`) with `std::fs::File::lock` locking and atomic state writes; verify a concurrency test with two writers and no corrupt lines.
- [x] 3.2 Implement compaction (7 days raw → hourly aggregates, `[monitors].retention` default 30 days) and its triggers; verify tests with synthetic timestamps for both scenarios.
- [x] 3.3 Warn when `.rustman/` is not git-ignored; verify the warning test.

## 4. Alert rules and state machine

- [x] 4.1 Implement the pure `evaluate(rules, window, prev_state)` function for all rule types; verify table-driven unit tests per rule including the consecutive-failures and latency scenarios.
- [x] 4.2 Implement deduplication, cooldown and recovery with persisted state shared by daemon and one-shot runs; verify tests for no-repeat spam and the 42-minute recovery scenario.

## 5. Notifiers

- [x] 5.1 Implement desktop (the system's `osascript`/`notify-send`/PowerShell toast), webhook (`slack`, `discord`, `generic` `minijinja` templates with user overrides) and email (`lettre` with rustls, TLS required) channels with 10 s timeout, one retry and isolated failures; verify tests with a local webhook receiver, a fake SMTP server, and the one-channel-down scenario.
- [x] 5.2 Implement `rustman monitor test-alert`; verify per-channel output and exit code.
- [x] 5.3 Apply redaction to payloads, emails, notifications, stored results and logs, and never print secret webhook URLs; verify a test with a secret in an assertion message and in the webhook URL.

## 6. Daemon and recipes

- [x] 6.1 Implement `rustman monitor daemon` (heap scheduler, overlap skip, hot reload of monitor files, workspace lock file, graceful SIGINT/SIGTERM with 10 s drain, no listening port); verify integration tests for overlap, reload, second-daemon refusal, and that no socket is bound.
- [x] 6.2 Implement `rustman monitor schedule-recipe --for cron|systemd|launchd|github-actions` with optional `--write`; verify snapshot tests per target, and that the GitHub Actions output parses as a valid workflow.

## 7. Views

- [x] 7.1 Implement `rustman monitor status [--json]` (uptime 24h/7d/30d, p95, sparkline, firing rules, coverage) and `rustman monitor history`; verify the uptime scenario (284/287 → 98.95%) and the history filter.
- [x] 7.2 Add the TUI Monitors view with sparklines and a non-blocking "run now" through the same policy; verify a ratatui test-backend snapshot and a non-blocking test.
- [x] 7.3 Add the `monitor_trend` viz view (uptime gauges, latency plot, outcome heatmap) and the text fallback when the companion is absent; verify a protocol test with a stub companion and the fallback scenario.
  - Done: `monitor_trend` view and message in `rustman-viz/1` (schema, round-trip and doc tests in `crates/rustman-viz-protocol/tests/messages.rs`), trend data in `rustman_monitor::status::trend`, the producer in `rustman_tui::monitor_viz`; stub-companion and fallback tests in `crates/rustman-cli/tests/monitor_alerts.rs` and `crates/rustman-tui/src/monitors_tests.rs`. The Raven companion does not advertise the view yet (like `bench` and `stress`), so it falls back until its renderer lands.
- [x] 7.4 Document monitors in the user guide (file format, daemon vs. external schedulers, alert channels, retention); verify doc examples in a doc test.
