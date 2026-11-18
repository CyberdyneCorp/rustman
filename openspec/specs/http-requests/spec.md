# http-requests Specification

## Purpose
Executes `kind: http` requests and produces an exchange. This covers transport (HTTP/1.1 and HTTP/2), timeouts, redirects, TLS, proxies, cookies, auth helpers, timing breakdown, body handling, and curl import/export.

## Requirements

### Requirement: Protocol negotiation
The engine SHALL negotiate HTTP/2 via ALPN for `https` URLs and fall back to HTTP/1.1. Plain `http` URLs SHALL use HTTP/1.1, unless `settings.http2_prior_knowledge: true`. The negotiated version SHALL be recorded in the exchange.

#### Scenario: HTTPS server supporting h2
- **GIVEN** an HTTPS server advertising `h2`
- **WHEN** a request is sent
- **THEN** the exchange SHALL record `http_version: "HTTP/2"`

#### Scenario: Prior knowledge on cleartext
- **GIVEN** an h2c server and `settings.http2_prior_knowledge: true`
- **WHEN** a request is sent to its `http://` URL
- **THEN** the exchange SHALL record `HTTP/2`

### Requirement: Timeouts
The total request timeout SHALL default to 30000 ms. It can be overridden by `rustman.toml [defaults] timeout_ms`, the request's `settings.timeout_ms`, or `--timeout`. A separate connect timeout SHALL default to 10000 ms. A timeout SHALL produce an error result `timeout after <n> ms (<phase>)`, where phase is `connect` or `response`.

#### Scenario: Slow server
- **GIVEN** a server that delays its response by 2 s and `settings.timeout_ms: 500`
- **WHEN** the request is sent
- **THEN** the result SHALL be `error: timeout after 500 ms (response)` within 600 ms

### Requirement: Redirect policy
Redirects SHALL be followed by default, up to 10 hops (`max_redirects`). Each hop SHALL be recorded in the exchange. A `301`/`302`/`303` hop SHALL switch a non-GET/HEAD method to `GET` and drop the body. A `307`/`308` hop SHALL keep the method and body. `follow_redirects: false` SHALL return the 3xx response as is.

#### Scenario: Redirect chain recorded
- **GIVEN** `/a` → 302 → `/b` → 200
- **WHEN** `/a` is requested
- **THEN** the exchange SHALL record final status 200 and the redirect list `[302 /a]`

#### Scenario: Too many redirects
- **GIVEN** a redirect loop
- **WHEN** the request is sent with defaults
- **THEN** the result SHALL be `error: too many redirects (10)`

### Requirement: Redirect credential stripping
Every credential the request carries (`Authorization`, `Proxy-Authorization`, an explicit `Cookie` header, and the `api_key` header or query parameter) SHALL be dropped when a redirect hop changes origin (scheme, host or port).

#### Scenario: Cross-host auth stripping
- **GIVEN** `https://api.example.com/x` redirects to `https://cdn.other.com/x`
- **WHEN** the request carries bearer auth
- **THEN** the second hop SHALL NOT include `Authorization`

#### Scenario: Cross-origin API key stripping
- **GIVEN** `http://localhost:8080/x` redirects to `http://127.0.0.1:8080/x`
- **WHEN** the request carries an `api_key` query parameter
- **THEN** the second hop SHALL NOT include that parameter

### Requirement: TLS verification
TLS certificate and hostname verification SHALL be enabled by default, using the platform trust store plus any `ca_bundle` (PEM) configured in the workspace or request. Disabling it SHALL require `verify_tls: false` or `--insecure`, and every exchange made with verification off SHALL be flagged `insecure: true`.

#### Scenario: Self-signed rejected by default
- **GIVEN** a server with a self-signed certificate
- **WHEN** a request is sent with defaults
- **THEN** the result SHALL be an error starting with `tls: certificate verification failed`

#### Scenario: Custom CA trusted
- **GIVEN** `ca_bundle: ./certs/dev-ca.pem` containing the issuing CA
- **WHEN** the request is sent
- **THEN** it SHALL succeed with verification on

#### Scenario: Insecure flagged
- **WHEN** the request is sent with `--insecure`
- **THEN** it SHALL succeed and the exchange SHALL carry `insecure: true`

