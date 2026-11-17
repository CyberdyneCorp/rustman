# Proposal

## Why

Once a collection has good assertions, the next question is "is this still working right now?". Today that means wiring `rustman run` into cron by hand and building alerting on top. Monitors make that first-class and local: a schedule, alert rules, a small local history of results, and notifications, all defined in plain files in the workspace. No SaaS is involved.

## What Changes

- New monitor definitions in `monitors/*.rman-monitor.yaml`. Each references requests or collections, an environment, a schedule (cron or interval), a timeout and alert rules: consecutive failures, failure count in a window, p95 latency, assertion failures, and expected status.
- `rustman monitor run <name|--all>` runs once and evaluates the alert rules. It's meant for external schedulers, with documented recipes for cron, systemd timers, launchd and GitHub Actions `schedule`.
- `rustman monitor daemon` is a long-running local scheduler for all monitors in the workspace.
- Results go to a local time-series store in `.rustman/monitors/` (git-ignored), with a 30-day retention by default and compaction of old points into hourly aggregates.
- Alerts can go to a desktop notification, a webhook POST (Slack, Discord and generic JSON templates) or email over SMTP (optional). Alerts are deduplicated, have a cooldown, and send a recovery notice when the monitor is healthy again.
- `rustman monitor status` shows uptime %, the last result and the latency trend per monitor. The TUI gets a monitors view with sparklines, and `visual-dashboards` gets trend charts.
- Monitors are **read-only by default**: they go through the safety policy, and mutating methods need explicit opt-in in the monitor file.

## Capabilities

### New Capabilities
- `monitors`: scheduled collection runs defined in workspace files, alert rules with deduplication and recovery, local time-series results with retention, notifications (desktop, webhook, SMTP), status and trend views in the CLI, TUI and viz companion.

### Modified Capabilities
None. Monitors execute through `cli-runner`'s run engine and `assertions`. Results are summarized with the `test-reports` schema, secrets are handled by `environments`, the views use `tui` and `visual-dashboards`, and sending goes through the safety policy from `api-exploration`. None of those requirements change.

## Out of scope

- A hosted or multi-node scheduler, distributed probes from several regions, or public status pages.
- PagerDuty/Opsgenie native integrations (the generic webhook template covers them in v1).
- Running monitors when the machine is asleep. Users who need 24/7 coverage run the daemon or the one-shot command on a server or in CI.
- Synthetic browser checks.

## Impact

- New crate `crates/rustman-monitor` (definitions, scheduler, rule evaluation, store, notifiers). It depends on `rustman-core`, `rustman-http` (run engine), `rustman-report` and `rustman-cli`'s runner library.
- New CLI subcommands: `rustman monitor run|daemon|status|list|history|test-alert`.
- New workspace files `monitors/*.rman-monitor.yaml`, and git-ignored state in `.rustman/monitors/`.
- New dependencies: `croner` (cron parsing), `notify-rust` (desktop notifications) and `lettre` with rustls (SMTP). None needs system libraries to be installed.
- The TUI gains a Monitors view, and the viz protocol gains a `monitor_trend` view (additive, still `rustman-viz/1`).
