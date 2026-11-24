# Tasks

## 1. Pipeline and IR

- [x] 1.1 Create `crates/rustman-import` with the neutral IR (`ImportedCollection`, folders, requests, environments, notes with source locations); verify `cargo test -p rustman-import` builds and the IR round-trips through serde.
- [x] 1.2 Implement `rustman import` and `export` subcommand skeletons in `rustman-cli` with `auto` detection, `--into`, `--dry-run`, `--on-conflict`, `--report`, `--max-size`, `--fail-on-unsupported`; verify CLI tests for the cannot-detect (exit 2), outside-workspace and oversize messages.
- [x] 1.3 Variable translation stage (Insomnia `_.x`, dynamic variable map, unmapped reporting); verify unit tests for each mapping row in the spec.
- [x] 1.4 Secret extraction stage producing `Secret<String>` placeholders and keychain writes (dry-run writes nothing); verify a test that scans all fixture outputs for the injected secret values and finds none, plus the no-active-environment case.
- [x] 1.5 Slugging, collision suffixes and conflict policy (skip/overwrite/rename) through the core deterministic emitter; verify a snapshot test that two imports into empty dirs are byte-identical and conflict tests per policy.
- [x] 1.6 Import report (human summary + `rustman.import-report/v1` JSON with schema in `docs/schemas/`) and exit codes; verify the JSON validates against the schema and counts match the summary. Document the command in `docs/import.md`.

## 2. Postman

- [x] 2.1 Parser for Collection v2.0/v2.1 (folders, order, items, disabled entries, collection/folder auth and variables); verify snapshot tests over a fixture corpus.
- [x] 2.2 Body modes and auth mapping, GraphQL → `kind: graphql`, unsupported auth reporting; verify one fixture per body mode and auth type.
- [x] 2.3 Script preservation as `# imported-script` comments plus the pattern matcher for status, header and capture suggestions; verify the spec's mapped and unmapped examples, and that no JS engine is in the dependency tree (`cargo tree` check in CI).
- [x] 2.4 Postman environment import with `secret` type handling; verify the staging fixture stores `api_key` as a secret.

## 3. Insomnia and Bruno

- [x] 3.1 Insomnia v4 JSON and v5 YAML parsers (groups, requests, GraphQL, environment flattening, template-tag reporting); verify snapshot tests and the sub-environment scenario.
- [x] 3.2 Bruno `.bru` parser with line numbers, `bruno.json` ordering, `environments/*.bru`, and `assert` mapping; verify the `res.status: eq 200` scenario and a malformed-file error with a line number.

## 4. HAR and HTTP files

- [x] 4.1 Streaming HAR 1.2 parser with dedup key, static-asset filter, `--host`/`--method`/`--content-type` filters and transport-header hygiene; verify the 40-entry fixture yields at most 10 files and pseudo-headers are dropped.
- [x] 4.2 `.http`/`.rest` parser (`###`, `@var`, bodies) plus `http-client.env.json`, private env and VS Code settings environments; verify the two-request and private-env scenarios.

## 5. Export

- [x] 5.1 Workspace → IR → Postman v2.1 writer; verify the round-trip property test (import → export → import is byte-identical for supported fixtures) and that output validates against the vendored Postman schema.
- [x] 5.2 Workspace → `.http` writer with warnings for assertions and captures; verify secrets stay `{{name}}` placeholders in output.

## 6. TUI entry point

- [x] 6.1 Add an "Import…" palette action that runs a dry-run preview and then imports on confirmation (layout from `add-terminal-ui`); verify with a ratatui test-backend snapshot of the preview screen.
  - Note: `rustman_import::session::run` runs the dry run and the import (under the import lock) and returns the redacted summary; the TUI dialog is `crates/rustman-tui/src/import_dialog.rs`. Snapshot `import_preview` in `import_dialog_tests.rs`, which also checks nothing is written before confirmation and no secret value reaches the screen, the toasts or the files. Documented in `docs/tui.md#importing-collections`.
