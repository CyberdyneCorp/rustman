# Stability and compatibility

From v1.0, Rustman keeps a small set of **public contracts** stable: the file formats you commit, the documents and protocols other tools read, and the exit codes scripts branch on. This page lists them, says what "stable" means for each, and names the test that fails CI when one of them changes by accident.

## The promise

- **Within a major version, changes are additive only.** A frozen contract can gain new optional fields, new message types, new report kinds, new enum values, new functions on imported interfaces and new commands. Nothing that exists is renamed, removed, retyped or given a new meaning, and no new field becomes required where older writers would not send it.
- **Readers ignore what they don't know.** Readers of the report and the viz protocol must accept unknown fields, kinds and enum values; Rustman's own readers do. Files written by an older 1.x release stay valid in every later 1.x release.
- **Breaking changes bump the contract's major version** (`version: 2`, `rustman.report/v2`, `rustman-viz/2`, `rustman:plugin@2.0.0`) and ship in a new major Rustman release. Rustman refuses a major version it does not support with a clear error, instead of guessing.
- **Deprecation before removal.** A field, flag, value or command that is going away is first marked deprecated in its documentation and in the release notes, keeps working for the rest of the major version, and, where there is human-readable output, makes Rustman print a warning when it is used. It is removed only in the next major version.

## Frozen contracts

| Contract | Where it is defined | What is frozen | Guard tests |
|---|---|---|---|
| Request files, `version: 1` | [Request files](request-files.md), the `*.rman.yaml` kinds in each protocol's page, [`openspec/specs/workspace`](https://github.com/CyberdyneCorp/rustman/blob/main/openspec/specs/workspace/spec.md) | The `version: 1` header and every documented field of every built-in kind. Any other version, or none, is refused with exit code 2 before the kind's fields are read. | `request_file_version_1_is_frozen_for_every_kind`, `future_version_is_rejected` and `a_quoted_version_is_rejected` in `crates/rustman-core/tests/request_files.rs` |
| Workspace settings, `rustman.toml` `version = 1` | [Request files](request-files.md#rustmantoml) | The `[workspace]` `version = 1` key and the documented tables. A newer version is refused. | `newer_settings_version_fails_to_load` in `crates/rustman-core/tests/workspace.rs` |
| Reports, `rustman.report/v1` | [Reports](reports.md), JSON Schema [`docs/schemas/report.schema.json`](schemas/report.schema.json) (also printed by `rustman report schema`) | Every property, required field, type and enum value of each report kind (`run`, `exploration`, `contract`, `bench`, `stress`). | The checked-in schema must equal the one generated from the report types (`report_schema_prints_the_published_schema` in `crates/rustman-cli/tests/report.rs`); its surface must contain every line of `crates/rustman-report/tests/frozen/report-v1.txt` (`crates/rustman-report/tests/frozen.rs`); v0.1 run reports still validate (`reports_without_data_keep_the_v0_1_shape`); other majors are refused (`an_unknown_major_version_exits_2_naming_it`) |
| Viz protocol, `rustman-viz/1` | [Viz protocol](viz-protocol.md), JSON Schema `crates/rustman-viz-protocol/schema/rustman-viz-1.schema.json` | The protocol name, the line limit, every message type with its fields, types and required fields, and the views, report kinds, outcomes, severities and run states. | `crates/rustman-viz-protocol/tests/frozen.rs` against `crates/rustman-viz-protocol/schema/frozen/rustman-viz-1.txt`; every message type decodes from its example in the protocol doc; `crates/rustman-viz-protocol/tests/messages.rs` (round trips, schema, unknown fields and types) |
| Plugin API, `rustman:plugin@1` | [Plugins](plugins.md#plugin-api-versions), WIT package `crates/rustman-plugin-sdk/wit/plugin.wit` | Every type, function, world import and world export of the package. Exported interfaces get no new functions (existing plugins would lack them). | `crates/rustman-plugin/tests/wit_api.rs` against `crates/rustman-plugin-sdk/wit/frozen/1.0.0.txt` |
| CLI exit codes | [CLI reference](cli.md#exit-codes), `crates/rustman-cli/src/exit.rs` (`CODES`) | The codes below and their meanings. A code is never reused for another meaning. Command-specific uses documented on each command's page (for example `rustman import`) are frozen too. | `the_exit_codes_are_frozen_and_documented` in `crates/rustman-cli/src/exit.rs`, which also checks this table and the one in the CLI reference |

### CLI exit codes

| Code | Meaning |
|------|---------|
| 0 | Success: every executed request passed. |
| 1 | At least one assertion (or threshold, or finding at the `--fail-on` level) failed, and nothing errored. |
| 2 | Usage or configuration error: a bad flag, no workspace, an invalid or unsupported file. |
| 3 | At least one request errored (network, TLS, timeout, auth, unresolved variable). It wins over 1. |
| 4 | Refused by the safety policy, the stress gate or the monitor policy. |
| 130 | Interrupted by Ctrl+C (SIGINT), or on Unix by SIGTERM or SIGHUP. |

## Changing a frozen contract

An additive change updates the contract's definition and its documentation, then refreshes the frozen surface file in the same pull request, so the review shows exactly what was added:

```bash
RUSTMAN_FREEZE_REPORT=1 cargo test -p rustman-report --test frozen
RUSTMAN_FREEZE_VIZ=1 cargo test -p rustman-viz-protocol --test frozen
RUSTMAN_FREEZE_WIT=1 cargo test -p rustman-plugin --test wit_api
```

A refresh only ever adds lines. A diff that removes a line from a frozen file is a breaking change: it needs a new major version of that contract, not a refresh. A new exit code is added to `CODES`, to the table above and to the [CLI reference](cli.md#exit-codes).

## Not covered

These can change in any release, including minor ones:

- **The terminal UI**: layout, colours, panes, toasts and the default key bindings.
- **Human-readable output**: the `pretty` reporter, progress lines, dashboards, warnings and error messages. Scripts should read reports, `--json` output and exit codes instead.
- **The `--json` output of individual commands** other than reports (for example `rustman kafka consume --json`): it is documented with each command and changes additively in practice, but it is not frozen yet.
- **The agent (MCP server) tool set and the HTML, Markdown and SARIF renderings** of reports. SARIF follows the SARIF 2.1.0 schema; its rule ids and messages may change.
- **The Rust APIs of the workspace's crates** (`rustman-core`, `rustman-http`, `rustman-report` and the others). They are internal and unpublished; only the plugin SDK's WIT package is a public API.
- **Files under `.rustman/`** (history, caches, journals, monitor results): their format belongs to Rustman, not to you.
- **Behaviour that is a bug**: fixing it is not a breaking change, even if something relied on it. Security fixes may tighten defaults (for example refusing a request that was allowed) within a major version; the release notes say so.
