# snapshot-testing Specification

## Purpose
Lets users catch unintended response changes by recording a normalized response once (a Snapshot) and failing later runs when it differs, with readable diffs, noise-removing normalization and an explicit approve flow.

## Requirements

### Requirement: Snapshot assertion
The `assertions` list SHALL accept `snapshot` (shorthand) or `snapshot: {name, compare, ignore, headers, replace, sort, presets}`. A request file MAY contain several snapshot assertions with distinct `name`s (default name `default`). It SHALL apply to `http`, `graphql`, `jsonrpc` and `mcp` exchanges.

#### Scenario: Shorthand
- **GIVEN** a request file with `assertions: [snapshot]`
- **WHEN** it runs and a stored snapshot matches
- **THEN** the assertion SHALL pass

#### Scenario: Duplicate names
- **GIVEN** two snapshot assertions without explicit names in one file
- **THEN** loading SHALL fail with `<path>:<line>: duplicate snapshot name 'default'`

### Requirement: Snapshot location and naming
Snapshots SHALL be stored in `__snapshots__/<request-file-stem>.<name>.snap.yaml` next to the request file. For `--iterations` greater than 1, all iterations SHALL compare against the same snapshot. The directory SHALL NOT be added to `.gitignore` by `rustman init`.

#### Scenario: Path of a snapshot
- **GIVEN** `users/get-user.rman.yaml` with a `default` snapshot
- **THEN** its file SHALL be `users/__snapshots__/get-user.default.snap.yaml`

### Requirement: Recording on first run
Outside CI mode, when a snapshot file doesn't exist, the system SHALL write the normalized snapshot and report the assertion as `recorded` (counted as passed), and the run summary SHALL list the number of recorded snapshots.

#### Scenario: New snapshot
- **GIVEN** no snapshot file exists and CI mode is off
- **WHEN** `rustman run users/` executes
- **THEN** the file SHALL be written, the assertion SHALL be `recorded`, and the summary SHALL include `1 snapshot recorded`

### Requirement: Update mode
`rustman run --update-snapshots` (or `-u`) SHALL overwrite differing snapshots with the new normalized response and report them as `updated`. Matching snapshots SHALL NOT be rewritten, so unchanged files keep their bytes and timestamps.

#### Scenario: Intended change accepted
- **GIVEN** a stored snapshot that differs from the response
- **WHEN** the user runs with `--update-snapshots`
- **THEN** the snapshot SHALL be rewritten and reported `updated`, and the exit code SHALL be 0 if nothing else failed

#### Scenario: Unchanged file untouched
- **GIVEN** a matching snapshot
- **WHEN** the user runs with `--update-snapshots`
- **THEN** the file SHALL NOT be rewritten

### Requirement: CI mode refuses writes
When `--ci` is passed or the environment variable `CI` is `true` or `1`, the system SHALL never write snapshots. A missing snapshot SHALL fail with `snapshot missing: <path> (run without --ci to record)`. Combining `--ci` with `--update-snapshots` SHALL be a usage error with exit code 2.

#### Scenario: Missing snapshot in CI
- **GIVEN** `CI=true` and no snapshot file
- **WHEN** the run executes
- **THEN** the assertion SHALL fail, no file SHALL be written, and the exit code SHALL be 1

#### Scenario: Conflicting flags
- **WHEN** the user runs `rustman run . --ci --update-snapshots`
- **THEN** the command SHALL exit 2 with `--update-snapshots cannot be used in CI mode`

### Requirement: Compared parts
`compare` SHALL be a list from `status`, `headers` and `body`, defaulting to `[status, body]`. When headers are compared, only headers listed in `headers.include` (default: `content-type`) SHALL be stored, with names lower-cased and sorted.

#### Scenario: Header change ignored by default
- **GIVEN** the default `compare` and a response whose `Date` header changed
- **THEN** the snapshot SHALL still match

