# request-history Specification

## Purpose
Keeps a local, redacted record of past exchanges, so users and later front-ends (TUI, agent interface) can inspect, search and re-send what happened. Retention is bounded, and nothing is ever stored inside versioned workspace files.

## Requirements

### Requirement: Record exchanges
Every executed request (from `send`, `run` and later front-ends) SHALL be appended to history as one exchange: id (ULID), timestamp, workspace-relative request path or `adhoc`, environment, the resolved request, the response status, headers, body (or spill reference), timings, assertion results and error. `--no-history` and `[history] enabled = false` SHALL disable recording.

#### Scenario: Send is recorded
- **WHEN** the user runs `rustman send get-user`
- **THEN** `rustman history list` SHALL show the exchange as the newest entry

#### Scenario: Disabled
- **WHEN** the user runs `rustman run . --no-history`
- **THEN** no new exchange SHALL be written

### Requirement: Storage location
History SHALL be stored under `.rustman/history/` in the workspace, or under the platform data directory (`<data_dir>/rustman/history/`) for ad-hoc requests made outside a workspace. `.rustman/` SHALL be covered by the workspace's `.gitignore`.

#### Scenario: Ad-hoc outside workspace
- **GIVEN** no workspace
- **WHEN** an ad-hoc `rustman send GET https://example.com` runs
- **THEN** the exchange SHALL be written under the platform data directory

### Requirement: Retention
History SHALL keep at most `[history] max_entries` exchanges (default 500) per store. The oldest exchanges SHALL be pruned after each write, along with their spill files. `rustman history clear` SHALL delete all exchanges.

#### Scenario: Prune oldest
- **GIVEN** 500 stored exchanges
- **WHEN** one more request is sent
- **THEN** the store SHALL hold 500 exchanges and the oldest SHALL be gone, including its spill file

### Requirement: Redacted at rest
History SHALL store exchanges only after applying the secret redaction invariant. `--show-secrets` SHALL NOT affect what is stored. The userinfo of every stored URL (`user:password@`) SHALL be stored as `«redacted:userinfo»`, and the replay part SHALL keep it only when it comes from secrets.

#### Scenario: Token not stored
- **GIVEN** a request with a secret bearer token
- **WHEN** the exchange is recorded
- **THEN** no file under `.rustman/history/` SHALL contain the token value

#### Scenario: Password typed into a URL
- **GIVEN** a request to `https://alice:hunter2@api.example.com/x` that uses no secret variable
- **WHEN** the exchange is recorded
- **THEN** the entry SHALL show `https://«redacted:userinfo»@api.example.com/x` and no file SHALL contain `hunter2`
- **AND** `rustman history replay` SHALL fail before sending, naming `the URL's userinfo` as not kept
- **AND** a password holding `)` or `'`, or a URL nested in the query (`?next=https://bob:pw@intranet/`), SHALL be stored the same way, in the replay part too

### Requirement: List and search
`rustman history list [--limit n] [--status <code|class>] [--method m] [--grep <text>] [--since <duration>]` SHALL list exchanges newest first, as one line each: time, method, URL, status and total ms. `--grep` SHALL match URL, request body and response body text. `--json` SHALL print machine-readable output.

#### Scenario: Find failed calls
- **GIVEN** history with 200 and 500 responses
- **WHEN** the user runs `rustman history list --status 5xx --since 1h`
- **THEN** only 5xx exchanges from the last hour SHALL be listed

### Requirement: Inspect and replay
`rustman history show <id>` SHALL print a full exchange. `rustman history replay <id>` SHALL re-send the stored resolved request, re-resolving redacted secrets from the current secret store, and record a new exchange.

#### Scenario: Replay with redacted secret
- **GIVEN** an exchange whose `Authorization` was redacted, and the secret still set
- **WHEN** it is replayed
- **THEN** the request SHALL carry the real current secret value

#### Scenario: Replay with secret removed
- **GIVEN** the secret has since been deleted
- **WHEN** the exchange is replayed
- **THEN** replay SHALL fail with `unresolved variable(s): <name>`, and nothing SHALL be sent

### Requirement: Concurrency-safe writes
Concurrent writers (parallel runs, multiple processes) SHALL NOT corrupt history. Each exchange SHALL be written atomically (temp file + rename) with a file lock around pruning. A partially written or unreadable entry SHALL be skipped with a warning, not crash readers.

#### Scenario: Two runs at once
- **WHEN** two `rustman run --parallel 4` processes write to the same workspace at the same time
- **THEN** every exchange SHALL be readable afterwards, and `history list` SHALL show all of them up to the retention limit
