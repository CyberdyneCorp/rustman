# visual-dashboards Specification

## Purpose
An optional graphical companion (`rustman-viz`) that renders Rustman runs, explorations and large responses as charts, tables and graphs on demand, without ever being required by, or able to change the behaviour of, the core `rustman` tool.

## Requirements

### Requirement: Companion is optional
The `rustman` binary SHALL build, install and run with full functionality except graphical views when `rustman-viz` is absent. Building `rustman` SHALL NOT require the Raven library, a C toolchain or a GPU. `rustman-viz` SHALL be built only when the cargo feature `raven` is enabled.

#### Scenario: Default build
- **GIVEN** a machine with no Raven library and no Vulkan loader
- **WHEN** the user runs `cargo install rustman` and then `rustman run smoke/`
- **THEN** the install and the run SHALL succeed

#### Scenario: Feature-gated companion
- **WHEN** the workspace is built without the `raven` feature
- **THEN** no `rustman-viz` binary SHALL be produced

### Requirement: Companion discovery
`rustman` SHALL locate the companion by checking, in order: the `RUSTMAN_VIZ` environment variable, `rustman-viz` on `PATH`, then `rustman-viz` (`rustman-viz.exe` on Windows) in the directory of the running `rustman` executable. If `RUSTMAN_VIZ` is set but does not point to an executable file, discovery SHALL fail with that path in the error and SHALL NOT fall back.

#### Scenario: Environment override wins
- **GIVEN** `RUSTMAN_VIZ=/opt/viz/rustman-viz` and another `rustman-viz` on `PATH`
- **WHEN** a view is requested
- **THEN** `/opt/viz/rustman-viz` SHALL be used

#### Scenario: Sibling executable
- **GIVEN** `RUSTMAN_VIZ` unset, nothing on `PATH`, and `rustman-viz` next to `rustman`
- **WHEN** a view is requested
- **THEN** the sibling `rustman-viz` SHALL be used

#### Scenario: Invalid override
- **GIVEN** `RUSTMAN_VIZ=/nope/rustman-viz`, which does not exist
- **WHEN** a view is requested
- **THEN** the error SHALL name `/nope/rustman-viz` and no other location SHALL be tried

### Requirement: Protocol handshake
The producer SHALL start each session by sending a `hello` message carrying protocol `rustman-viz/<major>`. The companion SHALL reply with `hello_ack` listing its supported views, or with `hello_reject` listing its supported protocol versions. On rejection the producer SHALL report both versions and SHALL NOT send further messages.

#### Scenario: Compatible versions
- **GIVEN** producer and companion both speak `rustman-viz/1`
- **WHEN** the session starts
- **THEN** the companion SHALL reply `hello_ack` with protocol `rustman-viz/1` and its view list

#### Scenario: Incompatible versions
- **GIVEN** the producer speaks `rustman-viz/2` and the companion only `rustman-viz/1`
- **WHEN** the session starts
- **THEN** the companion SHALL reply `hello_reject` with `["rustman-viz/1"]`
- **AND** the producer SHALL show `Viz companion speaks rustman-viz/1, rustman needs rustman-viz/2` and an upgrade hint

### Requirement: Protocol transport
Messages SHALL be UTF-8 JSON objects, one per line, written to the companion's stdin, each with a `type` field. Lines SHALL NOT exceed 16 MiB; larger payloads SHALL be passed as a `blob_ref` to a temp file. The companion SHALL ignore unknown message types and unknown fields. End of stdin SHALL tell the companion the producer has gone, while leaving open windows usable.

#### Scenario: Unknown field ignored
- **GIVEN** a `run_finished` message with an extra field `"future": true`
- **WHEN** the companion receives it
- **THEN** it SHALL process the message normally

#### Scenario: Oversized payload via blob reference
- **GIVEN** a 200 MiB response to inspect
- **WHEN** the producer sends it
- **THEN** it SHALL write the body to a temp file and send `inspect_response` with a `blob_ref` path instead of inline data

