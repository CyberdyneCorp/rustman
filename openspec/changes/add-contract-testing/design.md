# Design

## Context

See proposal.md for the motivation. `add-openapi-explorer` already provides an OpenAPI model, `$ref` resolution, schema validation of response bodies, and the `rustman.report/v1` renderers. `bootstrap-core-engine` provides request files, assertions, history and redaction. This change adds a comparison engine and two Pact adapters on top of them. No new transport is needed.

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
Replays go through `GovernedSender` (from `add-openapi-explorer`), with an allowlist built from `--provider-url` and `--provider-states-url`. Method gating follows the existing flags. Exit code `4` follows the ROADMAP table.

### D7. Passive conformance reads exchanges only
`contract verify` takes exchanges from `request-history` (or from the exchanges inside a run report) and validates them with the same schema validator the explorer's conformance probes use. The crate has no access to `rustman-http`'s sender in this code path. A unit test builds the command with a sender that panics, so any accidental network use is caught.

### D8. Reports
A new `kind: contract` with `mode: diff | pact-verify | conformance`, added to the report schema (additive, so it stays `v1`). Renderers in `rustman-report` get a contract section. SARIF uses one `reportingDescriptor` per rule id. JUnit maps each difference, interaction or exchange to a testcase.

## Risks / Trade-offs

- [The rule catalog misclassifies an edge case] → Classifications are snapshot-tested against a corpus of real-world spec pairs (Stripe, GitHub, Petstore variants) checked into `fixtures/contract/`. Users can allowlist with a justification while the rule is fixed.
- [Our Pact matcher implementation drifts from the official semantics] → A CI interop job runs `pact_verifier_cli` on the Pact files we generate, against a fixture provider, and diffs the verdicts with ours.
- [Git refs missing in shallow clones] → A clear error suggests `git fetch origin main --depth=1`.
- [Large histories make conformance slow] → Streaming iteration over history files, with `--since` and `--limit` filters.

## Migration Plan

Additive. New subcommands and files only. Report readers built before this change ignore `kind: contract` as an unknown kind, per `test-reports` versioning.

## Open Questions

- Whether to also accept a Pact Broker URL as a read-only source for `pact verify` in a later change (it doesn't affect v1 specs).
