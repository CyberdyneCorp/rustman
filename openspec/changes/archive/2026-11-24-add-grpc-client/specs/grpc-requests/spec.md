# Spec Delta

## Purpose

Lets users discover gRPC services and execute unary and streaming calls from saved request files, the CLI and the TUI, with schema-aware message editing, decoded error details, assertions and health checks.

## ADDED Requirements

### Requirement: gRPC request file
A request file with `kind: grpc` SHALL accept `target` (`host:port` or a `grpc://`, `grpcs://` URL), `method` (fully qualified, `package.Service/Method`), `message` (one message) or `messages` (a list, for client and bidi streaming), `metadata`, `deadline_ms`, `tls`, `schema` and `stream` limits. Exactly one of `message` or `messages` SHALL be present.

#### Scenario: Valid unary request file
- **GIVEN** a file with `kind: grpc`, `target: "{{grpc_host}}"`, `method: shop.v1.Catalog/GetProduct` and `message: {id: "p-1"}`
- **WHEN** the workspace loads
- **THEN** the request SHALL load as a unary-capable gRPC request

#### Scenario: Both message fields
- **GIVEN** a file with both `message` and `messages`
- **WHEN** the workspace loads
- **THEN** loading SHALL fail with `grpc: exactly one of message or messages is allowed`

### Requirement: Kind registration
This change SHALL register the request kind `grpc` in the `workspace` kind registry and a `grpc` pane through the `tui` protocol pane extension point. Before this change is present, `kind: grpc` files SHALL be reported by the loader as an unregistered kind.

#### Scenario: Kind listed
- **GIVEN** a build that includes this change
- **WHEN** a request file has an unknown kind
- **THEN** the error's list of registered kinds SHALL include `grpc`

### Requirement: Variable interpolation
`{{variable}}` placeholders SHALL be resolved in `target`, metadata values and string values inside messages, using `environments` precedence. Secret values used in metadata SHALL be redacted in history, reports and the TUI per the `environments` redaction contract.

#### Scenario: Token in metadata
- **GIVEN** metadata `authorization: "Bearer {{token}}"` where `token` is a secret
- **WHEN** the call is sent and recorded
- **THEN** the server SHALL receive the real token and history SHALL store `Bearer «redacted:token»`

### Requirement: Server reflection
When no `schema` is configured, the system SHALL discover services using `grpc.reflection.v1.ServerReflection`, falling back to `grpc.reflection.v1alpha` when v1 returns `UNIMPLEMENTED`. If both are unimplemented, it SHALL fail with `grpc: server reflection unavailable at <target>; set schema.protos or schema.descriptor_set`.

#### Scenario: v1alpha fallback
- **GIVEN** a server that only implements `v1alpha` reflection
- **WHEN** the user runs `rustman grpc list <target>`
- **THEN** the services SHALL be listed

#### Scenario: No reflection
- **GIVEN** a server without reflection and no `schema`
- **WHEN** a call is attempted
- **THEN** it SHALL fail with the `server reflection unavailable` error and exit code 2

### Requirement: Local schema sources
`schema.protos` (a list of `.proto` files) with `schema.import_paths` SHALL be compiled in-process without invoking `protoc`. `schema.descriptor_set` SHALL load a binary `FileDescriptorSet`. Compile errors SHALL be reported as `<file>:<line>:<col>: <message>`. Paths SHALL be resolved relative to the request file and SHALL NOT escape the workspace root.

#### Scenario: Compile proto files
- **GIVEN** `schema.protos: [protos/shop/v1/catalog.proto]` and `import_paths: [protos]`
- **WHEN** a call to `shop.v1.Catalog/GetProduct` is prepared
- **THEN** the message types SHALL be resolved from the compiled files with no `protoc` binary on the PATH

#### Scenario: Proto syntax error
- **GIVEN** a `.proto` file with a missing semicolon on line 7
- **WHEN** it is compiled
- **THEN** the error SHALL start with `catalog.proto:7:`

### Requirement: Schema cache
Descriptors obtained by reflection SHALL be cached under `.rustman/grpc/descriptors/` keyed by target, and reused until refreshed with `rustman grpc list --refresh` or the TUI refresh action. A cached schema SHALL let the TUI browse and template messages offline.

#### Scenario: Offline browsing
- **GIVEN** descriptors were cached for a target that is now unreachable
- **WHEN** the user opens the method browser
- **THEN** services and methods SHALL be listed from the cache, with a notice that the data may be stale

### Requirement: Service and method browser
The system SHALL list services, methods (with call type: unary, server streaming, client streaming, bidi) and message types, and SHALL generate a message template with every field set to its default (nested messages expanded to depth 3, repeated fields as one-element lists, enums as their first value name).

#### Scenario: Describe a method
- **WHEN** the user runs `rustman grpc describe <target> shop.v1.Catalog/ListProducts`
- **THEN** the output SHALL show call type `server streaming`, the request and response message types, and a request template

