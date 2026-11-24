# contract-testing Specification

## Purpose
Catches the two ways an API breaks its consumers, in CI and without a separate contract-testing stack: OpenAPI changes that remove or narrow what clients rely on (breaking-change detection with a rule catalog, an allowlist and Git-aware diffs), and a backend drifting from its spec (Pact generation and provider verification, and passive conformance of recorded traffic).

## Requirements

### Requirement: OpenAPI diff command
`rustman contract diff <old> <new>` SHALL compare two OpenAPI 3.0/3.1 documents (file paths or URLs, loaded and validated as in `openapi-import`) operation by operation, and SHALL report every difference with a rule id, a classification (`breaking`, `non-breaking` or `unknown`), the operation (`METHOD /path`) and a JSON pointer into each document.

#### Scenario: Two versions compared
- **GIVEN** `v1.yaml` and `v2.yaml` where v2 adds `GET /health` and removes `DELETE /users/{id}`
- **WHEN** the user runs `rustman contract diff v1.yaml v2.yaml`
- **THEN** the output SHALL list `operation-added` (non-breaking) for `GET /health`
- **AND** `operation-removed` (breaking) for `DELETE /users/{id}`, with JSON pointers into both documents

#### Scenario: Invalid document
- **GIVEN** `v2.yaml` fails structural validation
- **WHEN** the diff runs
- **THEN** the diagnostics from `openapi-import` SHALL be printed and the exit code SHALL be 2

### Requirement: Breaking rule catalog
The catalog SHALL classify at least these as `breaking`:
- an operation is removed;
- a response status is removed (no range or `default` covers it);
- a response field is removed or renamed;
- a response field's type changes;
- a new required request parameter or body field appears;
- an enum is narrowed;
- a request constraint gets stricter (`maxLength`, `pattern`, `additionalProperties`);
- a parameter moves location;
- every alternative of a security requirement is removed.

#### Scenario: New required request field
- **GIVEN** v2 adds `email` to the `required` list of the `POST /users` request body
- **WHEN** the diff runs
- **THEN** a `request-required-field-added` difference SHALL be reported as `breaking`

#### Scenario: Narrowed enum
- **GIVEN** the response field `status` has enum `[active, suspended, deleted]` in v1 and `[active, suspended]` in v2
- **WHEN** the diff runs
- **THEN** `response-enum-narrowed` SHALL be reported as `breaking`

#### Scenario: Request schema closed
- **GIVEN** the `POST /users` request body schema gains `additionalProperties: false`
- **WHEN** the diff runs
- **THEN** `request-constraint-tightened` SHALL be reported as `breaking`

#### Scenario: Status replaced by its range
- **GIVEN** `GET /users` declares `200` in v1 and `2XX` with the same content in v2
- **WHEN** the diff runs
- **THEN** no `response-status-removed` difference SHALL be reported

#### Scenario: Parameter location change
- **GIVEN** `api_key` moves from a query parameter to a header
- **WHEN** the diff runs
- **THEN** `parameter-location-changed` SHALL be reported as `breaking`

### Requirement: Non-breaking and unknown classification
These SHALL be classified as `non-breaking`:
- an operation is added;
- an optional request field is added;
- a response field is added (unless the response schema sets `additionalProperties: false`);
- an enum is widened in a request;
- a request constraint gets looser, or a response one stricter;
- descriptions or examples change.

Changes the catalog can't decide, such as `oneOf`/`anyOf` edits, vendor extensions or a looser response constraint, SHALL be `unknown`.

#### Scenario: Added optional field
- **GIVEN** v2 adds an optional `nickname` to the `POST /users` body
- **THEN** `request-optional-field-added` SHALL be reported as `non-breaking`

#### Scenario: Looser response constraint
- **GIVEN** a response schema drops `additionalProperties: false`
- **THEN** `response-constraint-loosened` SHALL be reported as `unknown`

#### Scenario: Composition change
- **GIVEN** a response schema changes from `oneOf: [A, B]` to `oneOf: [A, B, C]`
- **THEN** the difference SHALL be reported as `unknown` with rule id `composition-changed`

### Requirement: Rule catalog is documented and versioned
Every rule SHALL have a stable kebab-case id, a classification, a one-line description and an example. `rustman contract rules` SHALL list them as a table, or as JSON with `--json`. Changing a rule's classification SHALL bump the catalog version recorded in every diff report.

#### Scenario: Listing rules
- **WHEN** the user runs `rustman contract rules --json`
- **THEN** the output SHALL be a JSON array with `id`, `classification`, `description` and `example` for every rule
- **AND** it SHALL include the catalog version

### Requirement: Diff exit codes and strictness
The diff SHALL exit `0` when there are no unallowlisted breaking differences, and `1` when there is at least one. `unknown` differences SHALL NOT fail the command unless `--fail-on-unknown` is given. `--fail-on non-breaking` SHALL make any difference fail (for frozen APIs).

