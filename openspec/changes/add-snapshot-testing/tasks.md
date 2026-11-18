# Tasks

## 1. Model and normalization

- [x] 1.1 Add the `snapshot` assertion type to the request model (shorthand and full form, unique names, valid regexes) in `rustman-core`. Verify: fixture tests for shorthand, duplicate names and invalid regex messages.
- [x] 1.2 Implement the normalizer: redaction (including the secret-echo scan) → ignore → presets (`timestamps`, `dates`, `uuids`, `etags`, `epoch_ms`, `jwt`) → replace → sort → canonical form. Verify: unit tests per rule, plus a property test that normalizing twice gives identical output.
- [x] 1.3 Implement compared parts and the header include list. Verify: tests for the `Date` change being ignored and the `content-type` change failing.
- [x] 1.4 Document snapshots in `docs/snapshots.md` with examples for every rule. Verify: a doc test loads each example.

## 2. Storage

- [x] 2.1 Implement snapshot paths (`__snapshots__/<stem>.<name>.snap.yaml`) and the file format through the deterministic YAML emitter, plus `{size, sha256}` for binary and over-1 MiB bodies. Verify: golden tests for byte-stable output with reordered keys; binary fixture test.
- [x] 2.2 Write atomically with a per-file lock, and skip rewriting matching snapshots. Verify: concurrent-write test; mtime unchanged on matching update.
- [x] 2.3 Ensure `rustman init` doesn't ignore `__snapshots__/`. Verify: init test checks the generated `.gitignore`.

## 3. Comparison and diffs

- [x] 3.1 Implement the JSON structural differ (paths, kinds, cap of 50 plus a count) and text diffs via `similar`. Verify: tests for the changed-field and truncated-diff scenarios.
- [x] 3.2 Implement event-list snapshots with the event-count message. Verify: test for 3 → 2 events.

## 4. Runner integration

- [x] 4.1 Implement record, update (`-u`/`--update-snapshots`) and CI mode (`--ci`, `CI=true|1`) with the conflicting-flags error, plus summary counts. Verify: `assert_cmd` tests for first run, update, CI missing (exit 1) and conflicting flags (exit 2).
- [x] 4.2 Add snapshot outcomes to the pretty, JSON (additive `rustman.report/v1` fields), JUnit and TAP reporters. Verify: insta snapshots of each reporter's output; JSON validates against the published schema.
- [x] 4.3 Prove redaction end to end: a stub echoing a secret, recorded and then updated. Verify: the test greps the snapshot file for the raw secret and finds nothing.

## 5. Snapshot commands

- [x] 5.1 Implement `rustman snapshot list` (states ok, orphaned, unreferenced name; `--json`). Verify: tests with orphan and unreferenced fixtures.
- [x] 5.2 Implement `rustman snapshot prune` (`--dry-run`, `--yes`, TTY confirmation, non-TTY list-only). Verify: tests for deletion with `--yes` and list-only without it.

## 6. TUI review

- [x] 6.1 Add the Snapshot tab with diff rendering, `a` approve and `r` reject, disabled in CI mode. Verify: ratatui test-backend snapshots; file rewritten on approve and unchanged on reject.
- [x] 6.2 Add the post-run review queue (`Alt+A`; `Ctrl+Shift+<letter>` is not distinguishable in most terminals) that steps through failing snapshots. Verify: TUI test that walks a queue of three failures.

## 7. Integration

- [x] 7.1 End-to-end over the `http`, `graphql` (stub), `jsonrpc` (stub) and `mcp` (stub) kinds: record, then a mutated response fails in CI mode, then update fixes it. Verify: the CI job passes across Linux, macOS and Windows (path separators in snapshot paths).