### Requirement: Message mapping
Request messages SHALL be written as YAML or JSON and encoded using the proto3 JSON mapping (field names in lowerCamelCase or original, enums by name or number, `int64` as string or number, well-known types such as `Timestamp` and `Duration` in their canonical JSON form). Unknown fields SHALL fail before sending with `grpc: unknown field '<name>' in <MessageType>`.

#### Scenario: Unknown field
- **GIVEN** a message `{idd: "p-1"}` for `GetProductRequest`
- **WHEN** the call is sent
- **THEN** it SHALL fail before any network traffic with `grpc: unknown field 'idd' in shop.v1.GetProductRequest`

#### Scenario: Well-known types
- **GIVEN** a field of type `google.protobuf.Timestamp` set to `"2026-11-17T10:00:00Z"`
- **WHEN** the message is encoded
- **THEN** it SHALL encode seconds `1794909600` and nanos `0`

### Requirement: Unary calls
A unary call SHALL send one message and produce one exchange containing the response message (as proto3 JSON), response headers and trailers (metadata), the gRPC status, and timings (connect, TLS, first response byte, total).

#### Scenario: Successful unary call
- **GIVEN** a reachable server and a valid `GetProduct` request
- **WHEN** the call is sent
- **THEN** the exchange SHALL show status `OK (0)`, the response message as JSON, and the trailers

### Requirement: Streaming calls
Server streaming SHALL collect each response message in order. Client streaming SHALL send each entry of `messages` in order, then half-close. Bidi streaming SHALL send `messages` in order while receiving concurrently; in the TUI the user MAY send further messages interactively before half-closing.

#### Scenario: Server streaming
- **GIVEN** a `ListProducts` call where the server sends 3 messages
- **WHEN** the call runs
- **THEN** the exchange SHALL contain the 3 messages in order and the final status

#### Scenario: Client streaming
- **GIVEN** `messages` with 4 entries for a client-streaming method
- **WHEN** the call runs
- **THEN** the server SHALL receive 4 messages followed by half-close, and the exchange SHALL contain its single response

### Requirement: Message timeline
For streaming calls, the exchange SHALL include a timeline with one entry per message: direction (`sent`/`received`), timestamp relative to call start, encoded size in bytes, and the message as JSON. The TUI SHALL render the timeline with filtering by direction and text.

#### Scenario: Timeline entries
- **GIVEN** a bidi call that sent 2 and received 3 messages
- **WHEN** the exchange is inspected
- **THEN** the timeline SHALL have 5 entries with direction, relative time and size

### Requirement: Headless stream bounds
Under `rustman run`, receiving SHALL stop after `stream.max_messages` (default 100) received messages or `stream.duration_ms` (default 30000), whichever comes first; the call SHALL then be cancelled and assertions SHALL evaluate over the collected messages. Interactive streams SHALL run until the user stops them or the deadline expires.

#### Scenario: Endless stream bounded
- **GIVEN** a server-streaming method that never ends and no `stream` limits
- **WHEN** it runs in `rustman run`
- **THEN** collection SHALL stop at 100 messages, the call SHALL be cancelled, and the step SHALL complete; without assertions the step SHALL pass (the default `grpc_status: OK` expectation does not apply to a stream the client stopped)

### Requirement: Deadlines
Every call SHALL carry a deadline: `deadline_ms` from the request file, else `rustman.toml [defaults] timeout_ms`, else 30000 ms, sent as `grpc-timeout`. An expired deadline SHALL produce status `DEADLINE_EXCEEDED (4)` and count as a request error in `cli-runner` exit codes.

#### Scenario: Deadline exceeded
- **GIVEN** `deadline_ms: 200` and a server that takes 2 s
- **WHEN** the call runs in `rustman run`
- **THEN** the status SHALL be `DEADLINE_EXCEEDED (4)` and the run SHALL exit with code 3

### Requirement: Metadata
`metadata` SHALL be an ordered list of name/value pairs sent as request headers. Names ending in `-bin` SHALL take base64 values and be sent as binary metadata. Response headers and trailers SHALL be recorded separately and binary values shown as base64.

#### Scenario: Binary metadata
- **GIVEN** metadata `{name: trace-bin, value: "AQID"}`
- **WHEN** the call is sent
- **THEN** the server SHALL receive the binary value `0x01 0x02 0x03`

### Requirement: Transport security
`grpcs://` targets and targets with `tls.enabled: true` (the default for bare `host:port` on port 443) SHALL use TLS with verification on and the same CA, client-certificate (mTLS) and `insecure` options as `http-requests`. `grpc://` or `tls.enabled: false` SHALL use plaintext HTTP/2 (h2c).

#### Scenario: Plaintext local server
- **GIVEN** `target: grpc://localhost:50051`
- **WHEN** a call is sent
- **THEN** it SHALL use h2c without TLS

#### Scenario: Untrusted certificate
- **GIVEN** a `grpcs://` target with a self-signed certificate and no custom CA
- **WHEN** a call is sent
- **THEN** it SHALL fail with a TLS verification error naming the target

