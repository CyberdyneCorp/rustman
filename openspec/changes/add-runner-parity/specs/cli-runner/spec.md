## ADDED Requirements

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
Data files SHALL be read by the shared `data_rows` reader, which `load-testing` also uses. CSV SHALL follow RFC 4180 with a mandatory header row, a stripped UTF-8 BOM and string values. JSON SHALL be an array of objects; scalars SHALL become strings and nested values compact JSON text. Any other shape SHALL be a usage error.

#### Scenario: Empty data file
- **GIVEN** a CSV with a header row and no data rows
- **WHEN** a run uses it
- **THEN** the run SHALL exit with code 2 and print `--data users.csv: no rows`

#### Scenario: Ragged CSV row
- **GIVEN** a CSV whose header has 2 columns and whose row 4 has 3 fields
- **WHEN** a run uses it
- **THEN** the run SHALL exit with code 2 and print `--data users.csv: row 4 has 3 fields, expected 2`

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
Each exchange SHALL carry `iteration: {index, total, label}`, where `index` is 1-based and `label` is the value of `--data-label <column>` or else `row <index>`. Pretty output SHALL group by iteration, and JUnit test case names SHALL end with ` [<label>]`. A secret label column SHALL be rejected with exit 2.

#### Scenario: JUnit names per row
- **GIVEN** a run with `--data users.csv --data-label email` and request `get-profile`
- **WHEN** the JUnit reporter writes its file
- **THEN** it SHALL contain test cases named `get-profile [ana@example.com]` and `get-profile [bo@example.com]`

#### Scenario: Iteration in JSON report
- **WHEN** a data-driven run writes `rustman.report/v1`
- **THEN** each exchange SHALL include `iteration.index`, `iteration.total` and `iteration.label`
- **AND** a report from a run without `--data` SHALL NOT contain `iteration`

### Requirement: Bail across iterations
With `--data`, `--bail` SHALL stop the whole run after the first failed or errored request in any iteration, and SHALL report the remaining exchanges as `skipped (bail)`. `--bail=iteration` SHALL skip only the rest of the failing iteration, and SHALL continue with the next row.

#### Scenario: Bail per iteration
- **GIVEN** 3 rows, and the first of 2 requests fails for row 2
- **WHEN** the run uses `--bail=iteration`
- **THEN** row 2's second request SHALL be skipped and row 3 SHALL run both requests
