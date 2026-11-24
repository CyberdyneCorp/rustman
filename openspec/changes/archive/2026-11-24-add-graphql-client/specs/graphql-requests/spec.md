# Spec Delta

## Purpose

Lets users define, validate and execute GraphQL operations — queries, mutations, subscriptions and uploads — with a cached, explorable schema, from the TUI, the CLI runner and saved request files.

## ADDED Requirements

### Requirement: GraphQL request file
A request file with `kind: graphql` SHALL accept `endpoint` (required), `document` (inline string) or `document_file` (path relative to the request file), `operation_name`, `variables` (map), `headers`, `auth`, and `transport` (`http` default, `ws`, `sse`). Exactly one of `document` / `document_file` SHALL be present, otherwise loading SHALL fail with `graphql: exactly one of document or document_file is required`.

#### Scenario: Load a valid graphql request
- **GIVEN** `users.rman.yaml` with `kind: graphql`, `endpoint: "{{base}}/graphql"` and an inline `document`
- **WHEN** the workspace is loaded
- **THEN** the request SHALL appear in its collection as a graphql request

#### Scenario: Both document sources present
- **GIVEN** a graphql request with both `document` and `document_file`
- **WHEN** the workspace is loaded
- **THEN** loading SHALL fail with `graphql: exactly one of document or document_file is required` naming the file

### Requirement: Variable interpolation in GraphQL requests
`{{name}}` placeholders SHALL be resolved per the `environments` precedence in `endpoint`, `headers`, and string values inside `variables`. Placeholders inside the GraphQL `document` SHALL NOT be interpolated; GraphQL variables are the supported mechanism. Unresolved placeholders SHALL fail the send as defined by `environments`.

#### Scenario: Variables map interpolated
- **GIVEN** `variables: { id: "{{userId}}" }` and environment `userId=42`
- **WHEN** the request is sent
- **THEN** the payload `variables` SHALL be `{"id": "42"}`

#### Scenario: Document left untouched
- **GIVEN** a document containing the literal text `{{x}}` inside a string argument
- **WHEN** the request is sent
- **THEN** the document SHALL be sent byte-for-byte unchanged

### Requirement: Query and mutation over HTTP
For `transport: http`, the system SHALL POST `{"query", "operationName", "variables"}` as `application/json` with `Accept: application/graphql-response+json, application/json`, omitting null members. When `method: GET` is set on a query, the system SHALL send `query`, `operationName` and JSON-encoded `variables` as URL parameters. GET with a mutation SHALL be rejected before send with `graphql: mutations cannot use GET`.

#### Scenario: POST query
- **WHEN** a query is sent with default settings
- **THEN** the request SHALL be a POST with `Content-Type: application/json` and a JSON body containing `query`

#### Scenario: GET mutation rejected
- **GIVEN** `method: GET` on a document whose selected operation is a mutation
- **WHEN** the user sends it
- **THEN** no request SHALL be sent and the error `graphql: mutations cannot use GET` SHALL be shown

### Requirement: Operation selection
When the document contains several operations, `operation_name` SHALL select one; if omitted and more than one operation exists, the send SHALL fail with `graphql: document has N operations; set operation_name`. A single anonymous or named operation SHALL be sent without `operation_name`.

#### Scenario: Ambiguous document
- **GIVEN** a document with operations `A` and `B` and no `operation_name`
- **WHEN** the user sends it
- **THEN** the error `graphql: document has 2 operations; set operation_name` SHALL be shown

### Requirement: Schema introspection and cache
`rustman graphql introspect` and the TUI refresh action SHALL run the standard introspection query (including `isDeprecated`, `deprecationReason`, `specifiedByURL` when supported) and store the result in `.rustman/graphql/schemas/<endpoint-hash>.json` with the endpoint URL, fetch time and SDL hash. The cache SHALL be used until refreshed; it SHALL NOT be refreshed implicitly on send.

