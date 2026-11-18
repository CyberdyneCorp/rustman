# Tasks

## 1. OpenAPI model and import

- [x] 1.1 Add `crates/rustman-explore` with `oas3`, and lower 3.0/3.1 documents into the `ApiModel` IR (including the `nullable` rewrite). Verify with unit tests on petstore 3.0 and 3.1 fixtures, where both lower to the same IR.
- [x] 1.2 Detect version and format (JSON/YAML by content). Reject Swagger 2.0 with exit 2 and the conversion hint. Verify with CLI integration tests (assert_cmd) for each case.
- [x] 1.3 Load sources from a file or a URL through `http-requests`, with a 30 s timeout and a 20 MiB cap. Verify with tests against a local hyper server, including the oversized case.
- [x] 1.4 Resolve `$ref`s: local and relative file refs, remote refused unless `--allow-remote-refs`, cycles preserved. Verify with fixtures for each case.
- [x] 1.5 Validate the document and print diagnostics with JSON pointers (errors abort, warnings continue). Verify with snapshot tests (insta) of the diagnostic output.
- [x] 1.6 Generate collections: tag folders, kebab-case file names with collision suffixes, parameters as variables. Verify with snapshot tests of the generated tree.
- [x] 1.7 Implement example selection and the schema synthesizer (required props, formats, enums, depth cap 3). Verify with property tests: synthesized bodies validate against their schema.
- [x] 1.8 Map servers to environments and securitySchemes to auth config with secret placeholders. Verify with tests that no credential value is written to disk.
- [x] 1.9 Write the import snapshot and collection metadata hash. Verify that the test asserts both exist after import.
- [x] 1.10 Implement the three-way merge sync, the `_removed/` move, the change summary, `--json` and `--dry-run`. Verify with merge property tests (user-only fields always survive) and a CLI dry-run test showing no file changes.
- [x] 1.11 Document `rustman import openapi` in `docs/import-openapi.md`. Verify the docs example commands run in CI's doc test step.

## 2. Safety policy

- [x] 2.1 Implement `SafetyPolicy` in `rustman-core`: method gating, origin allowlist, the production detection rules, `--production <host>` matching, no destructive on production. Verify with a unit test table covering every gating rule.
- [x] 2.2 Implement `GovernedSender` with manual redirect handling and a policy check per hop, plus the `governor` rate limit, semaphore concurrency, 429/`Retry-After` backoff and probe/time budgets. Verify with integration tests against a local server: an off-host redirect is blocked, a 429 halves the rate, and budgets stop execution.
- [x] 2.3 Add the `[safety]` table to the `rustman.toml` schema (`allowed_hosts`, `production_hosts`, rate, concurrency). Verify with config parsing tests and a docs entry in `docs/safety.md`.

## 3. Exploration engine

- [x] 3.1 Build the dependency graph from `links` plus producer/consumer heuristics, with `[explore.producers]` overrides and `explore plan --graph`. Verify with unit tests on CRUD fixtures, including the unresolvable-parameter case.
- [x] 3.2 Implement the deterministic planner and probe registry with `--probes`, `--operations`, `--seed` and `--fuzz-cases`. Verify that the same seed gives an identical plan (test), and that plan output is snapshot-tested.
- [x] 3.3 Implement `explore plan` (dry run, blocked reasons, `--json`). Verify with an integration test where the local server records zero requests.
- [x] 3.4 Implement detectors: conformance (status plus schema with pointers), negative input, auth, undocumented status and method, leakage regex set, security headers. Verify each against a purpose-built misbehaving fixture server (axum) in `tests/fixtures/bad-api`. (Built as `rustman_test_support::bad_api` on hyper instead, see design D11.)
- [x] 3.5 Implement lifecycle chains and BOLA with `--identity-b`. Verify with fixture server cases for an update not reflected, a resource surviving delete, and a cross-tenant read.
- [x] 3.6 Implement proptest-based fuzz value generation from schemas. Verify with a property test: generated valid-mode values validate, and boundary-mode values hit min/max.
- [x] 3.7 Implement the executor with `journal.ndjson`, created-resource tracking, cleanup ordering and the left-behind list. Verify with an integration test showing cleanup deletes in reverse order with `--allow-destructive` and lists resources without it.
- [x] 3.8 Implement Ctrl+C handling (10 s drain, cleanup, exit 130) and `explore resume` with the document hash check. Verify with an integration test that interrupts via signal (unix) or a cancellation token (windows) and resumes, with 900 probes total.
- [x] 3.9 Implement the coverage matrix, finding de-duplication and severity, evidence capped at 4 KiB, curl reproduction, and `explore export` regression request files. Verify that exported files run with `rustman run` and fail against the bad fixture and pass against a fixed one.
- [x] 3.10 Implement `explore status`, live progress output, and the exit codes 0/1/2/4/130 with `--fail-on`. Verify with CLI tests per exit code.
- [x] 3.11 Write `docs/exploration.md` covering the probe families, rule ids and severities, the safety flags and production guard, and resume. Verify that docs list every rule id in the registry (test).

## 4. Reports

- [x] 4.1 Extend the `Report` types from `bootstrap-core-engine` with `kind: exploration` (additive), regenerate `docs/schemas/report.schema.json` with `schemars`, and add `rustman report schema`. Verify with a test that the generated schema equals the checked-in file and that all produced reports validate.
- [x] 4.2 Move the JSON and JUnit run reporters onto the shared report IR used by the new renderers, without changing their output. Verify the existing `cli-runner` reporter snapshots pass unchanged and old `kind: run` reports still validate.
- [x] 4.3 Implement the renderers: JUnit (exploration mapping), SARIF 2.1.0 (a typed `serde` model, see design D9; level mapping, pointer locations), Markdown and HTML (`minijinja`, inlined assets). Verify with snapshot tests per format, a SARIF schema validation test, and a test that HTML has no external URLs.
- [x] 4.4 Implement `rustman report render` and repeatable `--report <fmt>[=path]` on `run` and `explore run`. Verify with a CLI test writing two formats at once.
- [x] 4.5 Implement `rustman report diff` (new/fixed/unchanged, coverage delta, exit codes, `--format`). Verify with fixture report pairs.
- [x] 4.6 Add a redaction test that renders a report built with known secret values in all five formats and greps for the secret. Verify the test fails if redaction is removed (mutation check documented in the test).
- [ ] 4.7 Write `docs/reports.md` with format examples and CI recipes (GitHub code scanning upload of SARIF). Verify by review.

## 5. Integration

- [ ] 5.1 Run end-to-end against the `bad-api` fixture: import, then `explore run --allow-destructive`, then render all formats, then export regressions, then `rustman run regressions/`. Verify it runs in CI on Linux, macOS and Windows within 5 minutes.
- [x] 5.2 Run `openspec validate add-openapi-explorer --strict`. Verify the command exits 0.

## Workflow follow-up

- Archive the change with `openspec archive add-openapi-explorer` once implemented, so `openapi-import`, `api-exploration` and `test-reports` become living specs.
