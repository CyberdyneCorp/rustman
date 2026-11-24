# api-exploration Specification

## Purpose
Given an OpenAPI document and a target, investigates the backend automatically. It discovers resources and their dependencies, generates and runs probes for conformance, robustness and security, and reports coverage and findings with reproducible evidence, all under a safety policy that keeps it from harming the system under test.

## Requirements

### Requirement: Exploration lifecycle commands
The system SHALL provide `rustman explore plan`, `explore run`, `explore resume <id>` and `explore status [<id>]`. Each exploration SHALL get an id (`exp-<YYYYMMDD>-<6 hex>`), and its state, journal and report SHALL live in `.rustman/explorations/<id>/`.

#### Scenario: Run creates an exploration directory
- **WHEN** the user runs `rustman explore run --spec api.yaml --target https://staging.example.com`
- **THEN** a directory `.rustman/explorations/exp-<date>-<hex>/` SHALL be created containing `state.json`, `journal.ndjson` and, on completion, `report.json`

### Requirement: Exploration phases
An exploration SHALL run four phases in order: discover, plan, execute, report. The current phase and its progress (probes done/total, findings so far, elapsed time) SHALL be shown on the terminal, and `explore status` SHALL show them for a running or finished exploration.

#### Scenario: Status of a running exploration
- **GIVEN** an exploration in its execute phase
- **WHEN** the user runs `rustman explore status <id>`
- **THEN** the output SHALL show phase `execute`, probes done/total and the count of findings by severity

### Requirement: Dependency discovery
Discovery SHALL build a resource graph from OpenAPI `links` and from these heuristics: an operation whose response contains a field named `id` or `<resource>Id` is a producer for path parameters of the same name in other operations under the same path prefix. The graph SHALL be stored in the exploration state and printed by `explore plan --graph`.

#### Scenario: POST produces id for item operations
- **GIVEN** `POST /users` returning `{ "id": ... }` and operations `GET/PUT/DELETE /users/{id}`
- **WHEN** discovery runs
- **THEN** `POST /users` SHALL be recorded as the producer of `id` for those three operations

#### Scenario: Unresolvable parameter
- **GIVEN** `GET /orders/{orderId}` with no producer and no value in the active environment
- **WHEN** planning runs
- **THEN** probes needing a real `orderId` SHALL be marked `skipped: no producer` and listed in the report