### Requirement: gRPC-Web
`transport: grpc-web` SHALL send calls using the gRPC-Web protocol (`application/grpc-web+proto`) over HTTP/1.1 or HTTP/2 through `http-requests`. Client streaming and bidi SHALL be rejected with `grpc: grpc-web supports only unary and server-streaming calls`.

#### Scenario: Bidi over gRPC-Web
- **GIVEN** `transport: grpc-web` and a bidi method
- **WHEN** the call is prepared
- **THEN** it SHALL fail before sending with the grpc-web limitation error

### Requirement: Status and error details
A non-OK call SHALL show the status code name and number and the status message. When the `grpc-status-details-bin` trailer is present, it SHALL be decoded as `google.rpc.Status` and its details rendered by type: `ErrorInfo`, `BadRequest` (field violations), `RetryInfo`, `QuotaFailure`, `PreconditionFailure`, `ResourceInfo`, `Help`, `LocalizedMessage`, `DebugInfo`; unknown types SHALL be shown as type URL plus base64.

#### Scenario: BadRequest details
- **GIVEN** a server returning `INVALID_ARGUMENT` with a `BadRequest` detail for field `id`
- **WHEN** the exchange is shown
- **THEN** it SHALL display `INVALID_ARGUMENT (3)` and a field violation for `id` with its description

### Requirement: Assertions and captures
The `assertions` capability SHALL apply to gRPC exchanges with: `grpc_status: <name|number>` (default expectation `OK` when no assertions are given), `json` assertions over the response message (unary) or over the list of received messages at `$.messages` (streaming), `metadata` assertions on headers/trailers, and `latency`. Captures SHALL support `from: json` and `from: metadata`.

#### Scenario: Status assertion failure
- **GIVEN** `- grpc_status: OK` and a call returning `NOT_FOUND`
- **WHEN** the run evaluates assertions
- **THEN** the failure SHALL read `grpc_status: expected OK, got NOT_FOUND (5)` and the run SHALL exit with code 1

#### Scenario: Assertion over streamed messages
- **GIVEN** a server-streaming call and `- json: {path: "$.messages", length: 3}`
- **WHEN** 3 messages are received
- **THEN** the assertion SHALL pass

### Requirement: Health check helper
`rustman grpc health <target> [--service <name>]` SHALL call `grpc.health.v1.Health/Check` and print the serving status. It SHALL exit 0 for `SERVING`, 1 for `NOT_SERVING`, `SERVICE_UNKNOWN` or `UNKNOWN`, and 3 when the call errors (including `UNIMPLEMENTED`).

#### Scenario: Not serving
- **GIVEN** a server whose health service reports `NOT_SERVING` for `shop.v1.Catalog`
- **WHEN** the user runs `rustman grpc health <target> --service shop.v1.Catalog`
- **THEN** the output SHALL show `NOT_SERVING` and the exit code SHALL be 1

### Requirement: CLI commands
`rustman grpc list <target|request-file>` SHALL list services and methods. `rustman grpc describe <target> <symbol>` SHALL describe a service, method or message. `rustman grpc call <request-file | --target T --method M --data JSON>` SHALL execute a call and print the response (JSON lines for streams with `--json`). Exit codes SHALL follow `cli-runner`.

#### Scenario: Ad-hoc call with JSON output
- **WHEN** the user runs `rustman grpc call --target grpc://localhost:50051 --method shop.v1.Catalog/ListProducts --data '{}' --json`
- **THEN** each received message SHALL be printed as one JSON line, followed by a final status line

### Requirement: Runner and history integration
`rustman run` SHALL execute `kind: grpc` files in collection order alongside other kinds, and each call SHALL be recorded in `request-history` as an exchange with protocol `grpc`, redacted per the `environments` contract.

#### Scenario: Mixed collection
- **GIVEN** a collection with an `http` login request that captures `token`, followed by a `grpc` request using it in metadata
- **WHEN** `rustman run` executes the collection
- **THEN** the gRPC call SHALL receive the captured token and both exchanges SHALL appear in history

### Requirement: Agent tools
When the build includes `agent-mcp-interface`, the tools `grpc_list` and `grpc_describe` SHALL be registered, plus `grpc_call` for unary and server-streaming methods only. Calls SHALL pass the safety policy's host allowlist and rate limit, and results SHALL be redacted and size-capped per that capability.

#### Scenario: Host outside allowlist
- **GIVEN** an agent calls `grpc_call` with a target not in `[agent].allowed_hosts`
- **WHEN** the call is evaluated
- **THEN** it SHALL be refused with `blocked: host not allowed` and recorded in the audit log

### Requirement: Agent mutation rule
`grpc_call` SHALL treat a method as read-only only when its descriptor sets `idempotency_level = NO_SIDE_EFFECTS`. Any other method SHALL be a mutation, gated by `[agent].allow_mutations` and human confirmation like an HTTP `POST`.

#### Scenario: Unmarked method under default agent settings
- **GIVEN** default `[agent]` settings and a unary method without `idempotency_level = NO_SIDE_EFFECTS`
- **WHEN** an agent calls it with `grpc_call`
- **THEN** it SHALL be refused with `blocked: mutations not allowed for agents` before anything is sent
