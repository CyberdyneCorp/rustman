# Spec Delta

## Purpose

Resolves `{{variable}}` placeholders from environments, captures, overrides and dynamic generators. Keeps secrets out of the workspace, and redacts them everywhere they could leak.

## ADDED Requirements

### Requirement: Environment files
Each file `environments/<name>.yaml` SHALL define one environment, as a mapping of `variables` (string, number or boolean values) plus an optional `secrets` list naming variables whose values are stored outside the workspace. The environment name SHALL be the file stem, restricted to `[A-Za-z0-9_-]+`.

#### Scenario: Load environments
- **GIVEN** `environments/local.yaml` and `environments/staging.yaml`
- **WHEN** the user runs `rustman env list`
- **THEN** the output SHALL list `local` and `staging`, marking the active one with `*`

#### Scenario: Invalid name
- **GIVEN** a file `environments/my env.yaml`
- **WHEN** the workspace loads
- **THEN** a warning `ignoring environment 'my env': invalid name` SHALL be emitted

### Requirement: Active environment
Exactly one environment or none SHALL be active. The active one SHALL be chosen by `--env <name>`, else by `RUSTMAN_ENV`, else by `[defaults] default_environment`, else none. Naming a missing environment SHALL fail with `error: environment '<name>' not found (available: <list>)` and exit 2.

#### Scenario: CLI flag wins
- **GIVEN** `default_environment = "local"` and `RUSTMAN_ENV=staging`
- **WHEN** the user runs `rustman run . --env prod`
- **THEN** `prod` SHALL be active

#### Scenario: Missing environment
- **WHEN** the user passes `--env qa` and `environments/qa.yaml` does not exist
- **THEN** the command SHALL exit 2 with the not-found error listing available environments

### Requirement: Variable precedence
Variables SHALL resolve from the highest-precedence source first: runtime overrides (`--var k=v`) > captures from earlier requests in the same run > request-level `variables` > collection `variables` (nearest folder first) > persisted captures (`.rustman/state/<env>.yaml`) > active environment > `rustman.toml [variables]`. Resolution SHALL be case-sensitive.

#### Scenario: Override beats environment
- **GIVEN** environment `base_url=http://localhost:8080`
- **WHEN** the user runs with `--var base_url=http://127.0.0.1:9000`
- **THEN** `{{base_url}}` SHALL resolve to `http://127.0.0.1:9000`

#### Scenario: Capture beats environment
- **GIVEN** environment `token=old`, and an earlier request in the run captured `token=new`
- **WHEN** the next request uses `{{token}}`
- **THEN** it SHALL resolve to `new`

### Requirement: Interpolation scope and syntax
`{{name}}` SHALL be interpolated in the URL, query, headers, body content, form and multipart text fields, auth fields and file paths. A literal `{{` SHALL be written as `\{{`. Interpolation SHALL be non-recursive by default: a resolved value containing `{{x}}` SHALL NOT be expanded again.

#### Scenario: Escaped braces
- **GIVEN** body text `\{{not_a_var}}`
- **WHEN** sent
- **THEN** the body SHALL contain `{{not_a_var}}` literally

#### Scenario: No recursive expansion
- **GIVEN** variable `a="{{b}}"` and `b="x"`
- **WHEN** `{{a}}` is resolved
- **THEN** the result SHALL be the literal `{{b}}`

### Requirement: Unresolved variables are errors
If any placeholder cannot be resolved, the request SHALL NOT be sent. It SHALL fail with `unresolved variable(s): <names>`, listing every missing name, and the runner SHALL count it as an error (not as an assertion failure).

#### Scenario: Missing variable blocks the send
- **GIVEN** URL `{{base_url}}/users` and no `base_url` defined anywhere
- **WHEN** the request is run
- **THEN** no network connection SHALL be opened
- **AND** the result SHALL be `error: unresolved variable(s): base_url`

