# Tasks

## 1. Crate scaffold and normalized OpenAPI model

- [ ] 1.1 Create `crates/rustman-contract` (depends on `rustman-core`, `rustman-explore`, `rustman-report`) and add `rustman contract --help` to `rustman-cli`; verify `cargo build` and that `rustman contract --help` lists `diff`, `rules`, `pact`, `verify`.
- [ ] 1.2 Implement normalization: inline `$ref` with cycle markers, merge path-item parameters, map 3.0 `nullable` to 3.1 unions, match path templates ignoring parameter names; verify with unit tests on fixture pairs (renamed path param, shared parameters, nullable).

## 2. Diff engine and rule catalog

- [ ] 2.1 Implement the `Rule` trait and direction-aware schema walker; verify unit tests per direction (request stricter = breaking, response field removed = breaking).
- [ ] 2.2 Implement every breaking rule from the spec (operation/status removed, response field removed/renamed/type changed, request required field added, enum narrowed, stricter constraints, parameter location changed, security alternatives removed); verify one fixture pair and test per rule id.
- [ ] 2.3 Implement non-breaking and `unknown` rules (`operation-added`, optional field added, loosened constraints, docs-only, `composition-changed`, vendor extensions); verify tests per rule id.
- [ ] 2.4 Add the catalog version constant and a snapshot test of all classifications that fails unless the version changes with them; add `rustman contract rules [--json]`; verify the JSON output against the spec scenario.
- [ ] 2.5 Build the real-world corpus in `fixtures/contract/` (Petstore, Stripe, GitHub version pairs) with snapshot expectations; verify `cargo test -p rustman-contract corpus` passes deterministically across 3 runs.
- [ ] 2.6 Write `docs/contract-rules.md` generated from the catalog (a test checks the doc is up to date).

## 3. CLI diff, allowlist and Git base

- [ ] 3.1 Implement `rustman contract diff <old> <new>` with human output, exit codes 0/1/2, `--fail-on-unknown` and `--fail-on non-breaking`; verify CLI integration tests per scenario.
- [ ] 3.2 Implement `contracts/allowlist.yaml` parsing (justification required, `expires`, unused-entry warning, expired-entry warning); verify tests for each scenario including line-numbered errors.
- [ ] 3.3 Implement `--base <ref>` with `gix`, resolving relative `$ref` at the same tree; verify tests against a temporary git repo with two commits, a missing path, and a missing ref.

## 4. Reports

- [ ] 4.1 Add `kind: contract` (`mode: diff | pact-verify | conformance`) to the `rustman.report/v1` schema and types; verify the published JSON Schema validates generated reports and an older-schema reader ignores the kind.
- [ ] 4.2 Add contract sections to Markdown, JUnit and SARIF renderers and `--report fmt=path` to all contract commands; verify insta snapshots per format and a SARIF validation against the 2.1.0 schema.

## 5. Pact generation

- [ ] 5.1 Implement Pact v3/v4 serde models and the assertion→matcher mapping from design D5; verify unit tests per mapping row and omitted-assertion warnings.
- [ ] 5.2 Implement `rustman contract pact generate` (variable resolution, secret/dynamic values to regex matchers, non-http kinds skipped, `--pact-version`); verify generated files with a test that no secret value appears, and against the Pact JSON schema.

## 6. Pact provider verification

- [ ] 6.1 Implement Pact matching rules for verification (type, regex, equality, include, integer, decimal, min/max, eachLike) with JSON-path mismatch messages; verify unit tests per rule.
- [ ] 6.2 Implement `rustman contract pact verify` through `GovernedSender` with provider-state setup/teardown and safety gating (exit 4 on blocked); verify integration tests with a local fixture provider and state endpoint, including the missing-URL and blocked-POST scenarios.
- [ ] 6.3 Add a CI interop job running the official `pact_verifier_cli` on our generated Pact files against the fixture provider; verify verdicts match ours.

## 7. Passive conformance

- [ ] 7.1 Implement `rustman contract verify --spec` reading history (with `--since`, `--grep`) or a run report, matching exchanges to operations and validating parameters and schemas; verify tests for violation, undocumented and coverage scenarios.
- [ ] 7.2 Prove the no-network invariant: build the command with a sender that panics on use; verify the full conformance test suite passes with it.
- [ ] 7.3 Add redaction tests across diff output, Pact files and conformance evidence (secret echoed in a body); verify no secret value appears in any written file.
- [ ] 7.4 Document `rustman contract` in the user guide (diff in a GitHub Actions PR workflow, Pact consumer/provider flow, passive conformance); verify the doc examples run in a doc test.