#### Scenario: Producer exits
- **GIVEN** an open run dashboard
- **WHEN** `rustman` exits and the companion's stdin closes
- **THEN** the dashboard SHALL stay open, showing the last received state and the label `Producer disconnected`

### Requirement: Blob file ownership
The producer SHALL own each `blob_ref` temp file: it SHALL keep the file until the companion process has exited and then delete it. The companion SHALL NOT delete it.

#### Scenario: Blob file removed after the companion exits
- **GIVEN** a response body sent to the companion as a `blob_ref`
- **WHEN** the companion process exits while the producer is still running
- **THEN** the producer SHALL delete the temp file

### Requirement: Read-only companion
The companion SHALL NOT send network requests to API targets, execute request files, or modify workspace, environment, secret or history files. Its only outputs SHALL be its windows, protocol acknowledgements on stdout, and a log file.

#### Scenario: No outbound traffic
- **GIVEN** a run dashboard showing requests to `https://api.example.com`
- **WHEN** the user interacts with every view control
- **THEN** the companion SHALL make no connection to `api.example.com`

### Requirement: Secrets stay redacted in the companion
Data sent to the companion SHALL already be redacted according to the `environments` secret rules. The companion SHALL display redacted markers (`«redacted:name»`) as received and SHALL offer no reveal function.

#### Scenario: Redacted header in inspector
- **GIVEN** an exchange whose request header `Authorization` contains the secret `token`
- **WHEN** it is opened in the response inspector
- **THEN** the header SHALL show `Bearer «redacted:token»`

### Requirement: Run dashboard
The run dashboard SHALL show KPI tiles for total requests, assertion pass rate, p50 latency, p95 latency and error count, each with a sparkline. It SHALL also show a donut of outcomes by class (2xx, 3xx, 4xx, 5xx, transport error, cancelled), a latency-over-time plot, a latency histogram, and a table with one row per request file (count, pass rate, p50, p95, sparkline).

#### Scenario: Completed run
- **GIVEN** a run report with 200 exchanges: 180 ×2xx, 15 ×4xx, 5 transport errors
- **WHEN** it is opened in the run dashboard
- **THEN** the donut SHALL show 180 / 15 / 5 in their classes and the requests tile SHALL read `200`

### Requirement: Exploration dashboard
The exploration dashboard SHALL show a heatmap with one row per API operation and one column per status class, where each cell counts probes; gauges for operation coverage and documented-status-code coverage; and a findings table sorted by severity (critical, high, medium, low, info), then by operation, with a cell click filtering the table to that operation.

#### Scenario: Drill into an operation
- **GIVEN** an exploration with findings on `POST /users` and `GET /orders`
- **WHEN** the user clicks the heatmap row for `POST /users`
- **THEN** the findings table SHALL list only findings for `POST /users`

#### Scenario: Coverage gauges
- **GIVEN** an exploration that exercised 18 of 24 operations
- **THEN** the operation coverage gauge SHALL read `75%`

