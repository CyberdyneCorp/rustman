# mock-server Specification

## Purpose
Serves an OpenAPI document, plus hand-written overrides, as a local, validating and optionally stateful HTTP mock with fault injection, hot reload and request logging, so clients can be built and failure paths tested before or without the real backend.

## Requirements

### Requirement: Serve command and bind address
`rustman mock serve <openapi>` SHALL serve the document over HTTP/1.1 and h2c on `127.0.0.1:4010` by default, configurable with `--host`/`--port` or `[mock]` in `rustman.toml`. Port `0` SHALL pick a free port and print it. On start it SHALL print the listen URL and the number of operations served.

#### Scenario: Default start
- **GIVEN** `petstore.yaml` with 19 operations
- **WHEN** the user runs `rustman mock serve petstore.yaml`
- **THEN** it SHALL print `mock: serving 19 operations on http://127.0.0.1:4010`

#### Scenario: Port in use
- **GIVEN** port 4010 is taken
- **WHEN** the server starts
- **THEN** it SHALL exit with code 2 and `mock: cannot bind 127.0.0.1:4010: address in use`

### Requirement: Loopback by default
Binding a non-loopback address SHALL require `--allow-remote` on the CLI and confirmation in the TUI. Without it the server SHALL refuse to start with exit code 2.

#### Scenario: Remote bind refused
- **WHEN** the user runs `rustman mock serve api.yaml --host 0.0.0.0`
- **THEN** it SHALL exit with code 2 and `mock: binding 0.0.0.0 exposes the mock to the network; pass --allow-remote`

### Requirement: Document loading
The document SHALL be loaded with the `openapi-import` parser, versions and `$ref` rules (remote refs off unless `--allow-remote-refs`). Structural errors SHALL abort start-up with exit code 2 and diagnostics with JSON pointer locations.

#### Scenario: Invalid document
- **GIVEN** a document with a dangling `$ref`
- **WHEN** the server starts
- **THEN** it SHALL exit with code 2 and print the pointer of the broken reference

### Requirement: Route matching
Each OpenAPI path and method SHALL become a route, with the document's server base path stripped. Path templates SHALL match one segment per parameter. When several routes match, the one with more literal segments SHALL win, then the earlier path in the document. Unknown paths SHALL return `404`, and known paths with an undeclared method SHALL return `405` with an `Allow` header.

#### Scenario: Static beats templated
- **GIVEN** paths `/users/me` and `/users/{id}`
- **WHEN** a client calls `GET /users/me`
- **THEN** the `/users/me` operation SHALL handle it

#### Scenario: Method not allowed
- **GIVEN** `/users` declares only `get` and `post`
- **WHEN** a client calls `DELETE /users`
- **THEN** the response SHALL be `405` with `Allow: GET, POST`

### Requirement: Problem details for mock errors
Every error the mock generates itself (404, 405, validation, security, faults with a status) SHALL use `Content-Type: application/problem+json` with `type`, `title`, `status` and `detail`, and validation errors SHALL add an `errors` array of `{location, pointer, message}`.

#### Scenario: Unknown route body
- **WHEN** a client calls `GET /nope`
- **THEN** the body SHALL be problem+json with `status: 404` and `title: "No matching operation"`

### Requirement: Response selection order
For a matched operation, the response SHALL be chosen in this order: a matching hand-written override; with `--stateful`, the stateful store for an operation on a resource (so `Prefer` does not apply to it); the `Prefer` header; the lowest documented `2xx` (or `default` if no 2xx exists). Within the chosen status and the negotiated media type, the body SHALL come from the first named example, else the schema `example`, else schema synthesis.

#### Scenario: Default success response
- **GIVEN** `GET /pets/{id}` documents `200` with examples `cat` and `dog`
- **WHEN** a client calls `GET /pets/1` with no `Prefer` header
- **THEN** the response SHALL be `200` with the `cat` example

### Requirement: Prefer header
The `Prefer` header SHALL support `code=<status>`, `example=<name>` and `dynamic=true` (force schema synthesis), compatible with Prism. A `code` the operation doesn't document SHALL return `400` problem+json listing documented codes. `--no-prefer` SHALL disable this.

