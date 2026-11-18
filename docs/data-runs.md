# Data-driven runs

`rustman run --data <file>` runs the selected requests once per row of a CSV or JSON file, like `newman run -d`. Each row's columns are variables during its iteration, so one folder of requests can cover many users, tenants or test cases.

```sh
rustman run api/users --env ci --data users.csv
rustman run api/users --env ci --data users.csv --data-label email --reporter junit:out/junit.xml
```

With `users.csv`:

```csv
email,role
ana@example.com,admin
bo@example.com,user
cy@example.com,user
```

and a folder of 2 requests using `{{email}}`, the run sends 6 requests: both requests for row 1, then both for row 2, then both for row 3. Every request in iteration 2 resolves `{{email}}` to `bo@example.com`.

## Data files

The file's extension picks the format: `.csv` or `.json` (in any case). Both are read by the shared data-row reader (`rustman_core::data_rows`), which load testing's `--data` uses too.

- **CSV** follows RFC 4180. The first row is the header and names the columns. A UTF-8 BOM is stripped, fields may be quoted (`"Doe, Jane"`, with `""` for a quote), line endings may be `\n` or `\r\n`, and every value is a string (`007` stays `007`). Every row must have as many fields as the header. An empty cell defines its variable as empty text, so it overrides a value from the environment or the collection; leave the column out of the file to fall back to those.
- **JSON** must be an array of objects. Scalars become strings (`1` becomes `1`, `true` becomes `true`, `null` becomes empty text); numbers keep their text exactly as written (`1.10` stays `1.10`, `1e2` stays `1e2`, big integers keep every digit). Nested values become compact JSON text, their numbers kept as written too: `{"id": 1, "tags": ["a"]}` gives `{{id}}` = `1` and `{{tags}}` = `["a"]`. A key missing from a row is not defined in that iteration.

Rows are numbered from 1 without the header, as in iteration labels. A bad file is a usage error (exit code 2), reported before any request is sent:

| Problem | Message |
|---------|---------|
| No data rows (or an empty file) | `--data users.csv: no rows` |
| A quoted field that is never closed | `--data users.csv: invalid CSV: unterminated quoted field in row 1` |
| Text after the closing quote of a quoted field (`"a"b`) | `--data users.csv: invalid CSV: text after the closing quote of a quoted field in row 2` |
| A row with the wrong number of fields | `--data users.csv: row 4 has 3 fields, expected 2` |
| JSON that is not an array of objects | `--data cases.json: expected an array of objects` |
| A repeated or empty header name | `--data users.csv: duplicate column 'email'` |
| Another extension | `--data users.txt: unsupported data file (use .csv or .json)` |

## A collection's default data file

A `_collection.yaml` may name the data file its folder runs with, relative to the `_collection.yaml` and inside the workspace:

```yaml
# api/tenants/_collection.yaml
data: tenants.csv
secret_columns: [admin_pin]
```

`rustman run api/tenants` (or a request inside it) then runs once per row of `api/tenants/tenants.csv`. The nearest folder at or above each run path that sets `data` applies; two run paths naming different files is a usage error. `--data <file>` replaces the default and `--no-data` runs the folder once without row variables. A `data` path that leaves the workspace fails with `api/tenants/_collection.yaml: data: file path escapes workspace: ../x.csv`.

## Iterations and variables

The row sits in the [variable precedence](environments.md#precedence) directly below in-run captures: `--var` overrides > captures of the iteration > **data row** > request `variables` > collection `variables` > persisted captures > environment > `rustman.toml`.

- A row value beats the request's and the collection's `variables`, which act as defaults the data file varies, and the environment, so a row can change `{{base_url}}` per tenant.
- A capture made during the iteration beats the row, so a login request can replace a row's placeholder `token`.
- Iterations are independent: captures made without `persist: true` are cleared at the start of each iteration, so a request using `{{order_id}}` before it is captured again fails with `unresolved variable(s): order_id` in every iteration. Persisted captures stay available.

With `--data`, the number of iterations is the number of rows. `--data` with `--iterations` is a usage error: `--data and --iterations cannot be combined (iterations = row count)`. With a collection's default data file, pass `--no-data` to use `--iterations` instead.

## Bail

`--bail` stops the whole run after the first failed or errored request in any iteration: every request not started yet, in this iteration and the next ones, is reported as `skipped (bail)`. `--bail=iteration` skips only the rest of the failing iteration and goes on with the next row:

```text
iteration 2/3 · row 2
FAIL  rows/1-check  GET 500  2 ms
      status: expected 200, got 500
SKIP  rows/2-after  skipped (bail)

iteration 3/3 · row 3
PASS  rows/1-check  GET 200  1 ms
PASS  rows/2-after  GET 200  2 ms
```

## Iteration labels

Each iteration has a label: the value of the `--data-label <column>` column, else `row <index>` (also when the row's value is empty). The column must exist and must not be secret (a secret label column is a usage error, exit code 2).

- `pretty` groups the requests under one `iteration <index>/<total> · <label>` line per iteration.
- `junit` test case names end with ` [<label>]`: `get-profile [ana@example.com]`.
- `tap` descriptions end with ` [<label>]`.
- `json` keeps the integer `iteration` every request has (1-based; with `--data`, the row number) and adds a `data_row` object:

```json
"iteration": 2,
"data_row": { "index": 2, "total": 3, "label": "bo@example.com" }
```

A JSON report from a run without `--data` has no `data_row`, `--iterations` above 1 included, and is unchanged from v0.1. Under `--iterations`, pretty, JUnit and TAP still label each iteration by its index (`[2]` in JUnit and TAP names).

Compatibility: `data_row` is an optional field added to `rustman.report/v1` in v0.2; `iteration` keeps its v0.1 type and meaning, so v1 consumers keep working.

## Secret columns

These columns are secret:

- columns named in `--secret-columns <a,b>`;
- columns listed in `secret_columns` of a `_collection.yaml` above any request of the run;
- columns whose name contains `password`, `token`, `secret` or `api_key` (ignoring case, with `-` read as `_`): `password`, `access_token`, `API-KEY`.

Their values are redacted as `«redacted:<column>»` in every output, the same way as environment secrets (see [redaction](environments.md)): reports, history, logs, error messages and the pretty output. A request body `password={{password}}` is recorded as `password=«redacted:password»`, and a server echoing the value back is redacted too. Values are known to the redactor before the first request is sent.
