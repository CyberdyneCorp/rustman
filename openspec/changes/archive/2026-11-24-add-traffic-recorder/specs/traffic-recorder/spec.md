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

### Requirement: Network clients reach only public addresses
Requests from clients that are not on loopback SHALL reach only globally routable addresses; every other address SHALL be refused by default, decided on the resolved addresses that are then connected to. Clients on loopback MAY reach local and private targets.

#### Scenario: Ranges refused
- **WHEN** a client on another machine targets a loopback, unspecified, private (RFC 1918), CGNAT (`100.64.0.0/10`), link-local, unique local (`fc00::/7`), `192.0.0.0/24`, benchmarking, documentation, multicast, reserved (`240.0.0.0/4`) or broadcast address, or an IPv6 address wrapping one (IPv4-mapped, IPv4-compatible, NAT64 `64:ff9b::/96`, 6to4 `2002::/16`)
- **THEN** the request or tunnel SHALL be refused without connecting

#### Scenario: Network client asks for a private address
- **GIVEN** `rustman record --host 0.0.0.0 --allow-remote`
- **WHEN** a client on another machine sends `GET http://10.0.0.5/admin` or `CONNECT 100.100.100.200:80` through the proxy
- **THEN** the proxy SHALL answer `403` without connecting to the target
- **AND** a name resolving to a non-public address SHALL be refused after DNS, without connecting

### Requirement: Reverse-proxy mode
`rustman record --upstream <url>` SHALL accept plain requests on the listen address and forward them to the upstream origin, rewriting `Host` to the upstream. It SHALL record exchanges exactly like forward mode. HTTPS upstreams SHALL be verified with the standard TLS rules from `http-requests`.

#### Scenario: App without proxy support
- **GIVEN** `rustman record --upstream https://api.example.com --port 9000`
- **WHEN** a client sends `GET http://127.0.0.1:9000/v1/users`
- **THEN** the request SHALL be forwarded to `https://api.example.com/v1/users` and the exchange SHALL be captured

### Requirement: Interception scope
TLS SHALL be decrypted only for hosts matching the include list (`--intercept <glob>`, repeatable, or `[record].intercept`). Every other `CONNECT` SHALL be relayed as an opaque tunnel and recorded only as a host, port and byte-count entry. With no include list and no CA, all HTTPS SHALL pass through untouched. A `CONNECT` host that is neither a valid DNS name nor an IP address SHALL get `400` before any certificate is minted.

#### Scenario: Host not included
- **GIVEN** `--intercept "*.example.com"`
- **WHEN** a client connects through the proxy to `www.google.com:443`
- **THEN** the tunnel SHALL be relayed without decryption
- **AND** the capture list SHALL show `TUNNEL www.google.com:443` with byte counts only

#### Scenario: Included host
- **GIVEN** `--intercept "*.example.com"` and an installed local CA
- **WHEN** a client calls `https://api.example.com/v1/me` through the proxy
- **THEN** the full request and response SHALL be captured

#### Scenario: Wildcard CONNECT host
- **GIVEN** `--intercept "*.example.com"` and a local CA
- **WHEN** a client sends `CONNECT *.example.com:443`
- **THEN** the proxy SHALL answer `400` and SHALL NOT mint a certificate

### Requirement: Local CA generation and storage
The local CA SHALL be created only by `rustman record ca init`, or on first `--intercept` after an explicit prompt. It SHALL be per workspace, valid for 1 year, constrained to TLS server authentication, with its private key in the OS keychain and its certificate under the user config directory, never inside the workspace.

#### Scenario: CA files stay out of the workspace
- **WHEN** the user runs `rustman record ca init`
- **THEN** no CA file (private key or certificate) SHALL be written under the workspace root; the only workspace file it may create is the git-ignored per-clone id `.rustman/workspace-id` (design D3), when the workspace has none yet
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

### Requirement: CA removal
`rustman record ca remove` SHALL print the CA's fingerprint, its keychain entry, its certificate path and the uninstall commands, require typing `yes` (or `--yes`), then remove the CA from every trust store Rustman installed it in and delete its private key from the keychain and its certificate. Declining SHALL change nothing and exit with code 1; a failed uninstall SHALL keep the CA.

#### Scenario: Remove after use
- **GIVEN** a workspace CA installed in the user's trust store
- **WHEN** the user runs `rustman record ca remove --yes`
- **THEN** the CA SHALL be uninstalled, its keychain entry and certificate SHALL be deleted, and `rustman record ca export` SHALL then exit with code 2

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

### Requirement: In-flight capture budget
The copies of bodies still in flight SHALL share a budget of `max_buffer_bytes`: past it, a body SHALL still be proxied in full and its capture SHALL be marked `truncated`. An intercepted HTTP/2 connection SHALL allow at most 100 concurrent streams.

#### Scenario: In-flight budget used up
- **GIVEN** the bodies in flight already hold `max_buffer_bytes`
- **WHEN** another response streams through the proxy
- **THEN** the client SHALL receive the whole body
- **AND** its capture SHALL be marked `truncated`

