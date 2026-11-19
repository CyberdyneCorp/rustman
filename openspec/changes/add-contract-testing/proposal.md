# Proposal

## Why

Teams break their API consumers in two ways. They ship an OpenAPI change that removes or narrows something a client relies on, or the running backend drifts away from the spec it claims to implement. Rustman already imports OpenAPI documents (`openapi-import`), runs collections (`cli-runner`) and keeps exchanges (`request-history`), so it can catch both problems in CI without a separate contract-testing stack.

## What Changes

- New `rustman contract diff <old> <new>` that classifies every difference between two OpenAPI documents as `breaking`, `non-breaking` or `unknown`, using a documented, versioned rule catalog. It exits `1` on unallowlisted breaking changes.
- A Git-aware mode, `rustman contract diff --base <ref> <path>`, that reads the old document from a Git ref, so it drops into a PR pipeline.
- An allowlist file, `contracts/allowlist.yaml`. Each entry needs a justification and may carry an expiry date.
- Consumer-driven contracts:
  - `rustman contract pact generate` writes Pact Specification v3 or v4 JSON from a collection's requests and assertions (consumer side).
  - `rustman contract pact verify` replays Pact interactions against a provider, setting provider states through a configurable state-change endpoint (provider side).
- Passive conformance: `rustman contract verify` checks that recorded exchanges (history, or a run report) conform to an OpenAPI document, **without sending any new request**.
- All results are emitted as `rustman.report/v1` reports and rendered through `test-reports` (Markdown for PR comments, JSON, JUnit and SARIF).

## Capabilities

### New Capabilities
- `contract-testing`: OpenAPI breaking-change detection with a rule catalog and allowlist, Git-aware diffs, Pact contract generation and provider verification, and passive conformance of recorded traffic against an OpenAPI document.

### Modified Capabilities
None. This change uses `openapi-import` (parsing, `$ref` resolution, validation diagnostics), `test-reports` (rendering, with an additive `kind: contract` per its schema-versioning rule), `request-history`, `assertions`, `environments` (redaction) and the safety policy from `api-exploration`, all unchanged.

## Out of scope

- Pact Broker integration (publishing, can-i-deploy, webhooks). Pact files are read from and written to local paths only in v1. Users can push them with the official `pact-broker` CLI.
- Message (asynchronous) Pact interactions, and Pact plugins.
- Contract diffs for GraphQL schemas, JSON-RPC/OpenRPC, gRPC or AsyncAPI. Only OpenAPI 3.0/3.1 is in scope.
- Automatically fixing a spec or generating migration guides.

## Impact

- New crate `crates/rustman-contract` (diff engine, rule catalog, Pact model, conformance checker). It depends on `rustman-core`, `rustman-explore` (OpenAPI model and schema validation), `rustman-http` and `rustman-report`.
- New CLI subcommand tree `rustman contract …` in `rustman-cli`.
- New workspace files: `contracts/allowlist.yaml` and `contracts/pacts/*.json` (both meant to be committed).
- `rustman.report/v1` gains the additive `kind: contract` and its sections. The published JSON Schema is updated; older readers refuse the unknown kind with exit 2 (`unknown report kind 'contract'`).
- Documentation: a rule-catalog page listing every rule id, its classification and an example.