#### Scenario: Ask for a 404 example
- **GIVEN** `GET /pets/{id}` documents `404` with example `notFound`
- **WHEN** a client sends `Prefer: code=404, example=notFound`
- **THEN** the response SHALL be `404` with the `notFound` example body

#### Scenario: Undocumented code
- **WHEN** a client sends `Prefer: code=418` to that operation
- **THEN** the response SHALL be `400` and `detail` SHALL list `200, 404`

### Requirement: Content negotiation
The response media type SHALL be negotiated from `Accept` against the documented media types. With no `Accept` or `*/*`, the first JSON media type SHALL be used. When nothing acceptable exists, the response SHALL be `406`.

#### Scenario: XML requested but not documented
- **GIVEN** an operation documenting only `application/json`
- **WHEN** a client sends `Accept: application/xml`
- **THEN** the response SHALL be `406`

### Requirement: Deterministic schema synthesis
Synthesized bodies SHALL satisfy the schema's types, required properties, formats, enums, bounds and patterns, using the same synthesizer as `openapi-import`. Output SHALL be deterministic for the same `--seed` (default `0`), operation and request path, so repeated calls return the same body.

#### Scenario: Stable synthesized data
- **GIVEN** an operation whose 200 schema has no examples
- **WHEN** a client calls it twice with the same path
- **THEN** both bodies SHALL be identical and SHALL validate against the schema

#### Scenario: Different seed
- **WHEN** the server is restarted with `--seed 7`
- **THEN** the synthesized body SHALL differ from the seed-0 body and still validate

### Requirement: Request validation
By default each request SHALL be validated against its operation: required and typed path, query, header and cookie parameters → `400`; a body whose media type is not documented → `415`; a body that fails its schema → `422`. Each failure SHALL be returned as problem+json with all violations, up to 20. `--no-validate` SHALL skip validation.

#### Scenario: Missing required query parameter
- **GIVEN** `GET /search` requires query `q`
- **WHEN** a client calls `GET /search`
- **THEN** the response SHALL be `400` with an error `{location: query, pointer: "/q", message: "required"}`

#### Scenario: Schema violation
- **GIVEN** `POST /pets` requires `name` of type string
- **WHEN** a client posts `{"name": 5}`
- **THEN** the response SHALL be `422` with an error at pointer `/name`

#### Scenario: Unsupported media type
- **WHEN** a client posts `text/plain` to an operation documenting only `application/json`
- **THEN** the response SHALL be `415`

### Requirement: Optional security enforcement
With `--enforce-security`, a request missing the credentials required by the operation's effective security requirement (API key in header, query or cookie; HTTP bearer or basic; OAuth2/OIDC as bearer) SHALL get `401` with a `WWW-Authenticate` header. Credential values SHALL NOT be checked unless `[mock.security]` lists expected values as `environments` secrets.

#### Scenario: Missing bearer token
- **GIVEN** an operation secured by HTTP bearer and `--enforce-security`
- **WHEN** a client calls it without `Authorization`
- **THEN** the response SHALL be `401` with `WWW-Authenticate: Bearer`

#### Scenario: Not enforced by default
- **WHEN** the same call is made without `--enforce-security`
- **THEN** the documented success response SHALL be returned

### Requirement: Hand-written mock files
Files `mocks/*.rman-mock.yaml` (directory configurable as `[mock] dir`) SHALL define overrides: `match` (method, path template or glob, query, headers, body JSONPath conditions) and `respond` (status, headers, `body` or `body_file`). Overrides SHALL be evaluated in file-name then declaration order; the first match wins. Overrides MAY target paths absent from the document.

#### Scenario: Override by body field
- **GIVEN** an override matching `POST /payments` where `$.amount` `gt: 10000`, responding `402`
- **WHEN** a client posts `{"amount": 50000}`
- **THEN** the response SHALL be `402`

#### Scenario: Override for undocumented path
- **GIVEN** an override for `GET /health` responding `200 {"ok":true}`
- **WHEN** a client calls `GET /health`
- **THEN** the override SHALL respond even though the document has no `/health`

