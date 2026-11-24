# Mock server

`rustman mock serve` serves an OpenAPI 3.0 or 3.1 document as a local HTTP mock. Each operation becomes a route, and responses come from the document's examples or from data synthesized from its schemas. You can build a client before the backend exists, or ask for the error responses that are hard to get from a real one.

The mock is a development tool, not a security boundary. It binds to loopback by default.

The [TUI](tui.md#mock-server-pane) runs the same server in its mock server pane (`Alt+O`).

The behavior is specified in [`openspec/specs/mock-server`](https://github.com/CyberdyneCorp/rustman/blob/main/openspec/specs/mock-server/spec.md).

## Starting the mock

```text
$ rustman mock serve petstore.yaml
mock: serving 19 operations on http://127.0.0.1:4010
```

The mock speaks HTTP/1.1 (with keep-alive) and h2c (HTTP/2 without TLS) on the same port. It runs until Ctrl+C. Then it stops accepting connections, gives the requests in flight up to 5 seconds to finish, and exits with code 130.

| Flag | Effect |
|------|--------|
| `<OPENAPI>` | The document: a file or an `http(s)` URL. |
| `--host <HOST>` | Address to bind: an IP address or `localhost` (default: `[mock] host` in `rustman.toml`, else `127.0.0.1`). |
| `--port <PORT>` | Port to bind (default: `[mock] port`, else `4010`). `0` picks a free port, and the banner shows it. |
| `--allow-remote` | Required to bind an address reachable from the network, such as `0.0.0.0` or a LAN IP. |
| `--allow-remote-refs` | Fetch `$ref`s that point at other `http(s)` documents. Without it they are refused, as in [`rustman import openapi`](import-openapi.md). |
| `--seed <N>` | Seed of synthesized bodies (default: `[mock] seed`, else `0`). See [Synthesized data](#synthesized-data). |
| `--no-prefer` | Ignore the [`Prefer`](#the-prefer-header) request header. |
| `--no-validate` | Skip [request validation](#request-validation). |
| `--enforce-security` | Answer `401` to requests without the credentials their operation declares. See [Security](#security). |
| `--stateful` | Back resources with an in-memory store. See [Stateful mode](#stateful-mode). |
| `--env <NAME>` | The environment whose secrets hold the `[mock.security]` expected values, and that receives the secrets `--record` moves out of recorded requests (default: `RUSTMAN_ENV`, else the workspace default). |
| `--workspace <DIR>` | The workspace whose `rustman.toml` holds `[mock]`. Without it the workspace is searched from the current directory upwards. Outside a workspace the defaults apply. |
| `--cors`, `--no-cors` | Turn [CORS](#cors) on or off (default: on for a loopback bind, off otherwise). |
| `--jsonl` | Write the [request log](#request-log) as JSON lines on stdout; the banner goes to stderr. |
| `--history` | Also record each exchange in [history](#history), tagged `mock`. |
| `--proxy-unmatched <URL>` | Forward requests that nothing matches to this upstream. See [Proxy and record](#proxy-and-record). |
| `--record <DIR>` | Save each proxied exchange as a request file in `DIR` (needs `--proxy-unmatched` and a workspace). |

Settings can live in the workspace. The flags win over these:

```toml
[mock]
host = "127.0.0.1"
port = 4011
seed = 7
max_body_bytes = 10485760   # 413 past it
max_headers = 100           # 431 past it
max_header_bytes = 65536    # 431 past it
dir = "mocks"               # the hand-written overrides
delay_ms = 0                # delay of every response without an override delay
jitter_ms = 0               # uniform ± range around delay_ms
max_items = 10000           # stateful items per resource (507 past it)
max_store_bytes = 67108864  # stateful JSON in all resources together (507 past it)

[mock.security]
bearerAuth = "{{api_token}}"   # expected value of a security scheme, from an environment secret
```

### Start-up errors

Every start-up failure exits with code 2 before anything is served.

A port that cannot be bound:

```text
$ rustman mock serve petstore.yaml
error: mock: cannot bind 127.0.0.1:4010: address in use
```

An address reachable from the network, without `--allow-remote`:

```text
$ rustman mock serve petstore.yaml --host 0.0.0.0
error: mock: binding 0.0.0.0 exposes the mock to the network; pass --allow-remote
```

With `--allow-remote`, the mock starts and prints a warning on stderr.

A document that is not valid. The document is read by the same parser as [`rustman import openapi`](import-openapi.md), with the same versions, `$ref` rules and diagnostics. Each error names the JSON pointer of the problem:

```text
$ rustman mock serve api.yaml
error: /paths/~1a/get/responses/200/content/application~1json/schema/$ref: unresolved reference '#/components/schemas/Nope'
```

## Routing

Each path and method of the document is a route.

- **Path parameters.** A template such as `/users/{id}` matches one segment per parameter. Literal text around a parameter, as in `/files/{name}.json`, must match too. Parameter values are percent-decoded.
- **Literal segments win.** When several paths match, the one with more literal segments wins, then the one earlier in the document. `GET /users/me` goes to `/users/me`, not `/users/{id}`, in whatever order the document lists them.
- **Base paths.** The path of each `servers` URL (with its variables at their defaults) is stripped from the request first. With `servers: [{url: https://api.example.com/v1}]`, both `/v1/pets` and `/pets` reach `/pets`.
- **404.** A path that matches nothing gets `404`.
- **405.** A path that exists without the request's method gets `405` with an `Allow` header listing the declared methods, such as `Allow: GET, POST`.

## Errors

Every error the mock generates itself is `application/problem+json` (RFC 9457) with `type`, `title`, `status` and `detail`:

```json
{
  "type": "urn:rustman:mock:no-matching-operation",
  "title": "No matching operation",
  "status": 404,
  "detail": "GET /nope matches no operation of the document"
}
```

| Status | `type` (after `urn:rustman:mock:`) | When |
|--------|-----------------------------------|------|
| 400 | `undocumented-code`, `unknown-example` | `Prefer` asks for a status or example the operation does not document. The `detail` lists the documented ones. |
| 400 | `malformed-request` | The body could not be read. |
| 400 | `invalid-parameters` | A path, query, header or cookie parameter is missing or invalid. |
| 401 | `unauthorized` | With `--enforce-security`, the credentials are missing or do not match. |
| 404 | `no-matching-operation` | No path matches. |
| 404 | `unknown-id` | Stateful mode: no item has that id. |
| 404 | `reserved-path` | A path under `/__rustman/` other than `POST /__rustman/reset`. |
| 405 | `method-not-allowed` | The path does not declare the method. |
| 406 | `not-acceptable` | `Accept` matches none of the documented media types. |
| 408 | `request-timeout` | The body did not arrive within 30 seconds. |
| 413 | `body-too-large` | The body is over the limit. |
| 415 | `unsupported-media-type` | The body's `Content-Type` is not documented for the operation. |
| 422 | `invalid-body` | The body is not valid JSON or does not match its schema. |
| 431 | `header-fields-too-large` | Too many header fields, or too many header bytes. |
| 507 | `store-full` | Stateful mode: the resource already holds `max_items` items. |
| any | `injected-fault` | An override's `fault.error_rate` picked this request. |

Validation errors add an `errors` array, with one entry per violation (up to 20):

```json
{
  "type": "urn:rustman:mock:invalid-parameters",
  "title": "Invalid request parameters",
  "status": 400,
  "detail": "1 violation of the operation's schema",
  "errors": [{"location": "query", "pointer": "/q", "message": "required"}]
}
```

## Choosing the response

A request first passes [security](#security) (with `--enforce-security`) and [validation](#request-validation). Then the response is chosen in this order:

1. **A matching [override](#overrides).** Overrides also answer paths the document does not have.
2. **The [stateful store](#stateful-mode)**, with `--stateful`, for operations on a resource. `Prefer` does not apply to these operations.
3. **The `Prefer` header**, when the request sends one (see [below](#the-prefer-header)).
4. **The lowest documented `2xx`.** A `2XX` range counts as `200`. Without any `2xx`, `default` is used and sent as `200`. An operation that documents only errors gets its lowest one.

The response's media type is then negotiated from `Accept`, and the body is taken, in order, from:

1. the first named example (`examples`) of that media type;
2. the media type's `example`;
3. the schema's `examples` or `example`;
4. data synthesized from the schema.

A response without `content` is sent with an empty body and no `Content-Type`. A JSON media type is sent as compact JSON. A string example of another media type, such as `text/plain`, is sent as is.

### The `Prefer` header

The `Prefer` header is compatible with Prism:

| Preference | Effect |
|------------|--------|
| `code=<status>` | Answer with that status. It must be documented exactly, by its range (`4XX`) or by `default`. Otherwise the answer is `400`, listing the documented codes. |
| `example=<name>` | Use that named example of the chosen response. An unknown name gets `400`, listing the documented names. |
| `dynamic=true` | Synthesize the body from the schema, even when examples exist. |

Preferences are separated by commas or semicolons, and can be combined:

```text
$ curl -i http://127.0.0.1:4010/pets/1 -H 'Prefer: code=404, example=notFound'
HTTP/1.1 404 Not Found
content-type: application/json

{"error":"no such pet"}
```

`--no-prefer` makes the mock ignore the header.

### Content negotiation

Without an `Accept` header, or with only `*/*`, the first JSON media type of the response is used (`application/json` or any `+json` type), else the first one. Otherwise the documented media type that the highest-quality `Accept` range covers wins. Ranges such as `application/*` work in both directions. When nothing is acceptable, the answer is `406`.

## Synthesized data

When there is no example, the body is synthesized from the schema by the same synthesizer as [`rustman import openapi`](import-openapi.md). It fills the required properties (all properties when none is required) and honors `type`, `format`, `pattern`, `enum`, `const`, length, item-count and numeric bounds. A string with a `pattern` its plain sample does not match is generated from the regular expression (`\d`, `\w`, `\s` and `.` with their ASCII meaning). Past three levels of nesting only required properties and `minItems` items are filled, so circular schemas stay small; a schema whose required properties recurse without end has no finite instance and is cut at ten levels.

Synthesis is seeded. The seed combines `--seed` (default `0`), the operation and the request path. So the same request always gets the same body, whatever the order of calls, and another seed gives other data:

- free-form strings get a suffix, such as `string-3fa2c1`;
- numbers move up inside their bounds and stay on `multipleOf`;
- booleans flip.

Strings with a `format`, and values the schema documents (`const`, `enum`, `default`, examples), stay as they are; strings with a `pattern` are generated from the seed. A synthesized body validates against its schema, except where the schema cannot be satisfied (endless required recursion, contradictory bounds) or uses a regular expression the generator does not support (look-around, back-references), where the plain sample is kept.

## Request validation

By default every request is checked against its operation before a response is chosen:

- **Parameters → `400`.** Required path, query, header and cookie parameters must be present. Their text is read as the schema's type (`integer`, `number`, `boolean`, or an `array` written as `a,b` or as repeated query parameters) and validated against the schema.
- **Media type → `415`.** A body must have a `Content-Type` that the operation documents (ranges such as `application/*` count).
- **Body → `422`.** A JSON body must parse and match its schema. A required body must not be empty.

The schemas are those of [`rustman import openapi`](import-openapi.md): OpenAPI 3.0 schemas are converted to JSON Schema 2020-12 (`nullable`, boolean `exclusiveMinimum`) and validated by the same engine as `schema:` assertions. Every violation is listed, up to 20: missing required properties, then the first violation of each property, then the first of the body as a whole. Bodies of non-JSON media types are only checked for their media type.

`--no-validate` turns validation off.

## Security

Credentials are not checked by default. With `--enforce-security`, a request must carry the credentials of one alternative of its operation's security requirement (the operation's own `security`, else the document's). Otherwise the answer is `401` with a `WWW-Authenticate` header for each scheme involved:

| Scheme | Credential | `WWW-Authenticate` |
|--------|------------|--------------------|
| `http` `bearer`, `oauth2`, `openIdConnect` | `Authorization: Bearer <token>` | `Bearer` |
| `http` `basic` | `Authorization: Basic <base64 of user:password>` | `Basic realm="rustman-mock"` |
| `apiKey` | the named header, query parameter or cookie | `ApiKey name="...", in="..."` |

```text
$ curl -i http://127.0.0.1:4010/users
HTTP/1.1 401 Unauthorized
www-authenticate: Bearer
content-type: application/problem+json
```

Any value is accepted unless `[mock.security]` names an expected one for the scheme. Each expected value is a `{{variable}}` read from the active environment's secrets (never a literal in `rustman.toml`); for `basic` it is `user:password`. Values are compared in constant time and never appear in responses.

## Overrides

Hand-written overrides live in `*.rman-mock.yaml` files in the `mocks/` directory of the workspace (or of the current directory outside a workspace; `[mock] dir` changes it). Files are read in name order and overrides in declaration order. The first override that matches answers, even for paths the document does not have:

```yaml
overrides:
  - name: big payment               # optional
    match:
      method: POST                  # default: any method
      path: /payments               # exact, a template (/users/{id}) or a glob (* within a segment, ** across)
      query: {currency: EUR}        # each pair must be present
      headers: {X-Tenant: acme}     # names in any case
      body:                         # JSONPath conditions on a JSON body, all must hold
        $.amount: {gt: 10000}
    respond:
      status: 402                   # default 200
      headers: {X-Mock: "1"}
      body: {error: "over the limit"}
  - match: {method: GET, path: /health}
    respond: {body: '{"ok":true}'}
```

| Condition | Holds when the first node the JSONPath selects |
|-----------|-----------------------------------------------|
| a bare value, or `eq: <value>` | equals the value |
| `ne: <value>` | differs from it (or is absent) |
| `gt`, `gte`, `lt`, `lte` | is a number past the bound |
| `contains: <text>` | is a string containing the text, or an array holding it |
| `exists: true` / `false` | is present / absent |

`respond.body` can be a structure (sent as JSON), a string (sent as JSON when it parses, else as text), or `body_file: <path>` relative to the mock file, inside its directory and at most 10 MiB. `Content-Type` follows from the body unless `respond.headers` sets it. Framing and hop-by-hop headers (`Content-Length`, `Transfer-Encoding`, `Connection`...) cannot be set.

Requests to documented operations are checked for security and validated before overrides are tried. An invalid mock file stops start-up with exit code 2 and its file name, line and override number.

### Response templates

Override bodies (including `body_file` text) and header values can use request data:

| Template | Value |
|----------|-------|
| `{{request.path.<name>}}` | a path parameter of the override's template, or of the matched operation |
| `{{request.query.<name>}}` | the first value of a query parameter |
| `{{request.header.<name>}}` | a header (any case) |
| `{{request.body.<JSONPath>}}` | the first node of the JSON body, such as `{{request.body.$.user.name}}` |
| `{{$uuid}}`, `{{$timestamp}}`, `{{$randomInt}}`... | the [dynamic variables](environments.md#dynamic-variables) of `environments`, except `{{$env}}` |

```yaml
overrides:
  - match: {method: GET, path: '/users/{id}'}
    respond:
      body: {id: "{{request.path.id}}"}   # GET /users/42 → {"id":"42"}
```

A reference that does not resolve renders as an empty string and is logged as a warning on stderr. Templates only substitute; there is no logic.

### Latency and faults

```yaml
overrides:
  - match: {path: /slow}
    delay_ms: 2000          # before the response headers
    jitter_ms: 500          # uniform ± range; both at most 60000
  - match: {path: /flaky}
    fault: {error_rate: 0.2, status: 503}   # 20% of requests get a 503 problem+json (default status 500)
  - match: {path: /gone}
    fault: {drop_connection: true}          # close the connection without a response
  - match: {path: /trickle}
    fault: {slow_body_bps: 100}             # stream the body at 100 bytes per second
```

`[mock] delay_ms` and `jitter_ms` delay every response that has no override delay.

Delays and faults are random but reproducible: each route (an override's `name`, else its file and position) has its own sequence, seeded by `--seed` and advanced per request. The same seed and the same requests to a route give the same delays and faults, whatever other routes receive meanwhile.

## Stateful mode

With `--stateful`, resources are backed by an in-memory store. Resources are found like the explorer's [dependency discovery](exploration.md#dependency-discovery): a `POST` collection (`/users`) whose response holds the id that the item path's parameter (`/users/{id}`) takes.

| Request | Effect |
|---------|--------|
| `POST /users` | Stores the body with a new id and answers with it and the documented creation status. |
| `GET /users` | Lists the stored items, when the documented response is an array. |
| `GET /users/{id}` | The stored item, else `404`. |
| `PUT /users/{id}` | Replaces the item (keeping its id), else `404`. |
| `PATCH /users/{id}` | Merges the body into the item (JSON merge patch), else `404`. |
| `DELETE /users/{id}` | Removes the item, else `404`. |

New ids follow the id's schema: `format: uuid` gives a UUIDv4 drawn from the seed, `integer` counts from 1, anything else counts as text (`"1"`). Each resource holds at most 10000 items (`[mock] max_items`), and all resources together at most 64 MiB of JSON (`[mock] max_store_bytes`); a create, replace or patch past either limit gets `507`.

`POST /__rustman/reset` clears the store and answers `204`. Every path under `/__rustman/` is reserved for the mock and never routed to the document. The store lives in memory only and is lost when the mock stops.

## Request limits

The mock enforces the same defaults as the [webhook inspector](webhooks.md). Each limit can be changed under `[mock]`.

| Limit | Default | Over it |
|-------|---------|---------|
| Request body (`max_body_bytes`) | 10 MiB | `413`. A declared `Content-Length` past the limit is refused before the body is read. |
| Header fields (`max_headers`) | 100 | `431` |
| Header bytes (`max_header_bytes`) | 64 KiB | `431` |

Requests far past the header limits (more than twice the limit) are refused by the HTTP layer itself with a plain `431`. Malformed requests get `400`. On HTTP/1.1, a refused request also closes the connection.

## CORS

CORS is on by default when the mock is bound to loopback, so a browser app on another local port (a Vite dev server on `http://localhost:5173`, say) can call it. It is off for any other bind. `--cors` and `--no-cors` override the default.

When CORS is on:

- A preflight (`OPTIONS` with `Origin` and `Access-Control-Request-Method`) is answered `204` before routing, security and validation run. `Access-Control-Allow-Origin` echoes the request's `Origin`, `Access-Control-Allow-Methods` lists the methods the document declares for the path (the requested method for a path it lacks), and `Access-Control-Allow-Headers` echoes `Access-Control-Request-Headers`.
- Every other response to a request with an `Origin` echoes it in `Access-Control-Allow-Origin`, with `Access-Control-Expose-Headers: *` and `Vary: Origin`. Credentials are never allowed (no `Access-Control-Allow-Credentials`), and responses forwarded by [`--proxy-unmatched`](#proxy-and-record) get no CORS headers, so a web page cannot read the upstream through the mock.

With `--no-cors`, an `OPTIONS` request is routed like any other (usually `405`, unless the document declares `options`).

## Request log

Every request is logged once its response is sent, with:

| Field | Meaning |
|-------|---------|
| `time` | When it arrived, RFC 3339 UTC. |
| `method`, `path` | The request line; the path keeps its query. |
| `operation` | The matched operation's `operationId` (else `METHOD /template`). |
| `override` | The name of the override that answered, if one did. |
| `status` | The response status; `null` when a fault dropped the connection. |
| `source` | Where the response came from: `override`, `prefer`, `example`, `synthesized`, `stateful`, `proxy`, `cors` (a preflight) or `error` (an error the mock made itself, including injected faults). |
| `errors` | The validation errors (`location pointer: message`), the detail of other errors, and unresolved template references. |
| `duration_ms` | Time from the request head to the response head. |
| `headers` | The request headers. |

By default each entry is one line on stderr:

```text
2026-11-18T10:15:30.125Z GET /pets/1 -> 200 example (getPet) 0.4 ms
2026-11-18T10:15:31.002Z POST /pets -> 422 error (createPet) 0.3 ms [body /name: 5 is not of type "string"]
```

With `--jsonl`, each entry is one JSON object on stdout (and the banner moves to stderr, so stdout holds only entries):

```json
{"time":"2026-11-18T10:15:30.125Z","method":"GET","path":"/pets/1","operation":"getPet","status":200,"source":"example","duration_ms":0.4,"headers":[{"name":"authorization","value":"«redacted:Authorization»"}]}
```

Entries are redacted before they are written: credential headers (`Authorization`, `Cookie`, the headers the document's `apiKey` schemes name, and names like `X-Api-Key`, `X-Auth-Token` or `Client-Secret`) always show as `«redacted:<header>»`; credential query parameters (the `apiKey` ones, and names like `api_key`, `token` or `key`) show as `api_key=«redacted:api_key»`; and the values of both, and the expected values of `[mock.security]`, are replaced wherever else they appear. Entries are written on their own thread, so a slow reader of the output never holds up the server; when it falls more than 1024 entries behind, the skipped count is reported on stderr.

## History

With `--history`, each exchange is also recorded in [history](history.md), after the same redaction. In a workspace it goes to the workspace's history, otherwise to the ad-hoc one, subject to the usual retention limits. The entries carry `"request": "mock"` (`rustman history list --json` shows it), the request as received, the status and the duration, and `record.mock` holds the source, the operation, the override and the errors. They cannot be replayed with `rustman history replay`.

## Hot reload

The mock watches the document (when it is a file) and the mocks directory. About 200 ms after the last change it reads both again and serves the new version from the next request on:

```text
mock: reloaded: 20 operations, 3 overrides
```

Requests in flight finish with the version they started with, and the stateful store is kept. If the new version does not load (invalid YAML, a dangling `$ref`, an invalid mock file), the previous one keeps being served and the error is logged with its file and location:

```text
mock: reload failed, still serving the previous version: api.yaml: invalid YAML at line 4: unclosed bracket '['
```

## Proxy and record

`--proxy-unmatched <URL>` forwards every request that matches no operation and no override (instead of answering `404` or `405`) to that upstream, through the [HTTP engine](http.md). The request path and query are appended to the URL (`--proxy-unmatched https://staging.example.com` sends `GET /legacy/report` to `https://staging.example.com/legacy/report`). The URL may carry a base path but no query.

The request is sent with the [header hygiene](webhooks.md#replay-and-save) of webhook replay: hop-by-hop headers, the headers `Connection` names, `Host`, `Content-Length` and `Transfer-Encoding` are dropped, as is `Proxy-Authorization` (it is meant for the mock, never forwarded or recorded), and the engine sets the framing for the upstream. The upstream's response is returned as it came (status, headers, body; compressed bodies stay compressed), without its own hop-by-hop and framing headers. Redirects are returned to the client, never followed, so the upstream's origin is the only one contacted. An upstream that cannot be reached gets `502` problem+json. The request log marks forwarded requests `proxy`.

`--record <DIR>` saves each proxied exchange as a `kind: http` request file in `DIR`, which must be inside the workspace. The exchange goes through the same pipeline as a [HAR import](import.md): the file is named after the method and path (`recorded/get-legacy-report.rman.yaml`), a different request with the same name gets the next free suffix (`get-legacy-report-2.rman.yaml`), the same request is recorded once, and credentials (an `Authorization` token, an API key header) are moved to secrets of the active environment (`--env`, else `RUSTMAN_ENV`, else the workspace default; a new `environments/recorded.yaml` without one), leaving a `{{placeholder}}` in the file:

```yaml
kind: http
name: GET /legacy/report
method: GET
url: https://staging.example.com/legacy/report
headers:
  - name: authorization
    value: Bearer {{authorization_token}}
```

Each saved file is reported on stderr (`mock: recorded recorded/get-legacy-report.rman.yaml`).

## Self-check against the explorer

A mock of a valid document is a consistent [exploration](exploration.md) target: the explorer's conformance probes find nothing against it. CI checks this for [`fixtures/petstore.yaml`](https://github.com/CyberdyneCorp/rustman/blob/main/fixtures/petstore.yaml):

```bash
rustman mock serve fixtures/petstore.yaml --port 4010 &
rustman explore run --spec fixtures/petstore.yaml --target http://127.0.0.1:4010 \
  --probes conformance --fail-on info --env ci --allow-destructive
```

The `ci` environment gives the security schemes' variables (`bearerAuth_token`, `apiKey_key`) any value, since the mock does not check credentials by default. The exploration reports zero findings and exits with code `0`.
