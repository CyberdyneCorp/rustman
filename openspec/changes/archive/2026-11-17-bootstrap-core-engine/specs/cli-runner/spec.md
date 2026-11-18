# Spec Delta

## Purpose

The headless face of Rustman. `rustman send` executes one request and `rustman run` executes collections, both for scripts and CI. Reporters, exit codes and terminal behavior are stable contracts that automation can rely on.

## ADDED Requirements

### Requirement: Send a single request
`rustman send <file | name>` SHALL execute one request (by path, or by its unique `name` within the workspace) and print the response. By default that means the status line, headers and body to stdout. `--body-only` SHALL print only the raw body, and `-o <file>` SHALL write the body to a file.

#### Scenario: Pipe JSON to jq
- **WHEN** the user runs `rustman send get-user --body-only | jq .id`
- **THEN** stdout SHALL contain only the response body bytes

#### Scenario: Ambiguous name
- **GIVEN** two requests named `List`
- **WHEN** the user runs `rustman send List`
- **THEN** the command SHALL exit 2 with `error: request name 'List' is ambiguous:` followed by both paths

### Requirement: Ad-hoc requests
`rustman send` SHALL also accept an ad-hoc request: `rustman send <METHOD> <url> [-H 'K: V']... [-d <body> | --json <body>]`, without needing a workspace. Environment and secret features SHALL be available only inside a workspace.

#### Scenario: No workspace needed
- **GIVEN** a directory without `rustman.toml`
- **WHEN** the user runs `rustman send GET https://httpbin.org/get`
- **THEN** the request SHALL be sent and printed with exit code 0

### Requirement: Run collections
`rustman run <path>...` SHALL execute every request file under the given files or folders, in collection order. It SHALL accept `--env <name>`, repeated `--var k=v`, `--bail` (stop after the first failed or errored request), `--filter <tag|glob>` and `--iterations <n>` (default 1).

#### Scenario: Run a folder
- **GIVEN** a folder with 5 requests
- **WHEN** the user runs `rustman run api/users --env staging`
- **THEN** all 5 SHALL execute in collection order against the `staging` environment

#### Scenario: Bail
- **GIVEN** the 2nd of 5 requests fails an assertion
- **WHEN** the run uses `--bail`
- **THEN** requests 3–5 SHALL be reported as `skipped (bail)`

#### Scenario: Tag filter
- **GIVEN** requests tagged `smoke` and `slow`
- **WHEN** the run uses `--filter tag:smoke`
- **THEN** only `smoke` requests SHALL run

### Requirement: Parallel execution
`--parallel <n>` (default 1) SHALL run up to `n` requests concurrently. Requests in the same collection folder SHALL still run sequentially, so captures chain, unless the folder's `_collection.yaml` sets `parallel: true`. Report order SHALL always match collection order.

#### Scenario: Independent folders in parallel
- **GIVEN** two folders A and B, each with 3 sequential requests
- **WHEN** the run uses `--parallel 2`
- **THEN** A and B SHALL progress concurrently while each folder's requests stay in order

### Requirement: Reporters
`--reporter` SHALL accept `pretty` (the default on a TTY), `json`, `junit` and `tap`, and SHALL be repeatable with `--reporter json:report.json`-style output paths. Without a path, the report SHALL go to stdout. The JSON report SHALL use the canonical schema `"schema": "rustman.report/v1"` with `"kind": "run"`, and SHALL include every exchange (redacted), assertion results and summary counts. `test-reports` later extends this schema additively.

#### Scenario: JUnit for CI
- **WHEN** the user runs `rustman run . --reporter junit:out/junit.xml --reporter pretty`
- **THEN** `out/junit.xml` SHALL contain one `<testcase>` per request, with a `<failure>` element for each failed request
- **AND** the pretty output SHALL still appear on the terminal

#### Scenario: JSON summary
- **WHEN** the JSON reporter writes to stdout
- **THEN** the document SHALL contain `summary.total`, `summary.passed`, `summary.failed`, `summary.errored`, `summary.skipped` and `summary.duration_ms`

### Requirement: Exit codes
The CLI SHALL use these exit codes: `0` (all executed requests passed), `1` (at least one assertion failed), `2` (usage or configuration error, such as a bad flag, no workspace, an invalid file or a missing environment), `3` (at least one request errored: network, TLS, timeout or unresolved variable), `130` (interrupted by SIGINT). When both failures and errors occur, `3` SHALL win.

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

### Requirement: Terminal-aware output
Color and progress animations SHALL be used only when stdout is a TTY. They SHALL be disabled when `NO_COLOR` is set (to any non-empty value), when `--color never` is passed, or when `TERM=dumb`. `--color always` SHALL force color. On a non-TTY, the default reporter SHALL be `pretty` without color or spinners.

#### Scenario: NO_COLOR
- **GIVEN** `NO_COLOR=1`
- **WHEN** the user runs `rustman run .` in a terminal
- **THEN** the output SHALL contain no ANSI escape sequences

#### Scenario: Piped output
- **WHEN** stdout is redirected to a file
- **THEN** the output SHALL contain no ANSI escapes or carriage-return progress lines

### Requirement: Diagnostics stream separation
Logs, warnings and progress SHALL go to stderr. Reports and response bodies SHALL go to stdout. `-v` / `-vv` SHALL raise log verbosity, and `RUST_LOG` SHALL be honored. Logs SHALL obey the redaction invariant.

#### Scenario: Clean stdout
- **WHEN** the user runs `rustman run . --reporter json -vv > r.json`
- **THEN** `r.json` SHALL be valid JSON, and debug logs SHALL appear only on stderr

### Requirement: Discoverable commands
`rustman --help` and `rustman <cmd> --help` SHALL document every flag. `rustman completions <bash|zsh|fish|powershell>` SHALL print a shell completion script. `rustman --version` SHALL print `rustman <semver>`.

#### Scenario: Completions
- **WHEN** the user runs `rustman completions zsh`
- **THEN** a valid zsh completion script SHALL be printed to stdout with exit code 0