#### Scenario: First introspection
- **GIVEN** no cached schema for `https://api.example.com/graphql`
- **WHEN** the user runs `rustman graphql introspect https://api.example.com/graphql`
- **THEN** a cache file SHALL be written and the command SHALL print the type count and fetch time

#### Scenario: Cache reused
- **GIVEN** a cached schema
- **WHEN** a request to that endpoint is validated
- **THEN** no introspection request SHALL be sent

### Requirement: Schema from file
A graphql request or `rustman.toml` endpoint entry MAY set `schema_file` pointing to an SDL (`.graphql`/`.graphqls`) or introspection JSON file; when set, it SHALL take precedence over the introspection cache. Introspection failures (e.g. disabled in production) SHALL report `graphql: introspection failed: <reason>; set schema_file to validate offline`.

#### Scenario: Introspection disabled
- **GIVEN** a server that rejects introspection
- **WHEN** the user runs `rustman graphql introspect`
- **THEN** the command SHALL exit non-zero with the `set schema_file` hint

#### Scenario: SDL file used
- **GIVEN** `schema_file: schema.graphql`
- **WHEN** the request is validated
- **THEN** validation SHALL use the SDL types and SHALL NOT read the cache

### Requirement: Pre-send validation
Before sending, when a schema is available, the system SHALL parse and validate the selected operation against the GraphQL specification validation rules and report every error with message and `line:column`. Validation errors SHALL block the send unless the user passes `--no-validate` (CLI) or confirms in the TUI. Without a schema, the system SHALL only check syntax.

#### Scenario: Unknown field
- **GIVEN** a cached schema where `User` has no field `emial`
- **WHEN** the user sends `{ user(id: 1) { emial } }`
- **THEN** the send SHALL be blocked with `Cannot query field "emial" on type "User"` at `1:17`

#### Scenario: Syntax only without schema
- **GIVEN** no schema is available
- **WHEN** the user sends a document with a missing closing brace
- **THEN** a syntax error with its location SHALL be reported and no request SHALL be sent

### Requirement: Deprecation warnings
Selecting a field or enum value marked deprecated in the schema SHALL produce a non-blocking warning including the `deprecationReason`.

#### Scenario: Deprecated field used
- **GIVEN** `User.username` is deprecated with reason `Use handle`
- **WHEN** a document selecting `username` is validated
- **THEN** a warning `User.username is deprecated: Use handle` SHALL be shown and the send SHALL proceed

### Requirement: Schema explorer data
The engine SHALL expose, from the active schema, the root operation types, every named type with kind and description, fields with arguments, argument types and defaults, enum values, input fields, interfaces/possible types, directives, and deprecation status, for the TUI explorer and the agent interface.

#### Scenario: Browse a type
- **GIVEN** a cached schema
- **WHEN** the explorer requests type `User`
- **THEN** it SHALL receive every field of `User` with type signature, arguments and deprecation status

### Requirement: Completion data
Given a document and a cursor position, the engine SHALL return context-aware completion candidates: fields of the enclosing type, arguments of the enclosing field, enum values for enum-typed positions, variable names declared by the operation, directive names, and fragment names, each with its type signature.

#### Scenario: Field completion
- **GIVEN** the cursor inside `{ user(id: 1) { | } }`
- **WHEN** completion is requested
- **THEN** candidates SHALL be the fields of the `user` field's return type

#### Scenario: Argument completion
- **GIVEN** the cursor at `user(|)`
- **WHEN** completion is requested
- **THEN** candidates SHALL be the arguments of `Query.user`

### Requirement: Variables validation
When a schema is available, the variables map SHALL be checked against the operation's variable definitions: missing required (non-null without default) variables and values of the wrong scalar/enum/input shape SHALL block the send with a message naming the variable path.

#### Scenario: Missing required variable
- **GIVEN** `query($id: ID!) { user(id: $id) { name } }` and empty variables
- **WHEN** the user sends it
- **THEN** the send SHALL be blocked with `variable $id of type ID! is required`