### Requirement: Response templates
Override bodies and header values SHALL support templates using request data: `{{request.path.<name>}}`, `{{request.query.<name>}}`, `{{request.header.<name>}}`, `{{request.body.<JSONPath>}}`, plus the `environments` dynamic variables (`{{$uuid}}`, `{{$timestamp}}`, `{{$randomInt}}`). Unresolved request references SHALL render as an empty string and be logged.

#### Scenario: Echo a path parameter
- **GIVEN** an override for `GET /users/{id}` with body `{"id": "{{request.path.id}}"}`
- **WHEN** a client calls `GET /users/42`
- **THEN** the body SHALL be `{"id": "42"}`

### Requirement: Latency simulation
An override, or `[mock] delay_ms`, SHALL support a fixed `delay_ms` and a `jitter_ms` (uniform ± range), capped at 60000 ms. The delay SHALL be applied before the response headers are sent.

#### Scenario: Delayed response
- **GIVEN** an override with `delay_ms: 2000`
- **WHEN** a client calls it
- **THEN** the response headers SHALL arrive no earlier than 2000 ms after the request

### Requirement: Fault injection
An override SHALL support `fault` with `error_rate` (0.0–1.0, returns `fault.status`, default `500`), `drop_connection: true` (closes the connection without a response), and `slow_body_bps` (streams the body at that many bytes per second). Fault randomness SHALL use the server seed, so a given seed and request sequence reproduces the same faults.

#### Scenario: Error rate
- **GIVEN** an override with `fault: {error_rate: 1.0, status: 503}`
- **WHEN** a client calls the route
- **THEN** the response SHALL be `503` problem+json

#### Scenario: Dropped connection
- **GIVEN** an override with `fault: {drop_connection: true}`
- **WHEN** a client calls the route
- **THEN** the connection SHALL close without any response bytes

### Requirement: Stateful mode
With `--stateful`, resources inferred by the `api-exploration` dependency discovery SHALL be backed by an in-memory store. `POST` to a collection SHALL store the request body plus a generated id, returning the documented creation status. `GET`, `PUT`/`PATCH` and `DELETE` on the item path SHALL read, update and remove it. Unknown ids SHALL get `404`.

#### Scenario: Create then read
- **GIVEN** `--stateful` and operations `POST /users` and `GET /users/{id}`
- **WHEN** a client posts `{"name":"Ana"}` and then calls `GET /users/<returned id>`
- **THEN** the second response SHALL contain `"name": "Ana"` and the same id

#### Scenario: Read after delete
- **WHEN** the client deletes that user and calls `GET` again
- **THEN** the response SHALL be `404`

### Requirement: Stateful reset and limits
`POST /__rustman/reset` SHALL clear the stateful store and return `204`. Paths under `/__rustman/` SHALL be reserved and never routed to the document. The store SHALL hold at most 10000 items per resource and at most 64 MiB of JSON in all (both configurable); a write past either limit SHALL return `507` problem+json.

#### Scenario: Reset
- **GIVEN** a store with 5 users
- **WHEN** a client calls `POST /__rustman/reset`
- **THEN** the response SHALL be `204` and `GET /users/<old id>` SHALL return `404`

### Requirement: CORS
CORS SHALL be enabled by default when bound to loopback and disabled otherwise, overridable with `--cors` / `--no-cors`. When enabled, preflight `OPTIONS` requests SHALL get `204` with `Access-Control-Allow-Origin` echoing the request origin, the operation's methods and the requested headers, before validation runs.

#### Scenario: Browser preflight
- **GIVEN** the default loopback bind
- **WHEN** a browser sends `OPTIONS /pets` with `Origin: http://localhost:5173` and `Access-Control-Request-Method: POST`
- **THEN** the response SHALL be `204` with `Access-Control-Allow-Origin: http://localhost:5173` and `POST` in `Access-Control-Allow-Methods`

### Requirement: Request limits
The server SHALL enforce the same defaults as `webhook-inspector`: request bodies over 10 MiB → `413`, more than 100 headers or 64 KiB of header bytes → `431`, and malformed requests → `400` from `hyper`. These limits SHALL be configurable under `[mock]`.

#### Scenario: Oversized body
- **WHEN** a client posts 20 MiB to any route
- **THEN** the response SHALL be `413`