### Requirement: Dry-run planning
`rustman explore plan` SHALL execute discover and plan only and SHALL NOT send any request to the target. It SHALL print every planned probe (id, family, operation, method, expected outcome, and whether it's blocked by the safety policy, and why), with `--json` for machine output.

#### Scenario: Plan sends nothing
- **GIVEN** a target whose server records all traffic
- **WHEN** the user runs `rustman explore plan --spec api.yaml --target <url>`
- **THEN** the server SHALL receive zero requests
- **AND** the output SHALL list the planned probes with their blocked or allowed status

### Requirement: Probe families
The planner SHALL support these probe families, selectable with `--probes <list>` and enabled by default unless stated: `conformance`, `negative-input`, `auth`, `undocumented-status`, `method-not-allowed`, `error-leakage`, `security-headers`, `lifecycle`, `fuzz`, and `bola` (off by default; requires a second identity).

#### Scenario: Select families
- **WHEN** the user runs `explore run --probes conformance,auth ...`
- **THEN** only probes of the `conformance` and `auth` families SHALL be planned

### Requirement: Conformance probes
For each operation with resolvable inputs, a conformance probe SHALL send a valid request. It SHALL produce a finding if the status isn't documented for that operation, or if the response body violates the documented schema for its status and media type. JSON Schema violations SHALL cite the instance and schema pointers.

#### Scenario: Response schema violation
- **GIVEN** `GET /users/{id}` documents `email` as a required string
- **WHEN** the backend returns a user without `email`
- **THEN** a `medium` finding `response-schema-violation` SHALL be recorded with the instance pointer and the missing property

### Requirement: Negative input probes
Negative-input probes SHALL send requests that omit required fields or parameters, use wrong types, violate bounds, length, pattern or enum, or exceed size. A 5xx response SHALL produce a `high` finding `server-error-on-invalid-input`. A 2xx response SHALL produce a `medium` finding `invalid-input-accepted`. 4xx is the expected outcome.

#### Scenario: Missing required field causes 500
- **GIVEN** `POST /users` requires `name`
- **WHEN** a probe omits `name` and the backend answers 500
- **THEN** a `high` finding `server-error-on-invalid-input` SHALL be recorded

### Requirement: Auth enforcement probes
For each operation with a security requirement, auth probes SHALL send the request with no credentials and with a syntactically valid but invalid credential. Any 2xx response SHALL produce a `critical` finding `auth-not-enforced`. Responses other than 401/403 SHALL produce a `low` finding `unexpected-auth-status`.

#### Scenario: Endpoint ignores missing token
- **GIVEN** `GET /admin/users` requires `bearerAuth`
- **WHEN** a probe without the Authorization header receives 200
- **THEN** a `critical` finding `auth-not-enforced` SHALL be recorded

### Requirement: BOLA probes with two identities
When `--identity-b <env-or-profile>` is given and `bola` is enabled, the system SHALL create or read a resource as identity A, then access it by id as identity B. A 2xx response with the resource SHALL produce a `critical` finding `broken-object-level-authorization`. Without a second identity, BOLA probes SHALL be skipped with a reason.

#### Scenario: Cross-tenant read
- **GIVEN** identity A created order `o-1`
- **WHEN** identity B requests `GET /orders/o-1` and receives 200 with that order
- **THEN** a `critical` finding `broken-object-level-authorization` SHALL be recorded

### Requirement: Undocumented status and method probes
The system SHALL record an `info` finding `undocumented-status` for every observed status not documented for its operation. For each documented path, it SHALL send one undocumented method (from GET/HEAD/OPTIONS when read-only) and expect 405 or 404. A 2xx response SHALL produce a `medium` finding `undocumented-method-allowed`.

#### Scenario: Undocumented method allowed
- **GIVEN** `/users` documents only GET and POST, and mutations are allowed
- **WHEN** a probe sends `PUT /users` and receives 200
- **THEN** a `medium` finding `undocumented-method-allowed` SHALL be recorded

### Requirement: Error leakage and security header probes
Every response observed during an exploration SHALL be scanned for leaked internals (stack traces, SQL error text, framework debug pages, internal file paths), which produce a `medium` finding `error-leakage`. It SHALL also be checked for missing `Strict-Transport-Security` (HTTPS only), `X-Content-Type-Options` and permissive CORS (`*` with credentials), which produce `low` findings.

#### Scenario: Stack trace in response
- **GIVEN** a 500 response body containing `at com.example.UserService.find(UserService.java:42)`
- **WHEN** the response is scanned
- **THEN** a `medium` finding `error-leakage` SHALL be recorded with the matched excerpt

### Requirement: Lifecycle chains
When mutations are allowed, lifecycle probes SHALL run create → read → update → read → delete → read chains along the discovered graph, passing captured ids between steps. They SHALL record a finding when a read after update doesn't reflect the update (`medium`), or when a read after delete doesn't return 404/410 (`medium`).

#### Scenario: Deleted resource still readable
- **GIVEN** a chain created and then deleted `/users/42` with `--allow-destructive`
- **WHEN** `GET /users/42` returns 200
- **THEN** a `medium` finding `resource-survives-delete` SHALL be recorded

### Requirement: Reproducible fuzzing
Fuzz probes SHALL generate schema-shaped and boundary inputs from a seed. The seed SHALL be printed and stored in the report, and `--seed <u64>` SHALL reproduce the identical probe sequence. The default cap is 50 fuzz cases per operation (`--fuzz-cases`).

#### Scenario: Same seed same probes
- **GIVEN** two plans with `--seed 1234` against the same document
- **WHEN** their probe lists are compared
- **THEN** they SHALL be identical

### Requirement: Safety policy method gating
Exploration requests SHALL pass the safety policy. By default only GET, HEAD and OPTIONS are allowed. `--allow-mutations` SHALL additionally allow POST, PUT and PATCH. DELETE SHALL require `--allow-destructive`, which implies `--allow-mutations`. Blocked probes SHALL be reported as `blocked: <reason>`, not silently dropped.

#### Scenario: Default run is read-only
- **GIVEN** a document with GET, POST and DELETE operations
- **WHEN** the user runs `explore run` without flags
- **THEN** no POST, PUT, PATCH or DELETE request SHALL be sent
- **AND** those probes SHALL appear in the report as `blocked: mutations not allowed`

#### Scenario: Mutations without destructive
- **WHEN** the user runs `explore run --allow-mutations`
- **THEN** POST, PUT and PATCH probes SHALL run and DELETE probes SHALL be `blocked: destructive not allowed`

### Requirement: Safety policy host allowlist
The allowlist SHALL default to the scheme, host and port of the target. Requests to any other origin, including redirects and URLs built from response data, SHALL be refused and recorded as `blocked: host not allowed`. Extra origins SHALL be added only through `[safety].allowed_hosts` in `rustman.toml` or `--allow-host`.

#### Scenario: Redirect off target
- **GIVEN** a target endpoint that redirects to `https://other.example.net/`
- **WHEN** a probe follows the response
- **THEN** the redirect SHALL NOT be followed and the probe SHALL be recorded as `blocked: host not allowed`

### Requirement: Safety policy rate, concurrency and budgets
Explorations SHALL default to 10 requests/second, 4 concurrent requests, a 2000-probe budget and a 15-minute time budget (`--rate`, `--concurrency`, `--max-probes`, `--max-duration`). When a budget is reached, execution SHALL stop and report the remaining probes as `not run: budget`. A 429 response SHALL halve the rate and honor `Retry-After`; 429 responses to requests sent at the same rate (for example, the requests in flight together) SHALL halve it only once.

#### Scenario: Probe budget reached
- **GIVEN** a plan of 3000 probes and default budgets
- **WHEN** the exploration runs
- **THEN** at most 2000 probes SHALL be sent and 1000 SHALL be reported as `not run: budget`

#### Scenario: Backoff on 429
- **WHEN** the target answers 429 with `Retry-After: 5`
- **THEN** no request SHALL be sent for 5 seconds and the rate SHALL drop to 5 req/s

### Requirement: Production guard
A target SHALL be treated as production when its environment is marked `production: true`, or its host matches `[safety].production_hosts`, or its environment is named `prod` or `production`. Against production, `explore run` SHALL refuse with exit code 4 unless `--production <host>` repeats the exact target host. Even then, `--allow-destructive` SHALL be refused.

#### Scenario: Production refused
- **GIVEN** the active environment is `production`
- **WHEN** the user runs `rustman explore run --target https://api.example.com`
- **THEN** the command SHALL exit with code 4 explaining the production guard, and no request SHALL be sent

#### Scenario: Explicit production read-only
- **WHEN** the user runs the same command with `--production api.example.com`
- **THEN** read-only probes SHALL run
- **AND** adding `--allow-destructive` SHALL be refused with exit code 4

### Requirement: Cleanup of created resources
Every resource created by a probe and identified by a captured id SHALL be recorded in the journal. At the end of execution, or on interrupt, the system SHALL attempt to delete those resources in reverse creation order if `--allow-destructive` was given, and SHALL otherwise list them as `left behind` in the report.

#### Scenario: Cleanup after the budget ran out
- **GIVEN** a run with `--allow-destructive` whose probe budget ran out after its probes created users
- **WHEN** the run completes
- **THEN** the cleanup deletes SHALL still be sent, checked by the safety policy and paced, but not counted against or refused by the budgets

#### Scenario: Leftovers listed
- **GIVEN** a run with `--allow-mutations` but not `--allow-destructive` that created 3 users
- **WHEN** the run completes
- **THEN** the report SHALL list the 3 user URLs under `left behind`

### Requirement: Coverage matrix
The report SHALL include coverage as a matrix of operations × documented status codes, marking each cell observed, not observed or blocked. It SHALL also give a summary of operations exercised over the total and documented statuses observed over the total, as percentages.

#### Scenario: Coverage summary
- **GIVEN** 10 operations documenting 25 statuses, of which 8 operations and 15 statuses were observed
- **WHEN** the report is generated
- **THEN** it SHALL state operation coverage 80% and status coverage 60%

### Requirement: Findings with evidence and severity
Each finding SHALL have a stable rule id, a severity (`critical`, `high`, `medium`, `low`, `info`), the operation, a description, and the redacted evidence exchange (request, response status/headers, body excerpt ≤ 4 KiB). Identical findings on the same operation and rule SHALL be de-duplicated with an occurrence count.

#### Scenario: Duplicate findings collapsed
- **GIVEN** 12 fuzz probes on `POST /users` each triggering `server-error-on-invalid-input`
- **WHEN** the report is generated
- **THEN** one finding SHALL be listed with occurrences 12 and the first evidence exchange

### Requirement: Reproduction artifacts
Each finding SHALL include a curl command reproducing its evidence request, with secrets kept as `{{variable}}` references. `rustman explore export <id> --findings <ids|all> --into <collection>` SHALL write one request file per finding, with assertions that encode the expected correct behavior, for use as a regression suite with `rustman run`.

#### Scenario: Export regression request
- **GIVEN** finding `auth-not-enforced` on `GET /admin/users`
- **WHEN** the user exports it into `regressions/`
- **THEN** a request file SHALL be created that sends no credentials and asserts status is 401 or 403

### Requirement: Interrupt and resume
On Ctrl+C the system SHALL stop dispatching probes, wait up to 10 s for in-flight requests, run cleanup per policy, persist state and exit with code 130. `explore resume <id>` SHALL continue with the probes not yet run, using the same seed, safety flags and budgets, and SHALL refuse if the document hash has changed.

#### Scenario: Resume after interrupt
- **GIVEN** an exploration interrupted after 400 of 900 probes
- **WHEN** the user runs `rustman explore resume <id>`
- **THEN** only the remaining 500 probes SHALL be sent and the final report SHALL cover all 900

### Requirement: Exploration exit codes
`explore run` and `explore resume` SHALL exit 0 when no finding is at or above the `--fail-on` severity (default `high`), 1 when one is, 2 on invalid input or configuration, 4 when refused by the safety policy, and 130 when interrupted.

#### Scenario: Fail threshold
- **GIVEN** an exploration with one `medium` finding
- **WHEN** it is run with default `--fail-on high`
- **THEN** the exit code SHALL be 0
- **AND** with `--fail-on medium` the exit code SHALL be 1
