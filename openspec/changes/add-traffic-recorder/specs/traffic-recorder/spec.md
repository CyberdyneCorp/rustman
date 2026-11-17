## Purpose

Records real HTTP(S) traffic through a local proxy and turns it into reviewable request files, so an existing app's API usage becomes a versioned Rustman collection without leaking secrets.

## ADDED Requirements

### Requirement: Forward proxy command
`rustman record` SHALL start an HTTP forward proxy on `127.0.0.1:8888` (override with `--port`, `--host`). It SHALL accept plain HTTP proxy requests and `CONNECT` tunnels, and print the proxy URL plus the `HTTP_PROXY`/`HTTPS_PROXY` exports needed to route a CLI through it. A bind failure SHALL exit with code 2.

#### Scenario: Default start
- **WHEN** the user runs `rustman record`
- **THEN** the proxy SHALL listen on `127.0.0.1:8888`
- **AND** the output SHALL include `export HTTPS_PROXY=http://127.0.0.1:8888`

#### Scenario: Port busy
- **GIVEN** port 8888 is in use
- **WHEN** the user runs `rustman record`
- **THEN** the error `record: cannot bind 127.0.0.1:8888: address in use` SHALL be shown and the exit code SHALL be 2

### Requirement: Loopback by default
The proxy SHALL bind only to loopback unless `--allow-remote` is given. With `--allow-remote`, it SHALL print a warning that anyone on the network can use the proxy and see intercepted traffic. In the TUI, binding to a non-loopback address SHALL require a confirmation.

#### Scenario: Remote bind refused without flag
- **WHEN** the user runs `rustman record --host 0.0.0.0`
- **THEN** the command SHALL exit with code 2 and the message `binding a non-loopback address requires --allow-remote`

### Requirement: Reverse-proxy mode
`rustman record --upstream <url>` SHALL accept plain requests on the listen address and forward them to the upstream origin, rewriting `Host` to the upstream. It SHALL record exchanges exactly like forward mode. HTTPS upstreams SHALL be verified with the standard TLS rules from `http-requests`.

#### Scenario: App without proxy support
- **GIVEN** `rustman record --upstream https://api.example.com --port 9000`
- **WHEN** a client sends `GET http://127.0.0.1:9000/v1/users`
- **THEN** the request SHALL be forwarded to `https://api.example.com/v1/users` and the exchange SHALL be captured

### Requirement: Interception scope
TLS SHALL be decrypted only for hosts matching the include list (`--intercept <glob>`, repeatable, or `[record].intercept`). Every other `CONNECT` SHALL be relayed as an opaque tunnel and recorded only as a host, port and byte-count entry. With no include list and no CA, all HTTPS SHALL pass through untouched.

#### Scenario: Host not included
- **GIVEN** `--intercept "*.example.com"`
- **WHEN** a client connects through the proxy to `www.google.com:443`
- **THEN** the tunnel SHALL be relayed without decryption
- **AND** the capture list SHALL show `TUNNEL www.google.com:443` with byte counts only

#### Scenario: Included host
- **GIVEN** `--intercept "*.example.com"` and an installed local CA
- **WHEN** a client calls `https://api.example.com/v1/me` through the proxy
- **THEN** the full request and response SHALL be captured

### Requirement: Local CA generation and storage
The local CA SHALL be created only by `rustman record ca init`, or on first `--intercept` after an explicit prompt. It SHALL be per workspace, valid for 1 year, constrained to TLS server authentication, with its private key in the OS keychain and its certificate under the user config directory, never inside the workspace.

#### Scenario: CA files stay out of the workspace
- **WHEN** the user runs `rustman record ca init`
- **THEN** no file SHALL be written under the workspace root
- **AND** the CA private key SHALL be retrievable only from the OS keychain

#### Scenario: Keychain unavailable
- **GIVEN** no OS keychain is available
- **WHEN** the user runs `rustman record ca init`
- **THEN** the command SHALL fail with exit code 2 and the message `record: a keychain is required to store the CA private key`

### Requirement: CA is never trusted automatically
Rustman SHALL NOT add the CA to any trust store on its own. `rustman record ca install` SHALL print what will be trusted (subject, SHA-256 fingerprint, expiry), warn that it allows decryption of matching traffic on this machine, and require typing `yes` (or `--yes`). `uninstall` SHALL remove it, and `export` SHALL write only the public certificate in PEM.

#### Scenario: Install requires confirmation
- **WHEN** the user runs `rustman record ca install` and answers `no`
- **THEN** no trust store SHALL be modified and the exit code SHALL be 1

#### Scenario: Export never includes the key
- **WHEN** the user runs `rustman record ca export --out ca.pem`
- **THEN** `ca.pem` SHALL contain exactly one `CERTIFICATE` PEM block and no private key

### Requirement: CA rotation
`rustman record ca rotate` SHALL generate a new CA, remove the old one from the keychain and from any trust store it was installed in by Rustman, and report both fingerprints. Leaf certificates minted from the old CA SHALL be discarded.

#### Scenario: Rotate
- **GIVEN** an installed CA with fingerprint `AA:…`
- **WHEN** the user runs `rustman record ca rotate --yes`
- **THEN** the old CA SHALL be uninstalled and a new fingerprint SHALL be printed