### Requirement: Client certificates
A request or workspace SHALL support mutual TLS via `client_cert` and `client_key` PEM paths, or a PKCS#12 bundle plus a passphrase taken from a secret variable.

#### Scenario: mTLS required by server
- **GIVEN** a server that requires a client certificate, and a valid `client_cert`/`client_key`
- **WHEN** the request is sent
- **THEN** the handshake SHALL succeed and the response SHALL be returned

#### Scenario: Missing client cert
- **GIVEN** the same server and no client certificate configured
- **WHEN** the request is sent
- **THEN** the result SHALL be an error starting with `tls:`

### Requirement: Proxies
The engine SHALL honor `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY` and `NO_PROXY` (case-insensitive), unless an explicit `[http] proxy` or request `settings.proxy` is set. Supported schemes SHALL be `http`, `https` and `socks5`. `settings.proxy: none` SHALL bypass all proxies.

#### Scenario: Environment proxy
- **GIVEN** `HTTPS_PROXY=http://127.0.0.1:3128`
- **WHEN** an `https` request is sent
- **THEN** it SHALL tunnel through the proxy via `CONNECT`

#### Scenario: NO_PROXY honored
- **GIVEN** `HTTPS_PROXY` is set and `NO_PROXY=internal.local`
- **WHEN** `https://api.internal.local` is requested
- **THEN** the connection SHALL be direct

### Requirement: Cookie jar per environment
Each environment SHALL have its own persistent cookie jar in `.rustman/cookies/<env>.json`, following RFC 6265 domain, path, expiry and `Secure` rules. Cookies SHALL persist across runs. `settings.cookies: false` disables the jar for one request; `rustman cookies clear [--env e]` empties a jar.

#### Scenario: Login then authenticated call
- **GIVEN** `POST /login` responds with `Set-Cookie: sid=abc; Path=/`
- **WHEN** `GET /me` is sent next with the same environment
- **THEN** it SHALL include `Cookie: sid=abc`

#### Scenario: Jars isolated
- **GIVEN** a cookie stored under `staging`
- **WHEN** a request is sent with `--env prod`
- **THEN** that cookie SHALL NOT be sent

### Requirement: Basic, bearer and API-key auth
`auth` SHALL support `{type: basic, username, password}`, `{type: bearer, token}` and `{type: api_key, name, value, in: header|query}`. Auth SHALL be applied after interpolation, and SHALL NOT override an explicit `Authorization` header in the request.

#### Scenario: Basic auth
- **GIVEN** `auth: {type: basic, username: ana, password: pw}`
- **WHEN** sent
- **THEN** the request SHALL carry `Authorization: Basic YW5hOnB3`

#### Scenario: API key in query
- **GIVEN** `auth: {type: api_key, name: key, value: "{{k}}", in: query}` and `k=123`
- **WHEN** sent
- **THEN** the URL SHALL include `key=123`

### Requirement: OAuth2 auth
`auth: {type: oauth2}` SHALL support the `client_credentials` grant, and the `authorization_code` grant with PKCE (S256), using a loopback redirect on `127.0.0.1` and an ephemeral port. Tokens SHALL be cached per environment in the secret store, refreshed with a refresh token when available, and re-acquired when they expire or a request gets a `401`.

#### Scenario: Client credentials cached
- **GIVEN** a token endpoint returning `expires_in: 3600`
- **WHEN** two requests run 10 s apart
- **THEN** the token endpoint SHALL be called exactly once

#### Scenario: Auth code with PKCE
- **GIVEN** an `authorization_code` config and an interactive terminal
- **WHEN** the first request runs
- **THEN** the system SHALL open the browser (or print the URL) with a `code_challenge_method=S256` parameter, receive the code on the loopback listener, and exchange it

#### Scenario: Auth code in non-interactive mode
- **GIVEN** no cached token and stdin is not a TTY
- **WHEN** the request runs
- **THEN** it SHALL fail with `oauth2: interactive login required (run 'rustman auth login <request>')`

### Requirement: Timing breakdown
Each exchange SHALL record durations in milliseconds, with microsecond precision, for `dns`, `connect`, `tls`, `ttfb` (request written → first response byte), `download` and `total`. Phases that don't apply (a reused connection, plain HTTP) SHALL be `null`, not zero.

