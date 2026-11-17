## ADDED Requirements

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
Nonce and nonce count SHALL be cached per environment, realm and host, so later requests send credentials without a challenge. A `stale=true` challenge SHALL trigger one retry with the new nonce. `qop=auth-int` and `userhash` SHALL fail with `digest: unsupported qop auth-int`.

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
`auth: {type: aws_sigv4, region, service}` SHALL sign requests with AWS Signature Version 4, adding `Authorization`, `X-Amz-Date`, `X-Amz-Content-Sha256` and, with a session token, `X-Amz-Security-Token`. Signing SHALL be the last step before sending, and every redirect hop SHALL be re-signed.

#### Scenario: Signed S3 GET
- **GIVEN** `auth: {type: aws_sigv4, region: us-east-1, service: s3, access_key: "{{aws_key}}", secret_key: "{{aws_secret}}"}`
- **WHEN** `GET https://bucket.s3.amazonaws.com/a.txt` is sent
- **THEN** the request SHALL carry `Authorization: AWS4-HMAC-SHA256 Credential=<key>/<date>/us-east-1/s3/aws4_request, SignedHeaders=..., Signature=...`

#### Scenario: AWS test suite
- **GIVEN** the AWS SigV4 test suite cases (`get-vanilla`, `post-x-www-form-urlencoded`, `get-utf8` and the rest)
- **WHEN** each is signed with the suite's fixed credentials and date
- **THEN** the canonical request, string to sign and signature SHALL match the suite exactly

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