#### Scenario: Content type compared
- **GIVEN** `compare: [status, headers, body]` and `content-type` changing from `application/json` to `text/html`
- **THEN** the assertion SHALL fail, showing the header diff

### Requirement: Ignored paths
`ignore` SHALL be a list of JSONPath expressions whose matched values are replaced with the placeholder `«ignored»` before storing and comparing. A path that matches nothing SHALL not be an error.

#### Scenario: Ignore generated id
- **GIVEN** `ignore: ["$.id", "$.items[*].created_at"]`
- **WHEN** a response differs only in those fields
- **THEN** the assertion SHALL pass

### Requirement: Regex replacements
`replace` SHALL be a list of `{pattern, with}` rules applied to string values (JSON bodies) or to the whole text (non-JSON bodies), in order, after ignore rules. Invalid regexes SHALL fail at load time with the pattern and the error.

#### Scenario: Mask order numbers
- **GIVEN** `replace: [{pattern: "ORD-\\d+", with: "ORD-«n»"}]`
- **THEN** `ORD-123` and `ORD-987` SHALL normalize to the same value

#### Scenario: Invalid regex
- **GIVEN** `replace: [{pattern: "(", with: "x"}]`
- **THEN** loading SHALL fail with `<path>:<line>: invalid regex '(': <reason>`

### Requirement: Built-in presets
`presets` SHALL accept `timestamps` (RFC 3339 and ISO 8601 date-times), `dates`, `uuids`, `etags`, `epoch_ms` (13-digit integers in fields named `*_at`, `*time*` or `timestamp`) and `jwt`. Each SHALL replace matches with a typed placeholder such as `«timestamp»`. The default SHALL be `[timestamps, uuids]`, and `presets: []` SHALL disable all presets.

#### Scenario: Default presets
- **GIVEN** no `presets` key and a body with `"updated":"2026-11-17T10:00:00Z"`
- **THEN** the stored value SHALL be `«timestamp»`

#### Scenario: Presets disabled
- **GIVEN** `presets: []`
- **THEN** the timestamp SHALL be stored verbatim

### Requirement: Array ordering
`sort` SHALL be a list of `{path, by}` rules that sort the array at a JSONPath by the value at a relative key path (or by whole-value canonical JSON when `by` is omitted) before comparing. Without a rule, array order SHALL be significant.

#### Scenario: Unordered list
- **GIVEN** `sort: [{path: "$.users", by: "id"}]` and the same users returned in a different order
- **THEN** the assertion SHALL pass

#### Scenario: Order significant by default
- **GIVEN** no sort rule and reordered users
- **THEN** the assertion SHALL fail

### Requirement: Normalization order
Normalization SHALL run in a fixed order: secret redaction, then `ignore`, `presets`, `replace`, `sort`, and finally canonical serialization. The same normalized form SHALL be used both for storing and for comparing.

#### Scenario: Deterministic normalization
- **GIVEN** the same response normalized twice
- **THEN** both normalized forms SHALL be byte-identical

### Requirement: Secret redaction before storage
Secret values and sensitive headers SHALL be redacted to `«redacted:name»` before a snapshot is written or compared, as required by `environments`. A snapshot file SHALL never contain a resolved secret value, even with `--update-snapshots`.

#### Scenario: Token echoed in body
- **GIVEN** a secret `token=abc123` and a response body echoing `"token":"abc123"`
- **WHEN** the snapshot is recorded
- **THEN** the file SHALL contain `«redacted:token»` and not `abc123`

### Requirement: Snapshot file format
A snapshot file SHALL be YAML with `version: 1`, `request` (relative path), `name`, `kind`, `compare`, and `status`, `headers` and `body` as applicable. JSON bodies SHALL be stored as YAML structures with object keys sorted. Text SHALL be stored as a literal block. Files SHALL end with a newline and use 2-space indentation.

#### Scenario: Stable bytes
- **GIVEN** two runs producing semantically equal JSON with keys in different orders
- **THEN** both SHALL produce byte-identical snapshot files

