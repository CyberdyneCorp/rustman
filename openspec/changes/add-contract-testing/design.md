# Design

## Context

See proposal.md for the motivation. `add-openapi-explorer` already provides an OpenAPI model, `$ref` resolution, schema validation of response bodies, and the `rustman.report/v1` renderers. `bootstrap-core-engine` provides request files, assertions, history and redaction. This change adds a comparison engine and two Pact adapters on top of them. No new transport is needed.

## Building on existing code

- **OpenAPI loading**: `rustman_explore::import` (`crates/rustman-explore/src/openapi/mod.rs`) reads a path or URL, bundles external `$ref` files into one document (`openapi/bundle.rs`), rewrites 3.0 schemas to 3.1 (`openapi/normalize.rs`: `nullable` → `null` type, boolean exclusive bounds), validates (`openapi/validate.rs`, diagnostics with JSON pointers, rendered by `openapi::diagnostic::render`) and lowers into `ApiModel` (`openapi/model.rs`). `Operation::parameters` already merges path-item and operation parameters, and every `Operation` carries its JSON `pointer`. Schemas stay raw with `$ref`s into `ApiModel::components`; D1 only adds cycle-aware inlining for comparison and template matching that ignores parameter names (`validate::template_names` extracts them).
- **Matching exchanges to operations**: `rustman_mock::router::Router` (`crates/rustman-mock/src/router.rs`) is a path-template trie with server base paths stripped and literal-over-parameter precedence. Request parameter and body checks exist in `crates/rustman-mock/src/validate.rs` (`Checks`), and response checks in `crates/rustman-explore/src/explore/detect/conformance.rs` (`documented`, `schema_for`, `conformance`) on `openapi::schema::{validator, errors}`.
- **Safety**: `SafetyPolicy`, `Permission::from_flags`, `Blocked` (`crates/rustman-core/src/safety/`) and `rustman_http::GovernedSender` (`crates/rustman-http/src/governed.rs`). Blocked requests read `blocked: mutations not allowed` / `blocked: destructive not allowed` / `blocked: host not allowed`; policy refusals map to exit 4 (`PolicyError::is_refusal`, `crates/rustman-cli/src/exit.rs`).
- **Reports**: `rustman_report::Report` is a closed enum (`Run`, `Exploration`) and `Report::parse` refuses an unknown `kind` with `unknown report kind '<k>'` (`crates/rustman-report/src/report.rs`); the CLI maps that to exit 2. A new kind needs a variant, its schema part generated with `schemars` in `crates/rustman-report/src/schema.rs` (as `exploration` is), the regenerated `docs/schemas/report.schema.json`, and dispatch in `render/` (`junit.rs`, `sarif.rs`, Markdown/HTML templates). `--report <format>=<path>` parsing exists as `rustman_report::ReportTarget`.
- **Workspace inputs**: request files and resolution (`rustman_core::vars::resolve_request`), redaction (`rustman_core::Redactor`, which matches secret values in their JSON-escaped and other encoded forms, `crates/rustman-core/src/redact/forms.rs`), history (`crates/rustman-core/src/history/`, entries `rustman.history/v1` with ULID ids and `history::query::Query` filters for `--since`/`--grep`), and YAML with line numbers through `rustman_core::format` (for `contracts/allowlist.yaml` errors).

## Goals / Non-Goals

**Goals:**
- Diffs that are deterministic and explainable: the same two documents always give the same ordered list, with each item pointing to a documented rule.
- Pact files that the official Pact verifiers accept, so teams can mix Rustman with existing Pact tooling.
- Passive conformance that is strictly offline.

**Non-Goals:**
- A general JSON Schema equivalence prover. Undecidable composition changes are reported as `unknown`.
- Hosting or brokering contracts.

## Decisions

### D1. Diff on the resolved model, not on raw YAML
Both documents are loaded through `rustman-explore`'s OpenAPI loader, then normalized: `$ref`s inlined (with cycle markers), parameters merged from the path item into each operation, and 3.0 `nullable` mapped to 3.1 type unions. Operations are compared after matching by `(method, normalized path template)`, where `{id}` and `{userId}` count as the same template, with a `path-parameter-renamed` non-breaking note.
*Alternative rejected:* a textual or JSON-tree diff. It reports reorders and `$ref` refactors as changes, and can't tell request schemas from response schemas, which have opposite compatibility rules.

### D2. Direction-aware schema comparison
Schema walks carry a direction. A **request** schema breaks clients when it gets stricter: a new required field, a narrowed enum, a tighter constraint. A **response** schema breaks clients when it gets looser or loses fields: a removed field, a widened type, a widened enum (the client may not handle the new value, so that is classified `unknown`). Each rule is a small struct implementing `Rule { id, classify(ctx, old, new) }`. The catalog version is a constant bumped in the same PR that changes a classification, with a test that asserts the version is bumped whenever a snapshot of classifications changes.
*Alternative rejected:* reusing `oasdiff` (Go) through a subprocess. It would need another runtime and break the single-binary constraint.