### Requirement: API graph
The API graph view SHALL render one node per API operation from an exploration report, and one link per data dependency (a value captured from one operation's response used as input to another), read from the report's optional `dependencies` field. Nodes SHALL be coloured by the worst finding severity on that operation, with no findings shown in the neutral colour. Selecting a node SHALL list its findings.

#### Scenario: Dependency link
- **GIVEN** an exploration in which `POST /users` produced `id` that was used as `{id}` in `GET /users/{id}`
- **WHEN** the API graph opens
- **THEN** a link SHALL connect `POST /users` to `GET /users/{id}`

### Requirement: Large-response inspector
The response inspector SHALL show the top-level JSON array, or the array selected by a JSONPath sent with the request, as a sortable table with virtual rows and columns taken from the union of keys in the first 1,000 elements. It SHALL also show a read-only JSON text view with syntax highlighting. Non-array bodies SHALL show the text view only.

#### Scenario: Array of objects
- **GIVEN** a response `[{"id":1,"name":"a"},{"id":2,"email":"b@x"}]`
- **WHEN** it is opened in the inspector
- **THEN** the table SHALL have the columns `id`, `name`, `email`, with empty cells where a key is missing

#### Scenario: Object body
- **GIVEN** a response `{"ok": true}`
- **WHEN** it is opened in the inspector
- **THEN** only the read-only JSON text view SHALL be shown

### Requirement: Live updates
While a run or exploration is in progress, the dashboard SHALL update as progress events arrive, at no more than 10 redraws per second. Progress updates MAY be coalesced, but `finding`, `run_finished` and `exploration_finished` messages SHALL never be dropped. A final state SHALL be marked `Completed` or `Aborted`.

#### Scenario: Streaming run
- **GIVEN** `rustman run api/ --viz` running 1,000 requests
- **WHEN** requests complete
- **THEN** the run dashboard SHALL update its KPI tiles while the run is in progress, and show `Completed` when it ends

#### Scenario: Burst of findings
- **GIVEN** an exploration emits 500 findings within one second
- **THEN** all 500 findings SHALL appear in the findings table

### Requirement: Opening saved reports from the CLI
`rustman viz <REPORT>` SHALL open a run or exploration report file produced by `test-reports` in the matching dashboard, detected from the report's type field. `--view graph` SHALL open the API graph for an exploration report. An unrecognised or corrupt file SHALL fail with exit code 2 and a message naming the file.

#### Scenario: Open a run report
- **GIVEN** `reports/run-2026-11-17.json` produced by `rustman run`
- **WHEN** the user runs `rustman viz reports/run-2026-11-17.json`
- **THEN** the companion SHALL open the run dashboard with that report's data

#### Scenario: Corrupt report
- **WHEN** the user runs `rustman viz notes.txt`
- **THEN** the command SHALL exit with code 2 and say `notes.txt` is not a Rustman report

### Requirement: Streaming flag for runs and explorations
`rustman run` and `rustman explore` SHALL accept `--viz`, which opens the matching dashboard and streams progress to it. If the companion is unavailable, the command SHALL print a warning to stderr and continue without visuals; the run's exit code SHALL be unaffected.

#### Scenario: --viz without companion
- **GIVEN** no companion can be discovered
- **WHEN** the user runs `rustman run smoke/ --viz`
- **THEN** stderr SHALL include `Viz companion not found; continuing without visuals`
- **AND** the exit code SHALL reflect only the run's results

### Requirement: Graceful fallback
When the companion is not found, rejects the protocol, or cannot initialise graphics, `rustman` SHALL explain the specific reason and offer to write the HTML report from `test-reports` instead. When graphics cannot start (no GPU backend: no Vulkan driver on Linux or Windows, or no Metal device), the companion SHALL exit with code 3 after writing an `error` line with code `vulkan_unavailable`.

#### Scenario: No Vulkan
- **GIVEN** a machine without a Vulkan loader
- **WHEN** the companion starts
- **THEN** it SHALL write `{"type":"error","code":"vulkan_unavailable",...}` and exit with code 3
- **AND** `rustman` SHALL show `Graphics unavailable (Vulkan)` and offer the HTML report

### Requirement: Fault isolation
A crash, hang or exit of the companion SHALL NOT interrupt, slow down or change the result of the producing `rustman` process. Writes to the companion SHALL be non-blocking for the producer: if the companion stops reading, the producer SHALL drop progress messages and, after 5 seconds of back-pressure, detach from it with a warning.

#### Scenario: Companion crashes during a run
- **GIVEN** `rustman run api/ --viz` in progress
- **WHEN** the companion process is killed
- **THEN** the run SHALL continue to completion with an unchanged exit code, and a warning `Viz companion exited` SHALL be shown

#### Scenario: Companion stops reading
- **GIVEN** the companion is frozen and not reading stdin
- **WHEN** 5 seconds of back-pressure accumulate
- **THEN** the producer SHALL detach from the companion and continue the run
