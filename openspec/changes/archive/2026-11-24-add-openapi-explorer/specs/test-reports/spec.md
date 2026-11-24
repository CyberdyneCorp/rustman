# Spec Delta

## Purpose

Defines one canonical, versioned report for runs and explorations, and renders it for every audience: machines (JSON), CI (JUnit XML), code-scanning dashboards (SARIF), and humans (Markdown and self-contained HTML). No renderer ever leaks a secret.

## ADDED Requirements

### Requirement: Canonical JSON report schema
Runs and explorations SHALL produce a JSON report with a top-level `schema` field `rustman.report/v1` and a `kind` of `run` or `exploration`. It SHALL include tool version, start/end timestamps (RFC 3339), environment name, target, and summary counts. The JSON Schema SHALL be published in the repository and shipped via `rustman report schema`.

#### Scenario: Schema validation
- **GIVEN** any report produced by `rustman run --reporter json` or `rustman explore run`
- **WHEN** it is validated against the output of `rustman report schema`
- **THEN** validation SHALL pass

### Requirement: Schema versioning
Additive changes SHALL keep `rustman.report/v1`. Breaking changes SHALL bump the major version. `rustman report render` and `report diff` SHALL read any supported version and SHALL refuse unknown major versions with exit code 2.

#### Scenario: Unknown major version
- **GIVEN** a report with `schema: rustman.report/v9`
- **WHEN** the user runs `rustman report render it.json --format html`
- **THEN** the command SHALL exit with code 2 naming the unsupported version

### Requirement: Run report content
A `run` report SHALL list every executed request file with its path, exchange summary (method, URL, status, duration and timing breakdown), each assertion result (expression, expected, actual, pass/fail) and captures (names only for secrets).

#### Scenario: Failed assertion recorded
- **GIVEN** a request asserting `status == 200` that received 404
- **WHEN** the run report is produced
- **THEN** that assertion SHALL appear with expected `200`, actual `404` and result `fail`

### Requirement: Exploration report content
An `exploration` report SHALL contain the seed, safety settings, probe totals by outcome (`passed`, `finding`, `blocked`, `skipped`, `not run`, and `failed` for a probe whose request could not be sent or whose chain stopped early), the coverage matrix, the findings with evidence and reproduction, and the list of resources left behind.

#### Scenario: Blocked probes counted
- **GIVEN** a read-only exploration in which 40 probes were blocked
- **WHEN** the report is produced
- **THEN** the probe totals SHALL show `blocked: 40`

### Requirement: Output formats
`rustman report render <report.json> --format <json|junit|markdown|html|sarif> [-o <file>]` SHALL render any canonical report. `run` and `explore run` SHALL accept `--report <format>[=<path>]`, repeatable, to write several formats in one invocation. Output defaults to stdout when no path is given.

#### Scenario: Multiple formats at once
- **WHEN** the user runs `rustman explore run ... --report html=out/r.html --report sarif=out/r.sarif`
- **THEN** both files SHALL be written from the same report data

### Requirement: JUnit XML rendering
JUnit output SHALL map each request file (runs) or each operation (explorations) to a `testcase`, grouped into a `testsuite` per collection or tag. Failed assertions and findings at or above the `--fail-on` severity SHALL become `failure` elements, and blocked or skipped probes SHALL become `skipped`.

#### Scenario: Finding as failure
- **GIVEN** an exploration with a `high` finding on `POST /users`
- **WHEN** rendered as JUnit with default `--fail-on high`
- **THEN** the `POST /users` testcase SHALL contain a `failure` element with the finding's rule id

### Requirement: SARIF rendering
SARIF output SHALL be SARIF 2.1.0. Each finding rule SHALL be a `reportingDescriptor`. Each finding SHALL be a `result` whose level maps `critical`/`high` → `error`, `medium` → `warning` and `low`/`info` → `note`. Locations SHALL point to the OpenAPI document and the JSON pointer of the operation.

#### Scenario: SARIF location
- **GIVEN** a finding on `GET /users/{id}` from `api.yaml`
- **WHEN** rendered as SARIF
- **THEN** the result location SHALL reference `api.yaml` with the pointer `/paths/~1users~1{id}/get`

### Requirement: Human-readable renderings
Markdown output SHALL contain a summary, a findings table sorted by severity, and the coverage summary. HTML output SHALL be a single self-contained file with no external network references, offering filtering by severity and operation and expandable evidence. It SHALL render correctly with JavaScript disabled, except for the filtering.

#### Scenario: HTML is offline
- **WHEN** an HTML report is rendered
- **THEN** the file SHALL contain no `http://` or `https://` references to scripts, styles, fonts or images

### Requirement: Report diff
`rustman report diff <old.json> <new.json>` SHALL list new, fixed and unchanged findings (matched by rule id and operation), coverage deltas and assertion results that changed state. It SHALL exit 1 when there are new findings at or above `--fail-on`, and 0 otherwise. `--format json|markdown` selects the output format.

#### Scenario: Regression detected
- **GIVEN** the new report contains a `critical` finding absent from the old one
- **WHEN** the user runs `rustman report diff old.json new.json`
- **THEN** the finding SHALL be listed as `new` and the exit code SHALL be 1

#### Scenario: Fixed finding
- **GIVEN** a finding present in old and absent in new
- **WHEN** diffed
- **THEN** it SHALL be listed as `fixed`

### Requirement: Redaction in every report
No report format SHALL contain the value of a secret, or of any header named `Authorization`, `Proxy-Authorization`, `Cookie`, `Set-Cookie` or `X-Api-Key`, or of an auth scheme's credential. Such values SHALL appear as `«redacted:<name>»`. Redaction SHALL happen when the report is built, so re-rendering can never recover the value.

#### Scenario: Bearer token not leaked
- **GIVEN** an exploration authenticated with secret `bearerAuth_token = s3cr3t`
- **WHEN** the report is rendered in every format
- **THEN** none of the outputs SHALL contain `s3cr3t`
- **AND** the Authorization header SHALL appear as `«redacted:bearerAuth_token»`
