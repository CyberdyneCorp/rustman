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
When a redirect hop changes origin (scheme, host or port), every credential the request carries (`Authorization`, `Proxy-Authorization`, an explicit `Cookie` header, the `api_key` header or query parameter) SHALL be dropped, and so SHALL every other header, and a `307`/`308` body, that carries one of the request's secrets. The hop SHALL still be sent, with a warning naming what was dropped. A same-origin hop SHALL keep them.

#### Scenario: Cross-host auth stripping
- **GIVEN** `https://api.example.com/x` redirects to `https://cdn.other.com/x`
- **WHEN** the request carries bearer auth
- **THEN** the second hop SHALL NOT include `Authorization`

#### Scenario: Cross-origin API key stripping
- **GIVEN** `http://localhost:8080/x` redirects to `http://127.0.0.1:8080/x`
- **WHEN** the request carries an `api_key` query parameter
- **THEN** the second hop SHALL NOT include that parameter

#### Scenario: Secret in a custom header and a 307 body
- **GIVEN** `http://localhost:8080/x` answers `307` to `http://127.0.0.1:8080/y`
- **WHEN** the request is a `POST` with `X-Api-Key: '{{token}}'` and a form body with `client_secret={{token}}`, where `token` is a secret
- **THEN** the second hop SHALL be a `POST` without `X-Api-Key` and with an empty body
- **AND** the exchange SHALL warn `cross-origin redirect to http://127.0.0.1:8080: dropped header x-api-key, the body, which carry a secret`

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

### Requirement: Token endpoint redirects
Calls to an OAuth2 `token_url` SHALL NOT follow redirects, so the form with the client secret, code or refresh token is never sent to another URL.

#### Scenario: Token URL redirects
- **GIVEN** `token_url` answers `307` to another URL
- **WHEN** a token is requested
- **THEN** the request SHALL fail with an `oauth2:` error
- **AND** the other URL SHALL receive no request

### Requirement: Undecoded body limit
A response body that is not decoded (no supported `Content-Encoding`, or `settings.decompress: false`) SHALL fail with `network: the response body is larger than <n> bytes` once it exceeds `rustman.toml [http] max_download_bytes` (default 1 GiB), and its spill file SHALL be removed.

#### Scenario: Endless stream
- **GIVEN** a server that streams a body without end and `max_download_bytes = 2097152`
- **WHEN** the request is sent
- **THEN** the result SHALL be `network: the response body is larger than 2097152 bytes`
- **AND** no spill file SHALL remain

### Requirement: Private spill folder
Outside a workspace, spilled bodies SHALL go to a folder of the current user in the temp folder (`rustman-bodies-<uid>` on Unix). A spill folder that is a symbolic link, or on Unix belongs to another user, SHALL be refused, and a spill file SHALL always be created new, never opened when its path already exists.

#### Scenario: Symbolic link in place of the spill folder
- **GIVEN** the spill folder is a symbolic link to another folder
- **WHEN** a body larger than `max_body_bytes` is received
- **THEN** the request SHALL fail with an error naming the symbolic link
- **AND** nothing SHALL be written to the link's target

### Requirement: Loopback listener limits
The OAuth2 loopback listener SHALL serve at most 16 connections at once, and SHALL wait before retrying a failed `accept`, starting at 10 ms and doubling up to 1 s, instead of retrying at once.

#### Scenario: Accept keeps failing
- **GIVEN** every `accept` on the loopback listener fails
- **WHEN** the login waits 500 ms
- **THEN** `accept` SHALL have been called at most 10 times

### Requirement: Token storage notice
When `rustman auth login` stores the token in `.rustman/secrets.yaml` instead of the OS keychain, it SHALL print, once, `warning: OAuth2 tokens are stored in .rustman/secrets.yaml (git-ignored, readable by you only), not in the OS keychain`.

#### Scenario: File store
- **GIVEN** `RUSTMAN_SECRET_STORE=file`
- **WHEN** `rustman auth login <request>` succeeds
- **THEN** stderr SHALL be exactly that warning line