### Requirement: Response rendering with partial data
The system SHALL parse GraphQL responses and present `data`, `errors` (message, locations, path, extensions) and `extensions` separately. A response with both `data` and `errors` SHALL be shown as partial success. The HTTP status and timings from `http-requests` SHALL also be shown.

#### Scenario: Partial data
- **GIVEN** a 200 response with `data.user.name` and `errors[0].path = ["user","email"]`
- **WHEN** it is rendered
- **THEN** the data SHALL be shown and the error SHALL be listed with path `user.email` and the result marked `partial`

#### Scenario: Non-GraphQL body
- **GIVEN** a 502 with an HTML body
- **WHEN** it is rendered
- **THEN** the raw body SHALL be shown with `graphql: response is not a GraphQL result`

### Requirement: Response body limit over HTTP
A GraphQL HTTP response body larger than `[http] max_body_bytes` (default 10 MiB) SHALL be refused as soon as the limit is passed, without being parsed, spilled to disk or stored, with the error `HTTP: the response body is larger than the <n> byte limit (max_body_bytes)` (exit-code class 3). The request SHALL NOT be sent again (no APQ retry), and history SHALL record the error without the body. A body of exactly `max_body_bytes` SHALL be accepted.

#### Scenario: Oversized result refused
- **GIVEN** `[http] max_body_bytes = 4096` and an endpoint answering with a 64 KiB GraphQL result
- **WHEN** the operation is sent
- **THEN** the request SHALL be errored with `HTTP: the response body is larger than the 4096 byte limit (max_body_bytes)`, only one request SHALL be sent, and neither the history entry nor `.rustman/bodies/` SHALL contain the body

#### Scenario: Result at the limit accepted
- **GIVEN** `[http] max_body_bytes` equal to the size of the response body
- **WHEN** the operation is sent
- **THEN** the result SHALL be parsed and shown

### Requirement: GraphQL assertions
The `assertions` capability SHALL support graphql-specific checks: `no_errors`, `errors_count` (comparison), `error_message_contains`, `error_code` (`extensions.code`), and `data` JSONPath checks rooted at `data`. Captures SHALL be able to read from `data` paths.

#### Scenario: no_errors fails
- **GIVEN** assertion `no_errors` and a response with one error
- **WHEN** the request runs under `rustman run`
- **THEN** the assertion SHALL fail and report the error message and path

#### Scenario: Capture from data
- **GIVEN** capture `token: $.data.login.token`
- **WHEN** the login mutation succeeds
- **THEN** variable `token` SHALL be set for later requests in the run

### Requirement: Subscriptions over WebSocket
For `transport: ws`, the system SHALL use the `graphql-transport-ws` subprotocol: `connection_init` (with `connection_params` from the request file), `subscribe`, and `complete` on stop. Each `next` payload SHALL be recorded as a timestamped event. Connection ack SHALL time out after 10 s by default with `graphql: connection_ack timeout`.

#### Scenario: Receive events
- **GIVEN** a subscription to `onMessage`
- **WHEN** the server sends three `next` messages
- **THEN** three timestamped events SHALL be shown in order

#### Scenario: Ack timeout
- **GIVEN** a server that never sends `connection_ack`
- **WHEN** 10 s elapse
- **THEN** the subscription SHALL stop with `graphql: connection_ack timeout`

### Requirement: Legacy WebSocket protocol detection
If the server only accepts the `graphql-ws` subprotocol (subscriptions-transport-ws), the system SHALL fall back to it automatically and SHALL display that the legacy protocol is in use. `ws_protocol` (`auto` default, `graphql-transport-ws`, `graphql-ws`) SHALL force a protocol.

#### Scenario: Legacy server
- **GIVEN** a server negotiating only `graphql-ws`
- **WHEN** a subscription starts with `ws_protocol: auto`
- **THEN** the subscription SHALL work and the UI SHALL show `legacy subscriptions-transport-ws`

