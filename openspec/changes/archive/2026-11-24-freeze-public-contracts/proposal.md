## Why

v1.0 freezes the formats and codes that scripts, CI and other tools depend on. The `cli-runner` exit code requirement still lists only 0/1/2/3/130, although `4` (refused by the safety policy) has been in use since the explorer and is documented in the CLI reference, and nothing in the specs states what "stable" means for the public contracts.

## What Changes

- The `Exit codes` requirement lists `4` (refused by the safety policy, the stress gate or the monitor policy).
- A new `Public contract stability` requirement freezes request-file `version: 1`, `rustman.toml` `version = 1`, `rustman.report/v1`, `rustman-viz/1`, the plugin WIT API `rustman:plugin@1` and the exit codes: additive changes only within a major, deprecation before removal, and guard tests in CI.
- `docs/stability.md` documents the promise, each contract's definition and guard test, and what is not covered.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `cli-runner`: the exit code table gains `4`; the public contracts are frozen for v1.

## Impact

Docs (`docs/stability.md`, README, `docs/README.md`) and tests only: frozen-surface tests for the report schema and the viz protocol, an exit code table test, a request-file version test. No behavior changes.