### Requirement: Digest auth
`auth: {type: digest, username, password}` SHALL implement RFC 7616 with `MD5`, `MD5-sess`, `SHA-256`, `SHA-256-sess` and `qop=auth`. On a `401` carrying `WWW-Authenticate: Digest`, the system SHALL compute the response and resend once. A second `401` SHALL be returned as a normal response.

#### Scenario: Challenge and retry
- **GIVEN** a server that answers the first request with `401` and `WWW-Authenticate: Digest realm="api", qop="auth", algorithm=SHA-256, nonce="abc"`
- **WHEN** a request with digest auth is sent
- **THEN** the system SHALL resend once with an `Authorization: Digest` header using `algorithm=SHA-256`, `qop=auth`, `nc=00000001` and a fresh `cnonce`
- **AND** the exchange SHALL record both round trips in its timing breakdown

#### Scenario: RFC test vector
- **GIVEN** the RFC 7616 section 3.9.1 inputs (user `Mufasa`, password `Circle of Life`, SHA-256)
- **WHEN** the response value is computed
- **THEN** it SHALL equal the RFC's published `response` value

#### Scenario: Wrong password
- **GIVEN** incorrect credentials
- **WHEN** the retry also gets `401`
- **THEN** the exchange SHALL complete with status `401` and no further retries

### Requirement: Digest nonce reuse
Nonce and nonce count SHALL be cached per environment, realm and host, so later requests send credentials without a challenge. A `stale=true` challenge SHALL trigger one retry with the new nonce. A challenge offering only `qop=auth-int` SHALL fail with `digest: unsupported qop auth-int`, and one with `userhash=true` with `digest: unsupported userhash`.

#### Scenario: Preemptive second request
- **GIVEN** a successful digest exchange with host `api.example` in `staging`
- **WHEN** a second request goes to the same host and realm
- **THEN** it SHALL carry `Authorization: Digest ... nc=00000002` on the first attempt

#### Scenario: Stale nonce
- **GIVEN** a cached nonce, and the server answers `401` with `stale=true` and a new nonce
- **WHEN** the request runs
- **THEN** the system SHALL retry once with the new nonce and `nc=00000001`

### Requirement: Digest replayable body
Because a digest challenge requires resending, a `file` or multipart body SHALL be buffered up to 10 MiB. A larger body SHALL fail before sending, with `digest: body larger than 10 MiB cannot be replayed`.

#### Scenario: Large upload refused
- **GIVEN** digest auth and `body: {type: file, path: big.bin}` of 50 MiB
- **WHEN** the request runs
- **THEN** it SHALL fail as a request error (exit code 3) with that message, and no bytes SHALL be sent

### Requirement: AWS SigV4 auth
`auth: {type: aws_sigv4, region, service}` SHALL sign requests with AWS Signature Version 4, adding `Authorization`, `X-Amz-Date`, `X-Amz-Content-Sha256` and, with a session token, `X-Amz-Security-Token`. Signing SHALL be the last step before sending. Same-origin redirect hops SHALL be re-signed; from the first hop that leaves the origin on, no hop SHALL be signed.

#### Scenario: Signed S3 GET
- **GIVEN** `auth: {type: aws_sigv4, region: us-east-1, service: s3, access_key: "{{aws_key}}", secret_key: "{{aws_secret}}"}`
- **WHEN** `GET https://bucket.s3.amazonaws.com/a.txt` is sent
- **THEN** the request SHALL carry `Authorization: AWS4-HMAC-SHA256 Credential=<key>/<date>/us-east-1/s3/aws4_request, SignedHeaders=..., Signature=...`

#### Scenario: AWS test suite
- **GIVEN** the AWS SigV4 test suite cases (`get-vanilla`, `post-x-www-form-urlencoded`, `get-utf8` and the rest)
- **WHEN** each is signed with the suite's fixed credentials and date
- **THEN** the canonical request, string to sign and signature SHALL match the suite exactly

