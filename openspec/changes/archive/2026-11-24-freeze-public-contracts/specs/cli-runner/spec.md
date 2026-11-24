## MODIFIED Requirements

### Requirement: Exit codes
The CLI SHALL use these exit codes: `0` (every executed request passed), `1` (an assertion failed), `2` (usage or configuration error, such as a bad flag, no workspace, an invalid file or a missing environment), `3` (a request errored: network, TLS, timeout or unresolved variable), `4` (refused by the safety policy, the stress gate or the monitor policy) and `130` (interrupted by SIGINT, or on Unix by SIGTERM or SIGHUP, handled like Ctrl+C). When both failures and errors occur, `3` SHALL win.

#### Scenario: Assertion failure
- **GIVEN** a run where one assertion fails and nothing errors
- **WHEN** it finishes
- **THEN** the exit code SHALL be 1

#### Scenario: Network error dominates
- **GIVEN** one assertion failure and one connection refused
- **WHEN** the run finishes
- **THEN** the exit code SHALL be 3

#### Scenario: Ctrl+C
- **WHEN** the user presses Ctrl+C during a run
- **THEN** in-flight requests SHALL be cancelled, reporters SHALL flush partial results, and the exit code SHALL be 130

#### Scenario: SIGTERM
- **WHEN** the process receives SIGTERM (or SIGHUP) during a run on Unix
- **THEN** it SHALL behave as for Ctrl+C: in-flight requests SHALL be cancelled, stdio MCP servers SHALL be stopped with the processes they started, reporters SHALL flush partial results, and the exit code SHALL be 130

#### Scenario: Safety refusal
- **GIVEN** an exploration whose target is a production host without the production opt-in
- **WHEN** it is started
- **THEN** no request SHALL be sent and the exit code SHALL be 4

## ADDED Requirements

### Requirement: Public contract stability
Within a major version, the public contracts SHALL change only additively: request-file `version: 1`, `rustman.toml` `version = 1`, the `rustman.report/v1` report, the `rustman-viz/1` protocol, the plugin WIT API `rustman:plugin@1` and the exit codes. Nothing that exists SHALL be renamed, removed, retyped or given a new meaning, and a deprecated item SHALL keep working until the next major version. A test in CI SHALL fail when a frozen contract changes non-additively.

#### Scenario: Unsupported request-file version
- **GIVEN** a request file of any built-in kind with `version: 2`
- **WHEN** it is loaded
- **THEN** it SHALL be refused with `unsupported request file version 2 (expected 1)` and exit code 2

#### Scenario: A report field is removed
- **WHEN** a change removes or renames a property of a `rustman.report/v1` report kind
- **THEN** the frozen-surface test of the report schema SHALL fail