#### Scenario: Fresh HTTPS connection
- **WHEN** an HTTPS request opens a new connection
- **THEN** `dns`, `connect`, `tls`, `ttfb`, `download` and `total` SHALL all be non-null
- **AND** `total` SHALL be greater than or equal to their sum

#### Scenario: Plain HTTP
- **WHEN** an `http://` request is sent
- **THEN** `tls` SHALL be `null`

### Requirement: Response body handling
Response bodies up to `max_body_bytes` (default 10 MiB) SHALL be kept in memory. Larger bodies SHALL be streamed to `.rustman/bodies/<exchange-id>.bin`, with the exchange keeping a 64 KiB preview and the full size. A spill file SHALL be removed when its body is not read to the end, and redirect hops SHALL keep none. Bodies that aren't valid UTF-8 SHALL be marked `binary: true` and never decoded lossily.

#### Scenario: Large download spilled
- **GIVEN** a 50 MiB response
- **WHEN** it is received with defaults
- **THEN** process memory used for the body SHALL stay under 16 MiB
- **AND** the exchange SHALL reference the spill file and record `size_bytes: 52428800`

#### Scenario: Binary image
- **GIVEN** a `image/png` response
- **WHEN** it is received
- **THEN** the exchange SHALL record `binary: true`, and the raw bytes SHALL be preserved exactly

### Requirement: Response decompression
Bodies SHALL be decompressed (gzip, br, deflate, zstd) unless `settings.decompress: false`. Decoding SHALL use bounded memory whatever the compression ratio, and a body that decodes to more than 1 GiB SHALL fail with `network: the decompressed response body is larger than <n> bytes`.

#### Scenario: Compressed response decoded
- **GIVEN** a response with `Content-Encoding: gzip`
- **WHEN** it is received with defaults
- **THEN** the exchange SHALL hold the decoded body

#### Scenario: Decompression bomb
- **GIVEN** a compressed response that decodes to more than the limit
- **WHEN** it is received
- **THEN** the result SHALL be the decompressed-size error

### Requirement: Default headers
The engine SHALL send `User-Agent: rustman/<crate version>` and `Accept: */*`, unless the request sets them. Body types SHALL set a `Content-Type` default (`application/json`, `text/plain; charset=utf-8`, `application/x-www-form-urlencoded`, `multipart/form-data; boundary=…`, `application/octet-stream`) that an explicit header overrides. `Content-Length` and `Host` SHALL always be computed by the engine.

#### Scenario: Explicit content type wins
- **GIVEN** a JSON body and header `Content-Type: application/vnd.api+json`
- **WHEN** sent
- **THEN** exactly one `Content-Type: application/vnd.api+json` SHALL be sent

#### Scenario: Stale Content-Length ignored
- **GIVEN** a request header `Content-Length: 3` and a 10-byte body
- **WHEN** sent
- **THEN** the request SHALL carry `Content-Length: 10` and a warning SHALL be recorded

### Requirement: Curl import
`rustman import curl [--out <file>]` SHALL read a curl command from an argument or stdin and write an equivalent request file. It SHALL support `-X`, `-H`, `-d/--data*`, `--data-urlencode`, `-F`, `-u`, `-b`, `-k`, `-L`, `--compressed` and `--url`. Unsupported flags SHALL be listed as warnings, not silently dropped.

#### Scenario: Import POST JSON
- **WHEN** the user imports `curl -X POST https://x.io/a -H 'Content-Type: application/json' -d '{"a":1}'`
- **THEN** the written file SHALL have `method: POST`, `url: https://x.io/a` and `body: {type: json, content: {a: 1}}`

#### Scenario: Unsupported flag
- **WHEN** the command includes `--limit-rate 1k`
- **THEN** the import SHALL succeed and print `warning: ignored unsupported curl option --limit-rate`

### Requirement: Copy as curl
`rustman export curl <request>` SHALL print a single, POSIX-shell-quoted curl command that reproduces the resolved request. Secrets SHALL be redacted unless `--show-secrets` is passed.

#### Scenario: Export with secret
- **GIVEN** a request using bearer token `{{api_token}}` (a secret)
- **WHEN** exported without `--show-secrets`
- **THEN** the output SHALL contain `-H 'Authorization: «redacted:api_token»'`

#### Scenario: Round trip
- **GIVEN** any request without secrets
- **WHEN** exported to curl and then imported back
- **THEN** the method, URL, headers and body SHALL be equivalent
