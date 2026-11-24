## ADDED Requirements

### Requirement: Stress report content
A report with `kind: stress` SHALL contain the mode, profile, seed, target and SLOs, plus:
- the bench time series, with valid and fuzzed traffic separated;
- a step table (offered and achieved rate, p50/p95/p99, error classes, verdict);
- `breakpoint` (`max_sustainable_rps`, `breaking_rps`, `failure_mode`, `first_failing_endpoint`);
- `rate_limited_at`, `killed` and `recovery`;
- findings in the exploration finding format, with the evidence exchange as it happened.

#### Scenario: Breakpoint fields present
- **WHEN** a breakpoint stress run writes its JSON report
- **THEN** the document SHALL have `"kind": "stress"`, `breakpoint.max_sustainable_rps`, `breakpoint.breaking_rps`, `breakpoint.failure_mode` and `recovery.status`

#### Scenario: Older readers
- **GIVEN** a reader that only knows `run`, `exploration`, `bench` and `contract`
- **WHEN** it opens a `stress` report
- **THEN** schema validation SHALL still pass at the `rustman.report/v1` level, and the reader SHALL report an unsupported kind without crashing

#### Scenario: Evidence of a minimized finding
- **GIVEN** a `server-error-on-malformed-input` finding whose input was minimized
- **WHEN** the JSON report is written
- **THEN** its `evidence.status` SHALL be the response status, `evidence.body` the start of the response body, `evidence.request_body` the body that was sent, and `minimized.body` the minimized input

### Requirement: Stress findings in shared renderers
Stress findings SHALL render through the existing SARIF, Markdown, HTML and `report diff` paths, with the rule ids `server-error-on-malformed-input`, `timeout-on-malformed-input`, `accepted-malformed-input`, `oversized-input-accepted`, `ambiguous-framing-accepted`, `connection-dropped-without-response`, `no-slow-client-timeout`, `fuzz-degrades-valid-traffic`, `latency-drift`, `error-drift` and `no-recovery`.

#### Scenario: Diff two stress runs
- **GIVEN** two stress reports where the second adds `no-recovery`
- **WHEN** the user runs `rustman report diff old.json new.json`
- **THEN** `no-recovery` SHALL be listed as new, and the command SHALL exit with code 1