### Requirement: Request log
Each request SHALL produce a log entry with time, method, path, matched operation id or override, response status, response source (`override`, `prefer`, `example`, `synthesized`, `stateful`, `proxy`, `error`), validation errors and duration. `--jsonl` SHALL print entries as JSON lines on stdout; otherwise a one-line human format SHALL go to stderr. Secret header values SHALL be redacted.

#### Scenario: JSON lines
- **WHEN** the server runs with `--jsonl` and a client calls `GET /pets/1`
- **THEN** stdout SHALL get one JSON object with `"status": 200` and `"source": "example"`

#### Scenario: Authorization redacted
- **WHEN** a client sends `Authorization: Bearer abc`
- **THEN** the log entry SHALL show `«redacted:Authorization»`

#### Scenario: API key redacted
- **GIVEN** the document declares an `apiKey` scheme in the `X-Api-Key` header
- **WHEN** a client sends `X-Api-Key: canary-apikey-123456` and the query `?api_key=qcanary99`
- **THEN** the log entry and history entry SHALL show `«redacted:x-api-key»` and `api_key=«redacted:api_key»` and neither value

### Requirement: History integration
With `--history`, each served exchange SHALL be appended to `request-history` under the `mock` source, after redaction, subject to the normal retention limits.

#### Scenario: Exchanges in history
- **GIVEN** the server ran with `--history` and served 3 requests
- **WHEN** the user runs `rustman history list`
- **THEN** the 3 exchanges SHALL appear tagged `mock`

### Requirement: Hot reload
The server SHALL watch the document and mock files and reload within 1 s of a change, without dropping in-flight requests or the stateful store. If the new version fails to parse, the server SHALL keep serving the previous version and log the error with its location.

#### Scenario: Add an operation
- **GIVEN** a running mock
- **WHEN** the user adds `GET /orders` to the document and saves
- **THEN** within 1 s `GET /orders` SHALL be served

#### Scenario: Broken edit
- **WHEN** the user saves the document with invalid YAML
- **THEN** existing routes SHALL keep working and an error with file and line SHALL be logged

### Requirement: Proxy unmatched routes
With `--proxy-unmatched <url>`, requests matching no operation or override SHALL be forwarded to that upstream with transport headers stripped (as `webhook-inspector` replay does), and the upstream response returned unchanged. Only that upstream's origin SHALL be contacted, and the request log SHALL mark the source `proxy`.

#### Scenario: Forward unknown route
- **GIVEN** `--proxy-unmatched https://staging.example.com`
- **WHEN** a client calls `GET /legacy/report`, which the document lacks
- **THEN** the request SHALL be forwarded to `https://staging.example.com/legacy/report` and its response returned

### Requirement: Record proxied exchanges
With `--record <dir>` (requires `--proxy-unmatched`), each proxied exchange SHALL be saved as a `kind: http` request file in `<dir>` inside the workspace, named and deduplicated like `collection-import` HAR imports, with secrets extracted.

#### Scenario: Recorded request file
- **GIVEN** `--proxy-unmatched https://staging.example.com --record recorded/`
- **WHEN** a client calls `GET /legacy/report` with a bearer token
- **THEN** `recorded/get-legacy-report.rman.yaml` SHALL exist with the token as a `{{placeholder}}`

### Requirement: Mock serves the explorer self-check
The mock SHALL be usable as an `api-exploration` target: `rustman explore run --spec api.yaml --target http://127.0.0.1:<port>` against a mock of the same document with validation on SHALL produce no conformance findings for documented responses.

#### Scenario: Consistent document
- **GIVEN** a valid document and its mock running with default options
- **WHEN** an exploration with only conformance probes runs against the mock
- **THEN** it SHALL report zero conformance findings and exit with code 0

### Requirement: Mock exit codes and shutdown
`rustman mock serve` SHALL run until Ctrl+C, then stop accepting connections, wait up to 5 s for in-flight requests and exit with code `130`. Start-up failures (invalid document, bind error, remote bind without opt-in) SHALL exit with code `2`.

#### Scenario: Graceful stop
- **GIVEN** a request in flight with a 2000 ms delay
- **WHEN** the user presses Ctrl+C
- **THEN** that request SHALL complete before the process exits with code 130
