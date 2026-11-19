# Spec Delta

## MODIFIED Requirements

### Requirement: Exit codes
The CLI SHALL use these exit codes: `0` (all executed requests passed), `1` (at least one assertion failed), `2` (usage or configuration error, such as a bad flag, no workspace, an invalid file or a missing environment), `3` (at least one request errored: network, TLS, timeout or unresolved variable), `130` (interrupted by SIGINT, or on Unix by SIGTERM or SIGHUP, which are handled exactly like Ctrl+C). When both failures and errors occur, `3` SHALL win.

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