#### Scenario: Breaking change fails CI
- **GIVEN** an unallowlisted `operation-removed` difference
- **WHEN** the diff runs
- **THEN** the exit code SHALL be 1

#### Scenario: Unknown does not fail by default
- **GIVEN** only `composition-changed` differences
- **WHEN** the diff runs without flags
- **THEN** the exit code SHALL be 0
- **AND** with `--fail-on-unknown` the exit code SHALL be 1

### Requirement: Allowlist with justification
`contracts/allowlist.yaml` SHALL list accepted differences by `rule` and `operation` (optionally a `pointer`), each with a non-empty `justification` and an optional `expires` date. Allowlisted differences SHALL still be reported, marked `allowed`. An entry without a justification SHALL fail with exit code 2.

#### Scenario: Allowed removal
- **GIVEN** an allowlist entry `{rule: operation-removed, operation: "DELETE /users/{id}", justification: "Deprecated since 2026-06, no traffic"}`
- **WHEN** the diff finds that removal
- **THEN** it SHALL be reported as `breaking (allowed)` and the exit code SHALL be 0

#### Scenario: Missing justification
- **GIVEN** an allowlist entry without `justification`
- **WHEN** the diff runs
- **THEN** it SHALL fail with `contracts/allowlist.yaml:<line>: justification is required` and exit code 2

### Requirement: Expired allowlist entries
An allowlist entry whose `expires` date is earlier than the current date SHALL NOT allow anything. The diff SHALL warn `allowlist entry expired on <date>` for it and treat the matching difference as unallowlisted.

#### Scenario: Expired entry
- **GIVEN** an entry with `expires: 2026-01-01`, today being 2026-11-17, matching a breaking difference
- **WHEN** the diff runs
- **THEN** the expiry warning SHALL be printed and the exit code SHALL be 1

#### Scenario: Unused entry
- **GIVEN** an allowlist entry that matches no difference
- **WHEN** the diff runs
- **THEN** a warning `unused allowlist entry` SHALL be printed and the exit code SHALL be unaffected

### Requirement: Git-aware base
`rustman contract diff --base <git-ref> <path>` SHALL read the old document from `<git-ref>:<path>` in the repository containing the workspace, and the new document from the working tree. Relative `$ref` files SHALL be resolved at the same ref. A missing ref or path SHALL fail with exit code 2.

#### Scenario: PR pipeline
- **GIVEN** `openapi.yaml` modified on a feature branch
- **WHEN** CI runs `rustman contract diff --base origin/main openapi.yaml`
- **THEN** the document from `origin/main` SHALL be compared with the working-tree version

#### Scenario: File added on the branch
- **GIVEN** `openapi.yaml` does not exist at `origin/main`
- **WHEN** the diff runs with `--base origin/main`
- **THEN** it SHALL fail with `openapi.yaml not found at origin/main` and exit code 2

### Requirement: Contract reports
Every `contract` command SHALL be able to write a `rustman.report/v1` report with `kind: contract`, an additive extension under `test-reports` versioning. It SHALL support `--report <format>=<path>` for `json`, `markdown`, `junit` and `sarif`, and default to a human summary on stdout.

#### Scenario: PR comment
- **WHEN** the user runs `rustman contract diff --base origin/main openapi.yaml --report markdown=out/contract.md`
- **THEN** `out/contract.md` SHALL contain a summary line with counts per classification
- **AND** a table of breaking differences first, each with rule id, operation and a short description

#### Scenario: SARIF location
- **GIVEN** a breaking difference
- **WHEN** rendered as SARIF
- **THEN** the result SHALL be at level `error`, with a location pointing to the new document and the JSON pointer of the changed element

### Requirement: Pact generation from a collection
`rustman contract pact generate <collection> --consumer <name> --provider <name>` SHALL write one Pact file per consumer–provider pair, to `contracts/pacts/<consumer>-<provider>.json` by default. Each `http` request becomes one interaction. The request comes from the request file with variables resolved and secrets replaced by Pact matchers. The response expectations come from its assertions.

#### Scenario: Interaction from a request
- **GIVEN** `get-user.rman.yaml` with `GET {{base_url}}/users/{{user_id}}`, `- status: 200` and `- json: {path: "$.id", equals: 42}`
- **WHEN** a Pact file is generated
- **THEN** it SHALL contain an interaction with request `GET /users/42`, response status 200 and a body rule for `$.id` equal to 42

#### Scenario: Secret header becomes a matcher
- **GIVEN** a header `Authorization: Bearer {{token}}` where `token` is a secret
- **WHEN** a Pact file is generated
- **THEN** the header SHALL be written with a regex matcher `^Bearer .+$` and an example value `Bearer «redacted:token»`, never the real token

### Requirement: Pact specification versions
Generated Pact files SHALL default to Pact Specification v4, and SHALL produce v3 with `--pact-version 3`. Assertions without a faithful Pact equivalent (latency, JSON Schema) SHALL be omitted, with one warning per assertion naming the request and assertion. Requests of kinds other than `http` SHALL be skipped with a warning.