### Requirement: Connection timeouts
An upstream SHALL get `response_timeout_ms` (default 60 s), counted once the request body is sent, to send a response head; past it the client SHALL get `504` and the exchange SHALL be recorded with the error. Any client connection (plain, tunnel or intercepted) idle either way for `idle_timeout_ms` (default 5 minutes) SHALL be closed; a closed tunnel SHALL be recorded with an idle-timeout error. Both SHALL be configurable in `[record]`.

#### Scenario: Silent upstream
- **WHEN** an upstream accepts a request and sends no response head for 60 seconds
- **THEN** the client SHALL get `504 Gateway Timeout`
- **AND** the capture SHALL show the error `no response within 60 s`

#### Scenario: Idle tunnel
- **WHEN** an opaque tunnel carries no byte for 5 minutes
- **THEN** it SHALL be closed and its entry SHALL carry `idle timeout: no traffic for 300 s`

#### Scenario: Stalled request body
- **WHEN** a client sends a plain proxy request with `Content-Length: 100000` (or a chunked body), sends part of the body and then nothing for 5 minutes
- **THEN** the proxy SHALL close the connection and free its connection slot

### Requirement: Live capture list
The TUI SHALL provide a recorder pane listing captures as they arrive (time, method, host, path, status, duration, size), with filtering, multi-select and a detail view. `rustman record --json` SHALL print one redacted JSON object per capture to stdout.

#### Scenario: JSON lines output
- **WHEN** the user runs `rustman record --json` and one request passes through
- **THEN** stdout SHALL contain one JSON line with `method`, `url`, `status`, `duration_ms` and redacted headers

#### Scenario: Output slower than the traffic
- **WHEN** stdout cannot keep up and lines are dropped from the output queue
- **THEN** stderr SHALL show `warning: <n> captures not shown (output too slow)`
- **AND** the lines still queued when recording stops SHALL be printed before the save and HAR output

### Requirement: Save captures as request files
Saving captures (selected ones via the TUI action, or all of them on exit with `rustman record --save`) SHALL write `*.rman.yaml` files into a target collection (`--into <dir>`, default `recorded/<host>/`). Captures with the same method and templated path SHALL be deduplicated into one file. File names SHALL derive from the method and path.

A save with no request among the captures SHALL write nothing (no environment either) and SHALL say that nothing was saved.

#### Scenario: Dedupe by templated path
- **GIVEN** captures `GET /users/17` and `GET /users/42`
- **WHEN** the user saves both
- **THEN** one file `get-users-id.rman.yaml` SHALL be written with url `{{base_url}}/users/{{user_id}}`

#### Scenario: Nothing recorded
- **WHEN** a `rustman record --save` session that recorded no request is stopped with Ctrl+C
- **THEN** no file SHALL be written and stderr SHALL show `No requests recorded; nothing saved`

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
Before anything is written, values of `Authorization`, `Proxy-Authorization`, `Cookie`, `Set-Cookie`, `X-Api-Key`-style headers, and query or body fields named like `token`, `password`, `secret`, `api_key` SHALL be replaced by `{{<name>}}` placeholders declared under `secrets:` in the target environment. Raw values SHALL be stored only through the `environments` secret store, and only with `--store-secrets`.

#### Scenario: Bearer token captured
- **GIVEN** a capture with `Authorization: Bearer eyJ…`
- **WHEN** it is saved
- **THEN** the request file SHALL contain `Authorization: Bearer {{auth_token}}` with `auth_token` declared as a secret
- **AND** the string `eyJ` SHALL NOT appear in any file under the workspace

### Requirement: Redaction in display and export
Captures SHALL be redacted in the TUI, JSON-lines output, HAR export and logs using the `environments` redaction rules. Every value that saving would replace by a placeholder (secret-named headers and fields, values starting with an auth scheme, known token formats under any name) SHALL be redacted there too. They SHALL be kept unredacted only in memory, for the purpose of saving secrets with `--store-secrets` or replaying.

#### Scenario: Cookie hidden in the pane
- **WHEN** a captured request carries `Cookie: sid=abc`
- **THEN** the recorder pane SHALL show `Cookie: «redacted:Cookie»`

#### Scenario: Secrets found by their shape
- **WHEN** a captured request carries `X-Forward: ghp_…`, the JSON body `{"data":"ghp_…","x":"AKIAIOSFODNN7EXAMPLE","authorization":"Bearer X"}` and a `ghp_…` query value
- **THEN** none of these raw values SHALL appear in the HAR export, the `--json` line or the recorder pane
- **AND** saving SHALL NOT warn that any of them was kept

### Requirement: HAR export
`rustman record --har <file>` SHALL write all captures as HAR 1.2 when the session ends, and a TUI action SHALL export the selected captures at any time. Secrets SHALL be redacted unless `--show-secrets` is given. Tunnel-only and truncated entries SHALL be marked in the HAR `comment` field. The file SHALL be readable by the current user only (mode `0600` on Unix).

#### Scenario: HAR validity
- **WHEN** a `rustman record --har session.har` session with 3 captures is stopped with Ctrl+C
- **THEN** `session.har` SHALL validate against the HAR 1.2 schema and contain 3 entries with redacted `Authorization` values