### D3. Git access through `gix`
`--base` reads blobs with `gix` (pure Rust): `<ref>:<path>`, and every relative `$ref` file resolved against the same tree. Rustman doesn't shell out to `git`, so it works on Windows without git on PATH and in shallow CI clones that have fetched the base ref.
*Alternative rejected:* `git2`, which needs libgit2 (a C library build with CMake), violating the build constraint.

### D4. Pact model owned in-crate
We serialize Pact v3/v4 JSON with our own serde structs (a small, stable format), and verify using our own implementation of the Pact matching rules (`type`, `regex`, `equality`, `include`, `integer`, `decimal`, `min/max`, `eachLike`).
*Alternative rejected:* `pact_models` / `pact_verifier` crates. They pull in a large dependency tree (including optional native TLS and a protobuf plugin host), and their verifier owns its own HTTP client, which would bypass our safety policy and redaction. We keep conformance with the official verifiers through interop tests against the `pact_verifier_cli` binary in CI only.

### D5. Assertions → Pact matchers
| Rustman assertion | Pact v4 rule |
|---|---|
| `status: 200` | response status 200 |
| `status: "2xx"` | status matcher `statusCode` `successful` (v4), first concrete code from history in v3 |
| `header: {equals}` | equality |
| `header: {matches}` | regex |
| `json: {path, equals}` | equality at path |
| `json: {path, type}` | `type` matcher |
| `json: {path, matches}` | regex |
| `json: {path, length}` | `min`/`max` = length on an `eachLike` |
| `schema`, `latency`, `body_regex` | omitted, with a warning |

Secrets and `{{$dynamic}}` values become `regex` matchers whose example is the redacted placeholder (see spec).

### D6. Provider verification reuses `GovernedSender`
Replays go through `rustman_http::GovernedSender` (the explorer's sender, `crates/rustman-http/src/governed.rs`), with an allowlist built from `--provider-url` and `--provider-states-url`. Its `Governor` limits come from `[safety]` (`rate`, default 10/s; `concurrency`, default 4). Method gating follows the existing flags. Exit code `4` follows the ROADMAP table.

### D7. Passive conformance reads exchanges only
`contract verify` takes exchanges from `request-history` (or from the exchanges inside a run report) and validates them with the same schema validator the explorer's conformance probes use (`rustman_explore::openapi::schema`, via `explore::detect::conformance`). The crate has no access to `rustman-http`'s sender in this code path. A unit test builds the command with a sender that panics, so any accidental network use is caught.

### D8. Reports
A new `kind: contract` with `mode: diff | pact-verify | conformance`, added to the report schema (additive, so it stays `v1`). Readers built before this change refuse it as an unknown kind (exit 2) rather than misreading it. Renderers in `rustman-report` get a contract section. SARIF uses one `reportingDescriptor` per rule id. JUnit maps each difference, interaction or exchange to a testcase.

## Risks / Trade-offs

- [The rule catalog misclassifies an edge case] → Classifications are snapshot-tested against a corpus of real-world spec pairs (Stripe, GitHub, Petstore variants) checked into `fixtures/contract/`. Users can allowlist with a justification while the rule is fixed.
- [Our Pact matcher implementation drifts from the official semantics] → A CI interop job runs `pact_verifier_cli` on the Pact files we generate, against a fixture provider, and diffs the verdicts with ours.
- [Git refs missing in shallow clones] → A clear error suggests `git fetch origin main --depth=1`.
- [Large histories make conformance slow] → Streaming iteration over history files, with `--since` and `--limit` filters.

## Migration Plan

Additive. New subcommands and files only. Report readers built before this change (`Report::parse`) refuse `kind: contract` with `unknown report kind 'contract'` and exit 2; they never misread it.

## Open Questions

- Whether to also accept a Pact Broker URL as a read-only source for `pact verify` in a later change (it doesn't affect v1 specs).
- Where the shared route matching and request validation live: `rustman-contract` depending on `rustman-mock` (a server crate) for `Router` and `Checks`, or moving both into `rustman-explore::openapi` and having the mock server use them from there?
- D3 picks `gix`, a new and sizeable dependency. The workspace already shells out to `git` (`crates/rustman-core/src/secrets/git.rs`, `git check-ignore`). Keep `gix` for Windows/no-git support, or run `git show <ref>:<path>` for consistency?
- `--base` resolves relative `$ref`s at the same ref, but `openapi::bundle` reads `$ref` files through `openapi::source` (disk or HTTP). It needs a pluggable source for Git blobs.
