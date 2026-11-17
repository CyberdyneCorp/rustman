# Proposal

## Why

Writing one assertion per field doesn't scale. Most regressions in a real API are unplanned changes to response shape or content that nobody thought to assert on. Snapshot testing records a known-good response once, then fails when it changes, with a readable diff. Normalization keeps timestamps and ids from causing noise, and a fast approve flow handles intended changes. Snapshots are plain files next to the request, so the change also shows up in code review.

## What Changes

- A new `snapshot` assertion, usable in any request file's `assertions` list.
- **First run and updates**: on the first run (or with `--update-snapshots`), the normalized response is stored under `__snapshots__/` next to the request file. Later runs compare against it.
- **What gets compared**: choose any of status, headers and body (default: status and body).
- **Normalization**:
  - ignore paths given as JSONPath
  - ignore or redact headers
  - regex replacements
  - sort arrays by a key
  - built-in presets for timestamps, UUIDs, ETags and dates
- **Diffs**: JSON-aware structural diffs, line diffs for text, and size/hash comparison for binary bodies.
- **Redaction**: secrets are redacted before anything is stored, per the `environments` redaction invariant.
- **CI mode**: with `--ci` or `CI=true`, snapshots are never written. A missing snapshot is a failure.
- `rustman snapshot prune` finds and removes snapshots that no request uses anymore. `rustman snapshot list` shows status.
- **TUI approve/reject**: a review flow that shows the diff.
- **Kinds**: works for `http`, `graphql`, `jsonrpc` and `mcp` exchanges, through the shared exchange model.
- **File format**: deterministic, so git diffs stay clean.

Out of scope:
- Visual or screenshot snapshots.
- Snapshots of streaming subscriptions beyond the bounded event list.
- Storing snapshots outside the workspace.
- Fuzzy or AI-based similarity matching.

## Capabilities

### New Capabilities

- `snapshot-testing`: the `snapshot` assertion, the snapshot file format and location, normalization rules and presets, diffing, update and CI modes, pruning, and the TUI review flow.

### Modified Capabilities

None. The assertion plugs into `assertions` as a new assertion type. The `--update-snapshots`, `--ci` and `snapshot` subcommands extend `cli-runner`'s command set without changing its existing requirements. The review flow uses `tui` extension points.

## Impact

- New module in `rustman-core` (the snapshot model and normalization), plus diff rendering in `rustman-report` for the CLI and in `rustman-tui` for review.
- New dependency `similar` for text and line diffs. The JSON structural diff is our own code over `serde_json::Value`.
- Snapshot files under `__snapshots__/` are tracked in Git, so `rustman init` must not ignore them.
- Reports gain a `snapshot` assertion result with diff details. This is an additive change to `rustman.report/v1`.