#### Scenario: Latency assertion skipped
- **GIVEN** a request asserting `latency: {total_lt: 300}`
- **WHEN** a Pact file is generated
- **THEN** a warning `latency assertion has no Pact equivalent (get-user.rman.yaml)` SHALL be printed and the interaction SHALL still be written

#### Scenario: v3 output
- **WHEN** generation runs with `--pact-version 3`
- **THEN** the file's `metadata.pactSpecification.version` SHALL be `3.0.0`

### Requirement: Provider verification
`rustman contract pact verify <pact-file>... --provider-url <url>` SHALL replay each interaction against the provider and compare status, headers and body using the Pact matching rules. Each mismatch SHALL be reported with the interaction description and the JSON path. The exit code SHALL be `0` when all interactions pass, and `1` otherwise.

#### Scenario: Body mismatch
- **GIVEN** an interaction expecting `$.id` equal to 42 and a provider returning `{"id": "42"}`
- **WHEN** verification runs
- **THEN** the mismatch SHALL be reported as `$.id: expected number 42, got string "42"` and the exit code SHALL be 1

#### Scenario: All interactions pass
- **GIVEN** a provider that satisfies every interaction
- **WHEN** verification runs
- **THEN** the exit code SHALL be 0 and the summary SHALL show every interaction as passed

### Requirement: Provider states
When an interaction declares provider states, verification SHALL POST `{"consumer", "state", "params", "action": "setup"}` to `--provider-states-url` before the interaction, and the same body with `"action": "teardown"` afterwards. A missing URL with stateful interactions SHALL fail with exit code 2. A non-2xx setup response SHALL fail that interaction.

#### Scenario: State setup
- **GIVEN** an interaction with state `user 42 exists` and `--provider-states-url http://localhost:8080/_pact/state`
- **WHEN** verification runs
- **THEN** a setup POST naming `user 42 exists` SHALL be sent before the interaction and a teardown POST after it

#### Scenario: State URL missing
- **GIVEN** an interaction with provider states and no `--provider-states-url`
- **WHEN** verification runs
- **THEN** it SHALL fail with `provider states declared but --provider-states-url not set` and exit code 2

### Requirement: Verification passes the safety policy
Provider verification SHALL send every request, including state-change calls, through the safety policy from `api-exploration`. The provider and state-change URLs form the host allowlist. Mutating interactions (POST, PUT, PATCH) SHALL require `--allow-mutations`, DELETE SHALL require `--allow-destructive`, and blocked interactions SHALL be reported as `blocked: <reason>` with exit code 4.

#### Scenario: Mutating interaction blocked
- **GIVEN** a Pact file with a `POST /users` interaction
- **WHEN** verification runs without `--allow-mutations`
- **THEN** that interaction SHALL be reported as `blocked: mutations not allowed` and the exit code SHALL be 4

### Requirement: Passive conformance of recorded traffic
`rustman contract verify --spec <openapi> [--from history|--from <run-report.json>]` SHALL check recorded exchanges against the document and send no network request. History is the default source, filterable with `--since` and `--grep`. Each exchange SHALL be matched to an operation by method and path template. Its request and response SHALL be validated against the operation's parameters and schemas.

#### Scenario: Response violates schema
- **GIVEN** a recorded `GET /users/7` exchange whose response lacks the required field `email`
- **WHEN** the user runs `rustman contract verify --spec openapi.yaml`
- **THEN** a violation SHALL be reported naming the exchange id, the operation `GET /users/{id}` and `$.email: required property missing`

#### Scenario: No network traffic
- **GIVEN** any input
- **WHEN** `rustman contract verify` runs
- **THEN** the system SHALL open no network connection

### Requirement: Conformance coverage and unmatched traffic
Passive conformance SHALL report:
- exchanges matching no operation, as `undocumented`;
- the documented operations that the recorded traffic exercised, with their count;
- a summary of violations per operation.

The exit code SHALL be `1` when there is any violation, or any `undocumented` exchange when `--fail-on-undocumented` is set. Otherwise it SHALL be `0`.

#### Scenario: Undocumented endpoint
- **GIVEN** recorded traffic to `GET /internal/debug`, which the document does not declare
- **WHEN** verification runs
- **THEN** the exchange SHALL be listed as `undocumented`
- **AND** the exit code SHALL be 0 unless `--fail-on-undocumented` is given

#### Scenario: Coverage summary
- **GIVEN** a document with 12 operations and history touching 5 of them
- **WHEN** verification runs
- **THEN** the summary SHALL read `5/12 operations observed`

### Requirement: Redaction in contract outputs
Contract reports, generated Pact files, diff output and conformance evidence SHALL go through the `environments` redaction step. No secret value SHALL appear in any file this capability writes.

#### Scenario: Echoed secret in evidence
- **GIVEN** a recorded exchange whose response body echoes a secret token
- **WHEN** conformance evidence is written to a report
- **THEN** the token SHALL appear as `«redacted:token»`