### Requirement: Dynamic variables
The system SHALL provide these dynamic variables, re-evaluated for every occurrence: `{{$uuid}}` (UUIDv4), `{{$timestamp}}` (Unix seconds), `{{$timestampMs}}`, `{{$isoTimestamp}}` (RFC 3339 UTC), `{{$randomInt}}` (0–1000 inclusive), `{{$randomInt min max}}`, `{{$randomString n}}` (alphanumeric, default 16) and `{{$env NAME}}` (an OS environment variable; unresolved if it's unset).

#### Scenario: Two UUIDs differ
- **GIVEN** a body `{"a":"{{$uuid}}","b":"{{$uuid}}"}`
- **WHEN** it is sent
- **THEN** `a` and `b` SHALL be distinct valid UUIDv4 strings

#### Scenario: Bounded random int
- **WHEN** `{{$randomInt 5 7}}` is resolved 1000 times
- **THEN** every value SHALL be in `{5, 6, 7}`

#### Scenario: Missing OS variable
- **GIVEN** `API_KEY` is not set in the OS environment
- **WHEN** `{{$env API_KEY}}` is resolved
- **THEN** the request SHALL fail as unresolved, naming `$env API_KEY`

### Requirement: Secret storage
Values of variables listed under `secrets` SHALL never be stored in workspace files. They SHALL be read from the OS keychain (service `rustman`, account `<workspace-id>/<env>/<name>`). If the keychain is unavailable, they SHALL fall back to `.rustman/secrets.yaml` and emit a one-time warning. `rustman secret set <name> [--env e]` SHALL prompt for the value without echoing it.

#### Scenario: Set and use a secret
- **GIVEN** `environments/prod.yaml` lists `secrets: [api_token]`
- **WHEN** the user runs `rustman secret set api_token --env prod` and enters a value
- **THEN** the value SHALL be stored in the OS keychain and no workspace file SHALL change

#### Scenario: Keychain unavailable
- **GIVEN** a headless Linux host with no secret service running
- **WHEN** a secret is set
- **THEN** it SHALL be written to `.rustman/secrets.yaml` with file mode `0600` on Unix
- **AND** the warning `OS keychain unavailable; storing secrets in .rustman/secrets.yaml` SHALL be printed once

#### Scenario: Secret not set
- **GIVEN** `api_token` is listed as a secret but has no stored value
- **WHEN** a request uses `{{api_token}}`
- **THEN** it SHALL fail with `unresolved variable(s): api_token (secret not set; run 'rustman secret set api_token --env prod')`

### Requirement: Secret redaction invariant
Secret values, and values of the `Authorization`, `Proxy-Authorization`, `Cookie` and `Set-Cookie` headers, SHALL be replaced by `«redacted:<name>»` in every artifact the system produces: terminal output, logs, history, reports, copy-as-curl and error messages. The only exception is an explicit per-command opt-in (`--show-secrets`).

#### Scenario: Secret echoed by server
- **GIVEN** a secret `api_token=s3cr3t` and a server that echoes request headers in its response body
- **WHEN** the exchange is written to history and to a JSON report
- **THEN** neither artifact SHALL contain the substring `s3cr3t`
- **AND** both SHALL contain `«redacted:api_token»`

#### Scenario: Authorization header without a secret variable
- **GIVEN** a literal header `Authorization: Bearer abc123`
- **WHEN** the pretty reporter prints request details
- **THEN** it SHALL show `Authorization: «redacted:Authorization»`

#### Scenario: Explicit opt-in
- **WHEN** the user runs `rustman send req.rman.yaml --show-secrets`
- **THEN** terminal output SHALL include real values
- **AND** history SHALL still store redacted values

### Requirement: Secrets file is git-ignored
The system SHALL refuse to write `.rustman/secrets.yaml` when Git reports that the path would be tracked (not ignored). It SHALL then fail with `refusing to write secrets: .rustman/ is not git-ignored`.

#### Scenario: Ignore rule removed
- **GIVEN** the `.rustman/` line was removed from `.gitignore` in a Git repository
- **WHEN** a secret falls back to file storage
- **THEN** the write SHALL be refused with the not-ignored error