### Requirement: Certificate pinning failures
When a client aborts the TLS handshake against an intercepted host (for example, because it pins certificates or doesn't trust the CA), the capture list SHALL show a `TLS REJECTED` entry for that host, with the hint `client rejected the Rustman CA (not installed or pinned); exclude the host or install the CA`.

#### Scenario: Pinned client
- **GIVEN** `api.example.com` is intercepted and the client pins its certificate
- **WHEN** the client's handshake fails
- **THEN** a `TLS REJECTED api.example.com` entry SHALL be shown with the hint

### Requirement: Capture filters
Captures SHALL be filtered by `--include-host`/`--exclude-host` globs, `--method`, and `--content-type` prefixes. By default, static assets SHALL be dropped (images, fonts, CSS, JavaScript, media, source maps by content type or extension). `--keep-static` SHALL disable that rule. Filtered exchanges SHALL still be proxied.

#### Scenario: Static assets dropped
- **WHEN** a browser loads a page that fetches `app.js`, `logo.png` and `GET /api/items`
- **THEN** only `GET /api/items` SHALL appear in the capture list
- **AND** all three requests SHALL still be proxied

### Requirement: Capture limits
Each captured body SHALL be stored up to `max_body` (default 10 MiB). Larger bodies SHALL be proxied in full but stored truncated and marked `truncated`. The capture store SHALL be bounded by `max_captures` (default 2000) and `max_buffer_bytes` (default 512 MiB), evicting the oldest captures first.

#### Scenario: Oversized response
- **WHEN** an intercepted response body is 25 MiB
- **THEN** the client SHALL receive all 25 MiB
- **AND** the capture SHALL hold the first 10 MiB and be marked `truncated`

### Requirement: Live capture list
The TUI SHALL provide a recorder pane listing captures as they arrive (time, method, host, path, status, duration, size), with filtering, multi-select and a detail view. `rustman record --json` SHALL print one redacted JSON object per capture to stdout.

#### Scenario: JSON lines output
- **WHEN** the user runs `rustman record --json` and one request passes through
- **THEN** stdout SHALL contain one JSON line with `method`, `url`, `status`, `duration_ms` and redacted headers

### Requirement: Save captures as request files
Saving captures (selected ones via the TUI action, or all of them on exit with `rustman record --save`) SHALL write `*.rman.yaml` files into a target collection (`--into <dir>`, default `recorded/<host>/`). Captures with the same method and templated path SHALL be deduplicated into one file. File names SHALL derive from the method and path.

#### Scenario: Dedupe by templated path
- **GIVEN** captures `GET /users/17` and `GET /users/42`
- **WHEN** the user saves both
- **THEN** one file `get-users-id.rman.yaml` SHALL be written with url `{{base_url}}/users/{{user_id}}`

### Requirement: Path and id parameterization
When saving, path segments that look like identifiers (integers, UUIDs, 24-hex ObjectIds, ULIDs) SHALL become variables named after the previous segment in singular form (`/users/42` → `{{user_id}}`, fallback `{{id_N}}`). The first observed value SHALL be stored as a default in the generated environment.

#### Scenario: UUID segment
- **GIVEN** a capture `GET /orders/9b2c…-…/items`
- **WHEN** it is saved
- **THEN** the URL SHALL be `{{base_url}}/orders/{{order_id}}/items`

### Requirement: Base URL extraction
Saving SHALL create or update an environment (`--env <name>`, default `recorded`) with `base_url` set to the scheme, host and port of the captures. A different origin in the same save SHALL get its own variable (`base_url_2`, …) and a warning.

#### Scenario: Environment created
- **WHEN** captures from `https://api.example.com` are saved with `--env staging`
- **THEN** `environments/staging.yaml` SHALL contain `base_url: https://api.example.com`

### Requirement: Secrets become placeholders
Before anything is written, values of `Authorization`, `Proxy-Authorization`, `Cookie`, `Set-Cookie`, `X-Api-Key`-style headers, and query or body fields named like `token`, `password`, `secret`, `api_key` SHALL be replaced by `{{secret:<name>}}` references. Raw values SHALL be stored only through the `environments` secret store, and only with `--store-secrets`.

#### Scenario: Bearer token captured
- **GIVEN** a capture with `Authorization: Bearer eyJ…`
- **WHEN** it is saved
- **THEN** the request file SHALL contain `Authorization: Bearer {{auth_token}}` with `auth_token` declared as a secret
- **AND** the string `eyJ` SHALL NOT appear in any file under the workspace

### Requirement: Redaction in display and export
Captures SHALL be redacted in the TUI, JSON-lines output and logs using the `environments` redaction rules. They SHALL be kept unredacted only in memory, for the purpose of saving secrets with `--store-secrets` or replaying.

#### Scenario: Cookie hidden in the pane
- **WHEN** a captured request carries `Cookie: sid=abc`
- **THEN** the recorder pane SHALL show `Cookie: «redacted:Cookie»`

### Requirement: HAR export
`rustman record --har <file>` SHALL write all captures as HAR 1.2 when the session ends, and a TUI action SHALL export the selected captures at any time. Secrets SHALL be redacted unless `--show-secrets` is given. Tunnel-only and truncated entries SHALL be marked in the HAR `comment` field.

#### Scenario: HAR validity
- **WHEN** a `rustman record --har session.har` session with 3 captures is stopped with Ctrl+C
- **THEN** `session.har` SHALL validate against the HAR 1.2 schema and contain 3 entries with redacted `Authorization` values
