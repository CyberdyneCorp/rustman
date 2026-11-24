# cli-runner Specification

## Purpose
The headless face of Rustman. `rustman send` executes one request and `rustman run` executes collections, both for scripts and CI. Reporters, exit codes and terminal behavior are stable contracts that automation can rely on.

## Requirements

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

### Requirement: Data-driven runs
`rustman run --data <file.csv|file.json>` SHALL run the selected requests once per data row, in row order. Each row's columns or keys SHALL be available as variables during that iteration. With `--data`, the number of iterations SHALL equal the number of rows.

#### Scenario: One iteration per CSV row
- **GIVEN** `users.csv` with header `email,role` and 3 rows, and a folder of 2 requests using `{{email}}`
- **WHEN** the user runs `rustman run api/users --data users.csv`
- **THEN** 6 exchanges SHALL run: both requests for row 1, then row 2, then row 3
- **AND** each request in iteration 2 SHALL resolve `{{email}}` to row 2's value

#### Scenario: JSON data file
- **GIVEN** `cases.json` containing `[{"id": 1, "tags": ["a"]}, {"id": 2, "tags": []}]`
- **WHEN** a run uses `--data cases.json`
- **THEN** iteration 1 SHALL resolve `{{id}}` to `1` and `{{tags}}` to `["a"]`

### Requirement: Data file parsing
Data files SHALL be read by the shared `data_rows` reader, which `load-testing` also uses. CSV SHALL follow RFC 4180 with a mandatory header row, a stripped UTF-8 BOM and string values. JSON SHALL be an array of objects; scalars SHALL become strings (numbers as written) and nested values compact JSON text. Any other shape SHALL be a usage error.

#### Scenario: Empty data file
- **GIVEN** a CSV with a header row and no data rows
- **WHEN** a run uses it
- **THEN** the run SHALL exit with code 2 and print `--data users.csv: no rows`

#### Scenario: Ragged CSV row
- **GIVEN** a CSV whose header has 2 columns and whose row 4 has 3 fields
- **WHEN** a run uses it
- **THEN** the run SHALL exit with code 2 and print `--data users.csv: row 4 has 3 fields, expected 2`

#### Scenario: JSON numbers keep their text
- **GIVEN** a JSON data file containing `[{"price": 1.10, "e": 1e2, "big": 12345678901234567890123}]`
- **WHEN** a run uses it
- **THEN** `{{price}}` SHALL resolve to `1.10`, `{{e}}` to `1e2` and `{{big}}` to `12345678901234567890123`

#### Scenario: Text after a closing quote
- **GIVEN** a CSV whose row 2 has the field `"a"b`
- **WHEN** a run uses it
- **THEN** the run SHALL exit with code 2 and print `--data users.csv: invalid CSV: text after the closing quote of a quoted field in row 2`

#### Scenario: JSON object instead of array
- **GIVEN** a JSON data file containing `{"id": 1}`
- **WHEN** a run uses it
- **THEN** the run SHALL exit with code 2 and print `--data cases.json: expected an array of objects`

### Requirement: Data row variable precedence
The data row SHALL slot into the `environments` precedence chain directly below in-run captures: overrides > in-run captures > data row > request `variables` > collection `variables` > persisted captures > environment > `rustman.toml`. In-run captures SHALL be cleared at the start of each iteration; persisted values stay in their layer.

#### Scenario: Row overrides environment
- **GIVEN** the environment sets `base_url=https://a.example` and the row sets `base_url=https://b.example`
- **WHEN** the iteration runs
- **THEN** requests SHALL be sent to `https://b.example`

#### Scenario: Row overrides collection variables
- **GIVEN** `_collection.yaml` sets `variables: {tenant: default}` and the row sets `tenant=acme`
- **WHEN** the iteration runs
- **THEN** `{{tenant}}` SHALL resolve to `acme`

#### Scenario: Capture overrides row
- **GIVEN** the row sets `token=placeholder` and the first request captures `token` from the login response
- **WHEN** the second request in the same iteration uses `{{token}}`
- **THEN** it SHALL use the captured value

#### Scenario: Iterations are independent
- **GIVEN** iteration 1 captured `order_id` without `persist`
- **WHEN** iteration 2 starts
- **THEN** `{{order_id}}` SHALL be unresolved until iteration 2 captures it again

### Requirement: Data and iterations are exclusive
Passing both `--data` and `--iterations` SHALL be a usage error with exit code 2 and the message `--data and --iterations cannot be combined (iterations = row count)`.

#### Scenario: Both flags
- **WHEN** the user runs `rustman run . --data users.csv --iterations 5`
- **THEN** the run SHALL exit with code 2 before sending any request

### Requirement: Collection default data file
A `_collection.yaml` MAY set `data: <path>`, a path resolved relative to that file that SHALL NOT escape the workspace. Running that folder without `--data` SHALL use it. `--data` SHALL override it, and `--no-data` SHALL disable it.

#### Scenario: Default data used
- **GIVEN** `api/tenants/_collection.yaml` with `data: tenants.csv` holding 4 rows
- **WHEN** the user runs `rustman run api/tenants`
- **THEN** the folder SHALL run 4 iterations

#### Scenario: Data disabled
- **WHEN** the user runs `rustman run api/tenants --no-data`
- **THEN** the folder SHALL run once without row variables

### Requirement: Data row redaction
Columns named in `--secret-columns <a,b>` or `_collection.yaml` `secret_columns`, and columns named like `password`, `token`, `secret` or `api_key`, SHALL be secret. Their values SHALL be redacted as `«redacted:<column>»` in every output, per the `environments` invariant.

#### Scenario: Password column in a report
- **GIVEN** `users.csv` with a `password` column and a request whose body contains `{{password}}`
- **WHEN** the run writes a JSON report
- **THEN** the recorded request body SHALL contain `«redacted:password»` and not the value

### Requirement: Iteration labels in reports
In a data-driven run, each exchange SHALL carry `data_row: {index, total, label}`: `index` is 1-based and `label` is the `--data-label <column>` value, or `row <index>` when it is empty or absent. The v0.1 integer `iteration` SHALL keep its type and meaning. Pretty output SHALL group by iteration, and JUnit test case names SHALL end with ` [<label>]`. A secret label column SHALL be rejected with exit 2.

#### Scenario: JUnit names per row
- **GIVEN** a run with `--data users.csv --data-label email` and request `get-profile`
- **WHEN** the JUnit reporter writes its file
- **THEN** it SHALL contain test cases named `get-profile [ana@example.com]` and `get-profile [bo@example.com]`

#### Scenario: Data row in JSON report
- **WHEN** a data-driven run writes `rustman.report/v1`
- **THEN** each exchange SHALL include `data_row.index`, `data_row.total` and `data_row.label`
- **AND** each exchange SHALL keep the integer `iteration`, equal to `data_row.index`
- **AND** a report from a run without `--data` SHALL NOT contain `data_row` and SHALL be identical to the v0.1 report

### Requirement: Bail across iterations
With `--data`, `--bail` SHALL stop the whole run after the first failed or errored request in any iteration, and SHALL report the remaining exchanges as `skipped (bail)`. `--bail=iteration` SHALL skip only the rest of the failing iteration, and SHALL continue with the next row.

#### Scenario: Bail per iteration
- **GIVEN** 3 rows, and the first of 2 requests fails for row 2
- **WHEN** the run uses `--bail=iteration`
- **THEN** row 2's second request SHALL be skipped and row 3 SHALL run both requests
