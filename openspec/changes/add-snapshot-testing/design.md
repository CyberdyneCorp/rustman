# Design

## Context

`assertions` evaluates a list of assertion types against an exchange, `cli-runner` owns flags and reporters, `environments` owns redaction, and `tui` owns the response viewer. Every request kind produces the same exchange model (status, headers, body bytes, optional event list). Snapshots therefore only need a normalizer, a store and a differ on top of that model. See proposal.md for the motivation.

## Goals / Non-Goals

**Goals:**
- Deterministic normalization and serialization, so snapshot files produce minimal git diffs.
- Zero chance of writing a resolved secret into a tracked file.
- The same behavior in CLI, TUI and CI.

**Non-Goals:**
- Inline snapshots stored inside the request file.
- Approximate numeric comparison (that's what `json` assertions with `gt`/`lt` are for).
- Snapshot sharing between workspaces.

## Decisions

### D1. Separate `.snap.yaml` files, not inline
Inline snapshots would bloat request files and make every response change look like a request edit in review. Separate files under `__snapshots__/` follow the convention of `insta` and Jest, keep request files small, and let `prune` reason about orphans by path.
- *Rejected:* inline `snapshot: {value: …}`, and a single workspace-wide snapshot database (it causes merge conflicts).

### D2. YAML with sorted keys via the workspace's deterministic emitter
`bootstrap-core-engine` already has a deterministic YAML emitter for request files. Reusing it gives byte-stable output and readable multi-line text, so there's no new format to learn.
- *Rejected:* JSON (noisy diffs for long strings and escaped newlines), and a binary or CBOR format (not reviewable).

### D3. Redaction first, using the core redaction step
Normalization begins with the same redaction function every output path uses, so snapshots can't become a leak path. The secret-echo scan (substring match of known secret values) applies too, which catches tokens echoed inside bodies.

### D4. Our own JSON structural diff, `similar` for text
JSON diffs walk two `serde_json::Value` trees after normalization and emit `(JSONPath, kind, old, new)` entries. That gives precise paths, which a line diff can't. Arrays are compared by index after the optional sort rule. Text uses `similar`'s unified diff, the same crate `insta` uses.
- *Rejected:* `json-patch` diffs (RFC 6902 output is harder to read and its array moves are confusing), and `treediff` (unmaintained).

### D5. CI detection by flag or `CI` variable
`CI=true` is set by GitHub Actions, GitLab, CircleCI and others. Treating it as CI mode by default means a forgotten snapshot fails the pipeline instead of being silently recorded on the runner and lost.

### D6. Placeholders with typed markers
Placeholders like `«ignored»`, `«timestamp»` and `«uuid»` use the same `«…»` style as `«redacted:name»`. They're unlikely to collide with real data and are easy to spot in review.

### D7. Prune is opt-in destructive
Deleting tracked files needs `--yes` or a TTY confirmation, and non-interactive runs default to listing only. This follows the project's safe-by-default principle.

## Risks / Trade-offs

- [Over-broad ignore rules hide real regressions] → `snapshot list --json` and reports include the rules in effect, and placeholders stay visible in files and diffs.
- [Locale or timezone-dependent responses flap] → the `timestamps` and `dates` presets are on by default, and users can add `replace` rules.
- [Large bodies make huge snapshot files] → bodies above 1 MiB after normalization are stored as `{size, sha256}`, with a warning suggesting `ignore` rules.
- [Approving in the TUI while a run is writing] → snapshot writes go through the same atomic temp-file-and-rename path with a per-file lock.

## Migration Plan

Additive. Existing request files don't change. Adding `snapshot` to a file starts recording on the next non-CI run.

## Open Questions

- Whether `rustman snapshot review` should exist as a standalone TUI entry point, beyond the post-run queue. This is UX only.
