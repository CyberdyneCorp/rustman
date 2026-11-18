# GraphQL

Rustman treats GraphQL as more than an HTTP POST: requests are `kind: graphql` files, and a schema (introspected and cached, or read from a file) drives validation, completion and the explorer. The behavior is specified in [`openspec/changes/add-graphql-client`](../openspec/changes/add-graphql-client/).

## Request files

```yaml
# users/get-user.rman.yaml
version: 1
kind: graphql
name: Get user
endpoint: '{{base_url}}/graphql'
document: |
  query GetUser($id: ID!) {
    user(id: $id) { name email }
  }
operation_name: GetUser
variables:
  id: '{{user_id}}'
headers:
  - name: X-Client
    value: rustman
auth:
  type: bearer
  token: '{{api_token}}'
```

| Field | Required | Default | Meaning |
|-------|----------|---------|---------|
| `version`, `kind`, `name`, `description`, `tags` | | | As for every request file ([request-files.md](request-files.md#common-fields)); `kind` is `graphql`. |
| `endpoint` | yes | | The GraphQL endpoint URL. |
| `document` | one of | | The operation document, inline. |
| `document_file` | one of | | A `.graphql` file, relative to the request file and inside the workspace. |
| `operation_name` | no | | Selects one operation of a document that has several. |
| `variables` | no | | The GraphQL variables, any JSON shape. |
| `headers` | no | | `{name, value}` pairs, merged over the collection's headers. |
| `auth` | no | the collection's | Any [auth helper](http.md) of HTTP requests. |
| `transport` | no | `http` | `http`, `ws` (WebSocket subscriptions) or `sse` (subscriptions over Server-Sent Events). |
| `method` | no | `POST` | `GET` sends a query as URL parameters; mutations cannot use it. |
| `apq` | no | `false` | Automatic Persisted Queries. |
| `schema_file` | no | | An SDL or introspection JSON file used instead of the schema cache (see [Schema file](#schema-file)). |
| `ws_protocol` | no | `auto` | `auto`, `graphql-transport-ws` or `graphql-ws` (the legacy subscriptions-transport-ws protocol). |
| `connection_params` | no | | The `connection_init` payload of WebSocket subscriptions. |
| `max_events` | no | `10` | Headless subscriptions stop after this many events (at least 1). |
| `duration` | no | `30s` | Headless subscriptions stop after this long: `500ms`, `30s`, `2m`, or milliseconds. |

Exactly one of `document` and `document_file` must be set. A file with neither or both fails to load, naming the file:

```text
api/users.rman.yaml: graphql: exactly one of document or document_file is required
```

Unknown fields, an empty `endpoint`, `max_events: 0` and values outside the lists above are load errors too. Rustman never rewrites a graphql file: saving it keeps its text as written.

### Variables and interpolation

`{{name}}` placeholders are resolved with the usual [precedence](environments.md) in:

- `endpoint`;
- `headers`, including headers inherited from `_collection.yaml`;
- `auth` fields;
- every string value inside `variables` and `connection_params`, at any depth. Keys, numbers, booleans and `null` are kept as written.

With `variables: { id: "{{userId}}" }` and `userId=42`, the payload's `variables` is `{"id": "42"}`.

The document is **never** interpolated: a literal `{{x}}` inside a string argument is sent byte for byte, and a `document_file` is sent exactly as stored (line endings included). Use GraphQL variables to parameterize an operation. An unresolved placeholder fails the send and lists every missing name, as for HTTP requests.

The request's `variables` are GraphQL variables, so they are not a layer of Rustman variables. Secrets used anywhere in the request, `variables` included, are redacted wherever the request is shown or recorded.

## Schemas

A schema enables validation before send, deprecation warnings, completion and the explorer. Rustman picks the first of:

1. the request's `schema_file`;
2. the `schema_file` of the request's `[[graphql.endpoints]]` entry in `rustman.toml`;
3. the cached introspection result for the request's endpoint.

With none of them, only the document's syntax is checked. Choosing a schema never sends a request: introspection happens only when you refresh the schema explicitly (`rustman graphql introspect`, or the refresh action of the TUI GraphQL pane).

### Introspection

Refreshing sends the standard introspection query as a POST to the endpoint, through the same HTTP transport as every request (TLS, proxies, auth, cookies, redaction). It asks for descriptions, deprecations (`isDeprecated`, `deprecationReason`, including deprecated arguments and input fields), `specifiedByURL` and `isRepeatable`. A server that rejects those newer fields (`Cannot query field "specifiedByURL" ...`) is asked once more with the June 2018 introspection query.

Any other failure (introspection disabled, an error status, a body that is not a GraphQL result) fails with a hint:

```text
graphql: introspection failed: GraphQL introspection is not allowed; set schema_file to validate offline
graphql: introspection failed: HTTP 502: response is not a GraphQL result; set schema_file to validate offline
```

The result is converted to SDL and parsed. A schema that parses but breaks a validation rule is kept, with warnings: it is what the server executes.

### Schema cache

Introspection results are cached in `.rustman/graphql/schemas/<hash>.json`, under the workspace root (`rustman init` adds `.rustman/` to `.gitignore`). `<hash>` is the SHA-256 of the normalized endpoint: scheme, host, port and path, with user info, query and fragment removed. Endpoints that differ only in their query share a cache entry; each environment's host gets its own.

A cache file holds:

| Key | Meaning |
|-----|---------|
| `format` | `1`. |
| `endpoint` | The normalized endpoint, with any secret in it redacted. |
| `fetched_at` | When it was fetched, RFC 3339 UTC. |
| `sdl_sha256` | The SHA-256 of the schema as SDL, for cheap comparisons. |
| `type_count` | Named types defined by the schema (built-in types excluded). |
| `introspection` | The raw `{"__schema": ...}` result, lossless. |

A cached schema is used until it is refreshed; sending or validating a request never refreshes it. Secret values used by the request are never written to the cache file.

### Schema file

When introspection is disabled, point `schema_file` at a schema checked into the workspace. It may be SDL (`.graphql`, `.graphqls`) or an introspection result in JSON (`.json`, or any file starting with `{`; a whole response, its `data`, or the `__schema` object). When a `schema_file` applies, the cache is not read at all.

In a request file the path is relative to the request file. In `rustman.toml`, an endpoint entry applies to every request whose `endpoint` is written the same way, or resolves to the same normalized URL; its path is relative to the workspace root:

```toml
# rustman.toml
[workspace]
name = "shop"
version = 1

[[graphql.endpoints]]
url = "https://api.example.com/graphql"
schema_file = "schemas/api.graphql"
```

Both paths must stay inside the workspace.

## Validation before send

Before a request is sent, its document is checked:

- **With a schema**, against every validation rule of the GraphQL specification. Each error is reported with its `line:column` in the document, using the messages of the reference implementation:

  ```text
  1:17: Cannot query field "emial" on type "User".
  ```

  The request's `variables` are then checked against the variable definitions of the selected operation. A missing required variable (non-null, no default), a `null` for a non-null type, or a value of the wrong shape (scalar, enum value, input object field, list item) blocks the send, naming the variable's path. The messages never show the value itself, which may be a secret:

  ```text
  variable $id of type ID! is required
  variable $input.role: expected Role, got string
  ```

  Custom scalars (such as `Upload`) accept any value: only the server knows their rules.
- **Without a schema**, only the syntax is checked; a syntax error still blocks the send:

  ```text
  1:23: syntax error: expected R_CURLY, got EOF
  ```

Validation errors block the send unless you pass `--no-validate` (CLI) or confirm in the TUI.

Fields and enum values the schema marks `@deprecated` produce a warning with the reason (`No longer supported` when the schema gives none). Warnings never block the send:

```text
1:17: User.username is deprecated: Use handle
```

### Choosing the operation

A document with one operation (named or anonymous) is sent as is. A document with several needs `operation_name`; without it the send fails before anything leaves the machine:

```text
graphql: document has 2 operations; set operation_name
```

`operationName` is sent only when the request sets `operation_name`.

## Sending over HTTP

With `transport: http` (the default), queries and mutations go through the same HTTP transport as every request: TLS, proxies, cookies, auth helpers, timings and redaction all apply.

- **POST** (the default) sends a JSON body `{"query", "operationName", "variables"}` with `Content-Type: application/json`. Members that are not set (no `operation_name`, no `variables`) are left out.
- **`method: GET`** sends `query`, `operationName` and the JSON-encoded `variables` as URL parameters, for servers and CDNs that cache GET queries. Mutations and uploads cannot use GET:

  ```text
  graphql: mutations cannot use GET
  ```

- Subscriptions need `transport: ws` or `transport: sse`.

Every request sends `Accept: application/graphql-response+json, application/json` unless the request sets its own `Accept` header.

### Results

A response body is parsed into `data`, `errors` (message, locations, path, extensions) and `extensions`, kept apart, with an overall status:

| Status | Meaning |
|--------|---------|
| `ok` | No errors. |
| `partial` | Both `data` and `errors`: the data is shown next to the errors. |
| `errors` | Errors and no data. |

Error paths are shown joined with dots: `["user", "email"]` is `user.email`, `["users", 0, "name"]` is `users.0.name`. The HTTP status and timings are shown too. A body that is not a GraphQL result (an HTML error page from a proxy, say) is shown raw with:

```text
graphql: response is not a GraphQL result
```

### File uploads

A variable value `{ "$file": "path" }` uploads that file, following the [GraphQL multipart request specification](https://github.com/jaydenseric/graphql-multipart-request-spec). The path is relative to the request file and must stay inside the workspace:

```yaml
# media/upload-avatar.rman.yaml
version: 1
kind: graphql
name: Upload avatar
endpoint: '{{base_url}}/graphql'
document: |
  mutation Upload($file: Upload!) {
    upload(file: $file)
  }
variables:
  file:
    $file: ./avatar.png
```

The request becomes `multipart/form-data` with an `operations` part (the payload, each file replaced by `null`), a `map` part (`{"0": ["variables.file"]}`) and one part per file, named `0`, `1`, ..., whose `Content-Type` is guessed from the file extension (`application/octet-stream` when unknown). Markers may appear at any depth, including inside lists. A file that does not exist fails before anything is sent:

```text
graphql: upload file not found: ./avatar.png
```

The files are read when the request is sent and held in memory while it is sent, as for `kind: http` multipart bodies; very large uploads are better sent another way for now.

### Automatic Persisted Queries

With `apq: true`, Rustman first sends only the SHA-256 hash of the document (`extensions.persistedQuery` with `version: 1` and `sha256Hash`), without `query`. If the server answers `PersistedQueryNotFound` (by message or by `extensions.code`), it sends the full document with the hash once more, which registers it. The result shows which happened: `APQ hit`, or `APQ miss (registered)` after two requests. When the second answer is `PersistedQueryNotFound` again or not a GraphQL result, it shows `APQ miss (not registered)`. A server that answers `PersistedQueryNotSupported` also gets the full document once, shown as `APQ miss (not registered)`. When the first answer is not a GraphQL result (an HTML error page, say), neither is shown: the server did not say whether it knew the hash.

## Explorer and completion

With a schema, the engine exposes explorer data for the TUI explorer and the agent interface: the root operation types, every named type with its kind and description, fields with arguments, argument types and defaults, enum values, input fields, interfaces and possible types, the `@specifiedBy` URL of custom scalars, directives, and the deprecation status of each field, argument, input field and enum value.

Completion works on documents that are still being typed. At the cursor it offers the fields of the enclosing type (with `__typename`), the arguments of the enclosing field or directive, the values of an enum-typed argument, the variables the operation declares (after `$`), directive names (after `@`) and fragment names (after `...`), each with its type signature, such as `user(id: ID!): User`.

### In the TUI

The [TUI GraphQL editor](tui.md#the-graphql-editor) uses the same data: the document is checked as you type, with problems underlined and the one at the cursor shown on the editor's last line; `Ctrl+Space` opens the completion popup; the `Schema` tab browses types and fields. `Ctrl+G` refreshes the schema by introspection. Results show in the `Data`, `Errors`, `Extensions` and `Events` tabs of the response viewer, redacted like history.

## Subscriptions

Subscriptions need `transport: ws` or `transport: sse`. Each result the server pushes is recorded as a timestamped event, in order.

```yaml
# live/on-message.rman.yaml
version: 1
kind: graphql
name: On message
endpoint: '{{base_url}}/graphql'
transport: ws
document: |
  subscription OnMessage($room: String!) {
    onMessage(room: $room) { id text }
  }
variables:
  room: lobby
connection_params:
  authToken: '{{api_token}}'
max_events: 5
duration: 10s
assertions:
  - no_errors: true
  - data: {path: '$[0].onMessage.text', exists: true}
```

### WebSocket

With `transport: ws`, an `http://` or `https://` endpoint is reached as `ws://` or `wss://` (a `ws(s)://` endpoint is used as is). The handshake carries the request's headers, auth helper and cookies, and uses its TLS settings (`ca_bundle`, client certificates, `verify_tls`, `--insecure`); proxies are not used for WebSockets.

Rustman speaks [`graphql-transport-ws`](https://github.com/enisdenjo/graphql-ws/blob/master/PROTOCOL.md): it sends `connection_init` with `connection_params` as its payload, waits for `connection_ack`, sends `subscribe`, records each `next`, answers `ping` with `pong`, and sends `complete` when it stops the subscription. A server that does not acknowledge the connection within 10 seconds (or, headless, within what is left of `duration`; the handshake is bounded the same way) stops the subscription with:

```text
graphql: connection_ack timeout
```

`ws_protocol` picks the subprotocol:

| `ws_protocol` | Behavior |
|---------------|----------|
| `auto` (default) | Offers `graphql-transport-ws` and `graphql-ws`. A server that only accepts the legacy `graphql-ws` (subscriptions-transport-ws) gets that protocol (`start`, `data`, `stop`, `connection_terminate`), and the result shows `legacy subscriptions-transport-ws`. |
| `graphql-transport-ws` | Only the modern protocol. |
| `graphql-ws` | Only the legacy protocol. |

An `error` message from the server ends the subscription; its errors become the last event. A server that closes the connection without `complete` fails the subscription, with the close code and reason `graphql-transport-ws` uses for errors (for example `4401 Unauthorized` or `4403 Forbidden`):

```text
the server closed the connection without complete (4403 Forbidden)
```

Events are held in memory, so one WebSocket message (and one frame) may not be larger than `rustman.toml [http] max_body_bytes` (10 MiB by default); a larger one ends the subscription with `error` and `WebSocket: a message is larger than the <n> byte limit (max_body_bytes)`. The events received before it are kept.

When Rustman stops a subscription, it gives the server 2 seconds to take the `complete` (or `stop`) message and the close; a server that does not read them is left behind.

### Server-Sent Events

With `transport: sse`, Rustman follows the [GraphQL over SSE](https://github.com/enisdenjo/graphql-sse/blob/master/PROTOCOL.md) distinct connections mode: it POSTs the operation (as for queries) with `Accept: text/event-stream`, through the same HTTP transport as every request, records each `next` event and stops on `complete`. Stopping early closes the connection. A stream that ends without a `complete` event fails the subscription with `SSE: the stream ended without a complete event`. Events are held in memory, so one event (with the line being read) may not be larger than `rustman.toml [http] max_body_bytes` (10 MiB by default, see [HTTP](http.md)); a larger one ends the subscription with `error` and `SSE: an event is larger than the <n> byte limit (max_body_bytes)`. A response that is not a successful event stream fails with `graphql: subscription: SSE: HTTP <status> ...`. The response must start within the request `timeout` (headless, within what is left of `duration`), or the subscription fails with `SSE: the stream did not open within <n> ms`; stopping before then fails it with `SSE: stopped before the stream opened`.

### When a subscription stops

Headless subscriptions (`rustman run`, `rustman graphql query`) stop after `max_events` events (default 10) or `duration` (default `30s`), whichever comes first; `--max-events` and `--duration` replace both for every request of the command. A subscription also stops when the server completes it, closes the connection or sends an error, and on Ctrl+C: it then tells the server it stopped and keeps the events collected so far, which are printed, checked and recorded with `stopped` (it has 5 seconds for this; the command exits 130). Interactive subscriptions in the TUI run until the server completes them or you stop them (`Alt+X`); the first 1000 events are kept for the result and its assertions, later ones are counted, and history records the first 100.

The reason is recorded as `complete`, `max_events`, `duration`, `stopped`, `closed` or `error`. A subscription that failed (closed without `complete`, or an SSE event over the size limit) also records why as `failure`, which is an error of its result: `no_errors` fails and `rustman graphql query` exits 1.

## Assertions and captures

GraphQL requests take `assertions` and `captures` like HTTP requests (see [Assertions and captures](assertions.md)), plus checks of the GraphQL result:

| Assertion | Passes when |
|-----------|-------------|
| `no_errors: true` | The result has no `errors` (`no_errors: false`: it has some). A failure lists each error's message and path. |
| `errors_count: <n>` | There are exactly `n` errors; `errors_count: {gt\|gte\|lt\|lte\|equals: <n>}` compares instead. |
| `error_message_contains: <text>` | Some error's `message` contains `text`. |
| `error_code: <code>` | Some error's `extensions.code` is `code`. |
| `data: {path, <op>: value}` | A [JSONPath check](assertions.md#json-fields) rooted at `data`: `$.user.name` reads `data.user.name`. The operators are those of `json`. |

```yaml
# auth/login.rman.yaml
version: 1
kind: graphql
name: Login
endpoint: '{{base_url}}/graphql'
document: |
  mutation Login($password: String!) {
    login(user: "ada", password: $password) { token }
  }
variables:
  password: '{{password}}'
assertions:
  - no_errors: true
  - data: {path: $.login.token, type: string}
captures:
  - {var: token, from: json, path: $.data.login.token}
```

Captures read the whole response body, so a value under `data` is at `$.data...`; the captured `token` is set for the later requests of the run. A failed `no_errors` reads, for instance:

```text
no_errors: expected no errors, got 1 error: "email is private" at user.email
```

For a subscription, the checks read the collected events as one result: `data` is the list of every event's data (`$[0].onMessage`, `$` with `length`) and `errors` holds every event's errors. Status, header and latency assertions read the handshake (`101`) or the SSE response.

## In rustman run

`rustman run` executes graphql request files alongside HTTP ones, in collection order: values captured by either kind are available to the next requests. Each graphql request is validated and sent as described above; a validation error, an ambiguous document or a transport failure makes the request `errored` (exit code 3), and failed assertions make it `failed` (exit code 1). Reports list GraphQL assertions with their own kinds (`no_errors`, `errors_count`, `error_message_contains`, `error_code`, `data`), and a response that is not a GraphQL result adds the warning `graphql: response is not a GraphQL result`.

## History

Every executed GraphQL operation is recorded in [history](history.md) like any request, and its entry's `record.graphql` adds:

| Key | Meaning |
|-----|---------|
| `operation_name`, `operation_type` | The selected operation and whether it is a `query`, `mutation` or `subscription`. |
| `transport`, `protocol` | `http`, `ws` or `sse`, and for subscriptions the protocol used (`graphql-transport-ws`, `legacy subscriptions-transport-ws` or `sse`). |
| `variables` | The variables sent, uploads as `null`, with secrets redacted as `«redacted:<name>»`. |
| `result` | `ok`, `partial` or `errors`; `null` when the response was not a GraphQL result. |
| `apq` | `APQ hit`, `APQ miss (registered)` or `APQ miss (not registered)`, with `apq: true`; `null` when the first answer is not a GraphQL result. |
| `subscription` | `event_count`, the first 100 `events` (`ts`, `elapsed_ms`, `payload`, redacted), why it `ended` and, when it failed, the `failure` (redacted). |

Subscriptions cannot be replayed with `rustman history replay`; queries and mutations over HTTP can.

## CLI commands

### rustman graphql introspect

```text
rustman graphql introspect <endpoint | request> [--out <file>] [--env <name>] [--var k=v]... [--timeout <duration>] [--insecure]
```

Fetches the schema of an endpoint URL, or of a graphql request's endpoint resolved in the active environment (with its headers and auth), and stores it in the [schema cache](#schema-cache). Outside a workspace a URL can still be introspected, but nothing is cached. `--out <file>` also writes the schema as SDL.

```text
$ rustman graphql introspect https://api.example.com/graphql --out schema.graphql
introspected https://api.example.com:443/graphql: 42 types, fetched at 2026-11-18T09:30:00.123Z
cached in .rustman/graphql/schemas/5f2b...e1.json
wrote schema.graphql
```

It exits 0 on success, 3 when the server cannot be reached or refuses introspection (with the `set schema_file` hint), and 2 for a bad argument.

### rustman graphql query

```text
rustman graphql query <request | --endpoint <url> --query <text>> [--env <name>] [--var k=v]...
                      [--no-validate] [--json] [--max-events <n>] [--duration <duration>]
                      [--timeout <duration>] [--insecure] [--no-history]
```

Validates the operation, sends it and prints the result. The request is a graphql request file, its id or its name; `--endpoint` with `--query` sends an ad-hoc operation without a workspace (a `ws://` or `wss://` endpoint subscribes over WebSocket). `--no-validate` sends even when validation fails. The request's assertions are checked and the exchange is recorded in history, as in `rustman run`.

The output starts with the HTTP status, the result status and the time, then `data`, `errors` and `extensions` apart; a subscription prints its protocol, event count and why it ended, the failure when there is one (`failed: ...`), then each event. Everything is redacted. `--json` prints one JSON object instead (`status`, `result`, `data`, `errors`, `extensions`, `apq`, or for a subscription `protocol`, `ended`, `event_count`, `failure` and `events`).

```text
$ rustman graphql query users/get-user.rman.yaml --env dev
HTTP 200 · partial · 41 ms
data:
{
  "user": {
    "name": "Ada",
    "email": null
  }
}
errors:
  - email is private (path user.email, at 1:22, code FORBIDDEN)
```

| Exit code | Meaning |
|-----------|---------|
| 0 | No errors (and the request's assertions passed). |
| 1 | `errors` is not empty, the response is not a GraphQL result, or an assertion failed. |
| 2 | Usage or configuration error (no workspace, unknown request, not a graphql request). |
| 3 | No response (network, TLS, timeout, unresolved variable, `connection_ack` timeout) or the document failed validation. |
| 130 | Interrupted by Ctrl+C. |