### Requirement: Binary bodies
Bodies that aren't valid UTF-8, or whose content type is not textual, SHALL be stored as `{size, sha256}` instead of their content. A mismatch SHALL report old and new size and hash.

#### Scenario: Image changed
- **GIVEN** a stored PNG snapshot and a response with a different hash
- **THEN** the failure SHALL read `binary body changed: 10240 B sha256:ab12… → 10301 B sha256:9f0c…`

### Requirement: Structural diff on failure
For JSON bodies, a mismatch SHALL show each change as a JSONPath with its kind (`added`, `removed`, `changed`) and old and new values, capped at 50 entries plus a count of the rest. Non-JSON text SHALL show a unified line diff with 3 lines of context. Colors SHALL follow `cli-runner` terminal rules.

#### Scenario: Changed field
- **GIVEN** a stored `{"user":{"role":"admin"}}` and a response with `"role":"viewer"`
- **THEN** the failure SHALL include `changed $.user.role: "admin" → "viewer"`

#### Scenario: Large diff truncated
- **GIVEN** 80 differing paths
- **THEN** 50 SHALL be shown, followed by `… and 30 more changes`

### Requirement: Snapshot results in reports
Snapshot outcomes (`matched`, `recorded`, `updated`, `failed`, `missing`) SHALL appear in every reporter. The JSON report SHALL include the snapshot path and the structured diff as an additive field of `rustman.report/v1`. JUnit SHALL put the diff text in the failure message.

#### Scenario: JUnit failure body
- **GIVEN** a failing snapshot in a run with `--reporter junit:out.xml`
- **THEN** the testcase failure message SHALL contain the diff text

### Requirement: Snapshot list command
`rustman snapshot list [path]` SHALL list every snapshot with its request, name and state (`ok`, `orphaned`, or `unreferenced name`), without sending requests. `--json` SHALL print a machine-readable list.

#### Scenario: Orphan shown
- **GIVEN** `__snapshots__/old.default.snap.yaml` whose request file was deleted
- **THEN** it SHALL be listed with state `orphaned`

### Requirement: Prune obsolete snapshots
`rustman snapshot prune [path]` SHALL find snapshot files whose request file no longer exists or no longer declares that snapshot name. It SHALL list them and delete them only with `--yes` (or after confirmation on a TTY). `--dry-run` SHALL never delete. It SHALL exit 0, or 2 on I/O errors.

#### Scenario: Prune with confirmation
- **GIVEN** two obsolete snapshot files
- **WHEN** the user runs `rustman snapshot prune --yes`
- **THEN** both files SHALL be deleted and listed in the output

#### Scenario: Non-interactive without --yes
- **GIVEN** stdin is not a TTY and `--yes` is absent
- **THEN** nothing SHALL be deleted and the command SHALL print the candidates and exit 0

### Requirement: TUI snapshot review
When a sent request has a failing or new snapshot, the response viewer SHALL show a Snapshot tab with the diff. `a` SHALL approve (write the new snapshot) and `r` SHALL reject (keep the stored one). After a run, `Alt+A` SHALL open a review queue to step through every failing snapshot. CI mode SHALL disable approval.

#### Scenario: Approve from TUI
- **GIVEN** a request whose snapshot failed
- **WHEN** the user opens the Snapshot tab and presses `a`
- **THEN** the snapshot file SHALL be rewritten and the tab SHALL show `approved`

#### Scenario: Reject keeps file
- **WHEN** the user presses `r`
- **THEN** the file SHALL be unchanged and the assertion SHALL stay failed

### Requirement: Snapshot of subscription events
For exchanges with a bounded event list (`jsonrpc` subscriptions, `graphql` subscriptions), the snapshot body SHALL be the event list after normalization. When a run collects a different number of events than the snapshot, it SHALL report `event count changed: <old> → <new>` in addition to the diff.

#### Scenario: Event count mismatch
- **GIVEN** a snapshot with 3 events and a run that collected 2
- **THEN** the failure SHALL include `event count changed: 3 → 2`