### Requirement: Subscriptions over SSE
For `transport: sse`, the system SHALL follow the GraphQL over SSE distinct-connections mode: POST the operation with `Accept: text/event-stream` and record each `next` event until `complete` or user stop.

#### Scenario: SSE stream
- **GIVEN** an SSE-capable server
- **WHEN** a subscription is sent with `transport: sse`
- **THEN** each `next` event SHALL be recorded and the stream SHALL end on `complete`

### Requirement: Subscription limits in headless runs
Under `rustman run`, a subscription SHALL stop after `max_events` (default 10) events or `duration` (default 30 s), whichever comes first, and assertions SHALL evaluate over the collected events. Interactive subscriptions SHALL run until stopped.

#### Scenario: Headless stop
- **GIVEN** a subscription request without limits
- **WHEN** it runs in `rustman run` and the server emits 50 events
- **THEN** collection SHALL stop at 10 events and the step SHALL complete

### Requirement: Multipart file uploads
Variables MAY reference files with `{ "$file": "path" }`. When any are present, the system SHALL send a `multipart/form-data` request per the graphql-multipart-request-spec (`operations`, `map`, file parts) with each file's MIME type guessed from its extension. A missing file SHALL fail before send with `graphql: upload file not found: <path>`.

#### Scenario: Single upload
- **GIVEN** `variables: { file: { $file: "./avatar.png" } }`
- **WHEN** the mutation is sent
- **THEN** the request SHALL be multipart with `operations`, `map` `{"0":["variables.file"]}` and part `0` of type `image/png`

### Requirement: Automatic Persisted Queries
With `apq: true`, the system SHALL first send only `extensions.persistedQuery` (`version: 1`, `sha256Hash` of the document). On a `PersistedQueryNotFound` error it SHALL retry once with the full document and the hash. The UI SHALL show whether the hash hit or missed.

#### Scenario: APQ miss then register
- **GIVEN** `apq: true` and a server that does not know the hash
- **WHEN** the query is sent
- **THEN** two requests SHALL be made and the result SHALL be marked `APQ miss (registered)`

### Requirement: GraphQL CLI commands
`rustman graphql introspect <endpoint|request-file> [--out schema.graphql]` SHALL fetch and cache the schema (optionally writing SDL). `rustman graphql query <request-file | --endpoint URL --query TEXT> [--var k=v] [--env name] [--no-validate] [--json]` SHALL validate, send and print the result; exit code SHALL be 0 on no errors, 1 when `errors` is non-empty, and the `cli-runner` codes for transport failures.

#### Scenario: Query with errors
- **WHEN** `rustman graphql query users.rman.yaml` returns partial data with errors
- **THEN** the data and errors SHALL be printed and the exit code SHALL be 1

#### Scenario: Export SDL
- **WHEN** the user runs `rustman graphql introspect https://x/graphql --out schema.graphql`
- **THEN** `schema.graphql` SHALL contain the schema in SDL

### Requirement: History for GraphQL exchanges
Every executed GraphQL operation SHALL be recorded by `request-history` with operation name, operation type, variables (redacted per `environments`), response summary (`ok` / `partial` / `errors`) and, for subscriptions, the event count and the first 100 events.

#### Scenario: Subscription in history
- **GIVEN** a subscription that received 3 events
- **WHEN** it is stopped
- **THEN** the history entry SHALL show type `subscription`, 3 events and the event payloads

### Requirement: TUI GraphQL pane
When a graphql request is open, the TUI SHALL show a document editor with syntax highlighting, inline validation diagnostics and completion popup, a variables editor (JSON), a schema explorer panel, and a result view with tabs `Data`, `Errors`, `Extensions`, `Events` (subscriptions). Layout, keymap and theming follow `add-terminal-ui`.

#### Scenario: Inline diagnostic
- **GIVEN** a cached schema
- **WHEN** the user types an unknown field
- **THEN** the field SHALL be underlined and the error shown in the diagnostics line without sending