#### Scenario: Same-origin redirect
- **GIVEN** a SigV4 request to `/a` that redirects with `302` to `/b` on the same origin
- **WHEN** the redirect is followed
- **THEN** the request to `/b` SHALL carry a new signature, valid for `/b`

#### Scenario: Cross-origin redirect
- **GIVEN** a SigV4 request that redirects to another host or port
- **WHEN** the redirect is followed
- **THEN** the request to the other origin SHALL carry no `Authorization`, no `X-Amz-*` header, no access key and no session token
- **AND** a later redirect back to the original origin SHALL be unsigned too

#### Scenario: Default headers are signed
- **GIVEN** workspace default header `X-Trace: 1`
- **WHEN** a SigV4 request is sent
- **THEN** `x-trace` SHALL appear in `SignedHeaders`

### Requirement: AWS SigV4 credential sources
SigV4 credentials SHALL come from explicit `access_key`, `secret_key` and optional `session_token` fields, from `from: env` (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`), or from `from: profile` with `profile: <name>` (static keys in `~/.aws/credentials`). SSO, assume-role and `credential_process` SHALL fail with a message naming the unsupported key.

#### Scenario: Credentials from environment
- **GIVEN** `auth: {type: aws_sigv4, region: eu-west-1, service: execute-api, from: env}` and the three variables set in the shell
- **WHEN** the request is sent
- **THEN** it SHALL be signed with those keys and carry `X-Amz-Security-Token`

#### Scenario: SSO profile
- **GIVEN** `from: profile`, `profile: dev`, and a profile that only defines `sso_start_url`
- **WHEN** the request runs
- **THEN** it SHALL fail with `aws_sigv4: profile 'dev' uses sso_start_url, which is not supported; export temporary credentials or use a secret provider`

### Requirement: AWS SigV4 payload options
The payload hash SHALL be computed over the exact bytes sent. `unsigned_payload: true` SHALL send `UNSIGNED-PAYLOAD` instead, which suits large S3 uploads. A clock-skew rejection (`RequestTimeTooSkewed` or `Signature expired`) SHALL add the hint `check the system clock; request was signed at <X-Amz-Date>`.

#### Scenario: Unsigned payload
- **GIVEN** `unsigned_payload: true` and a 200 MiB file body
- **WHEN** the request is sent
- **THEN** `X-Amz-Content-Sha256` SHALL be `UNSIGNED-PAYLOAD` and the body SHALL stream without being buffered

### Requirement: JWT bearer auth
`auth: {type: jwt, algorithm, claims}` SHALL sign a new token per request with `HS256`, `HS384`, `HS512`, `RS256` or `ES256`. It SHALL add `iat` and `exp = iat + ttl` (default `5m`) unless `claims` sets them. `header` MAY add JOSE header fields such as `kid`.

#### Scenario: HS256 token per request
- **GIVEN** `auth: {type: jwt, algorithm: HS256, secret: "{{jwt_secret}}", claims: {sub: "{{user_id}}", scope: read}}`
- **WHEN** two requests are sent 2 seconds apart
- **THEN** each SHALL carry a different `Authorization: Bearer <jwt>` whose decoded payload has `sub`, `scope`, `iat` and `exp = iat + 300`

#### Scenario: Interpolated claims
- **GIVEN** `user_id=42`
- **WHEN** the token is generated
- **THEN** its `sub` claim SHALL be `"42"`

### Requirement: JWT key material and placement
HMAC secrets SHALL come from a variable. RSA and EC keys SHALL come from `private_key: <variable>` or `private_key_file: <path>` (PEM), and the file path SHALL NOT escape the workspace. `in: header` (default `Authorization: Bearer`), `in: header, name: <h>` or `in: query, name: <p>` SHALL set where the token goes. Generated tokens SHALL be redacted as `«redacted:jwt»`.

#### Scenario: Token redacted in history
- **GIVEN** a JWT-authenticated request
- **WHEN** the exchange is written to history
- **THEN** the stored `Authorization` header SHALL be `Bearer «redacted:jwt»`

#### Scenario: Algorithm and key mismatch
- **GIVEN** `algorithm: RS256` with an EC private key file
- **WHEN** the workspace loads
- **THEN** loading SHALL fail with `jwt: RS256 requires an RSA private key`

### Requirement: Snippet export
`rustman export snippet --lang <lang> <request>` SHALL print code that reproduces the resolved request. Supported languages SHALL be `curl`, `httpie`, `wget`, `python-requests`, `python-httpx`, `js-fetch`, `node-axios`, `go`, `rust-reqwest`, `powershell` and `csharp`. `rustman export snippet --list` SHALL list them.

#### Scenario: Python requests snippet
- **GIVEN** `POST {{base_url}}/users` with a JSON body and `base_url=https://api.example`
- **WHEN** the user runs `rustman export snippet --lang python-requests api/users/create`
- **THEN** the output SHALL call `requests.post("https://api.example/users", json={...}, headers={...})`

#### Scenario: Unknown language
- **WHEN** the user passes `--lang cobol`
- **THEN** the command SHALL exit with code 2 and list the supported languages

### Requirement: Snippet fidelity and redaction
Snippets SHALL use the same resolved request as `export curl`. Secrets SHALL be redacted unless `--show-secrets` is passed. Multipart parts SHALL reference the files by workspace-relative path. `graphql` requests SHALL export as HTTP POST snippets. Other kinds SHALL exit 2 with `snippet: kind '<kind>' has no snippet export`.

#### Scenario: Secret redacted in snippet
- **GIVEN** a bearer token from secret `api_token`
- **WHEN** exported as `js-fetch` without `--show-secrets`
- **THEN** the snippet SHALL contain `"Authorization": "Bearer «redacted:api_token»"`

#### Scenario: Snippet runs as printed
- **GIVEN** a request without secrets against a local echo server
- **WHEN** each generated snippet runs with its interpreter
- **THEN** the echo server SHALL receive the same method, path, query, headers and body for every language

### Requirement: Snippets for signature auth
For `digest`, `aws_sigv4` and `jwt`, snippets SHALL use the language's native helper where one exists: `HTTPDigestAuth` in python-requests, `--auth-type digest` in httpie, `--digest` in curl, `--aws-sigv4` in curl. Otherwise they SHALL include a comment naming the auth type, and the CLI SHALL warn `snippet: <auth> auth cannot be precomputed for <lang>`.

#### Scenario: Digest in curl
- **GIVEN** a digest-auth request
- **WHEN** exported as `curl`
- **THEN** the command SHALL include `--digest -u '<user>:«redacted:password»'`

#### Scenario: SigV4 in Go
- **GIVEN** an `aws_sigv4` request
- **WHEN** exported as `go`
- **THEN** the snippet SHALL contain a comment `// aws_sigv4 auth: sign with the AWS SDK signer` and stderr SHALL contain the warning

### Requirement: Snippet placeholder mode
`--placeholders` SHALL keep variables unresolved: `{{name}}` in curl, httpie and wget, and a language constant (such as `BASE_URL`) declared at the top of code snippets. Secrets SHALL always render as `«redacted:name»` in this mode. The aliases `fetch`, `python` and `rust` SHALL map to `js-fetch`, `python-requests` and `rust-reqwest`.

#### Scenario: Placeholders in Python
- **GIVEN** `GET {{base_url}}/users/{{id}}`
- **WHEN** exported with `--lang python --placeholders`
- **THEN** the snippet SHALL declare `BASE_URL` and `ID` constants and build the URL from them

#### Scenario: Shared with docs
- **GIVEN** `api-docs-generation` renders the code samples for an endpoint page
- **WHEN** it builds the samples
- **THEN** it SHALL use this snippet engine in placeholder mode, so `rustman export snippet --placeholders` and the docs page show identical code
