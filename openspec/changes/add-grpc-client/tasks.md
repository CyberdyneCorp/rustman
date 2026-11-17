# Tasks

## 1. Crate and request model

- [ ] 1.1 Create `crates/rustman-grpc` with `tonic`, `prost`, `prost-reflect` (serde feature) and `protox` pinned. Verify that `cargo build -p rustman-grpc` passes on Linux, macOS and Windows CI with no `protoc` installed.
- [ ] 1.2 Register kind `grpc` in the workspace kind registry and parse the request file (target, method, message/messages, metadata, deadline_ms, tls, transport, schema, stream). Verify with unit tests for a valid file, the `exactly one of message or messages` error, and the unregistered-kind list containing `grpc`.
- [ ] 1.3 Interpolate variables in target, metadata and message string values, and redact secrets in recorded metadata. Verify with a test where history stores `Bearer «redacted:token»`.

## 2. Schema sources and cache

- [ ] 2.1 Reflection client (v1 with fallback to v1alpha), resolving dependencies transitively into a `DescriptorPool`. Verify with integration tests against an in-process tonic server exposing v1 only, v1alpha only, or no reflection (expected error, exit code 2).
- [ ] 2.2 Compile local `.proto` files with `protox` (import paths, workspace-root containment), plus descriptor-set loading. Verify with tests for a successful compile, a `file:line:col` syntax error, and a rejected `../` path escape.
- [ ] 2.3 Descriptor cache in `.rustman/grpc/descriptors/` with refresh. Verify with a test that browsing works with the server stopped and shows the stale notice.
- [ ] 2.4 Service/method/message browser API and template generation (depth 3, defaults). Verify with `insta` snapshots of templates for nested, repeated, enum, oneof and map fields.

## 3. Message encoding and calls

- [ ] 3.1 Dynamic codec for `DynamicMessage`, plus the proto3 JSON mapping including well-known types and unknown-field rejection. Verify with tests for the Timestamp scenario and the `unknown field 'idd'` error raised before any network I/O.
- [ ] 3.2 Unary call with headers, trailers, status and timings. Verify with a test against an in-process server asserting the exchange shape.
- [ ] 3.3 Server, client and bidi streaming with the timeline. Verify with tests for 3 server messages, 4 client messages plus half-close, and a 2-sent/3-received bidi timeline.
- [ ] 3.4 Headless stream bounds (100 messages / 30 s) with cancellation. Verify with a test against an endless stream that stops at 100 and cancels the call.
- [ ] 3.5 Deadlines (`grpc-timeout`) with the default chain. Verify that a `DEADLINE_EXCEEDED` test makes `rustman run` exit 3.
- [ ] 3.6 Metadata, including `-bin` base64 handling. Verify with a test where the server receives bytes `01 02 03`.

## 4. Transport and errors

- [ ] 4.1 TLS, mTLS and h2c through the shared `TlsSettings` builder. Verify with tests for an h2c local server, a self-signed rejection, success with a custom CA, and mTLS with a client certificate.
- [ ] 4.2 gRPC-Web framing over `rustman-http` for unary and server streaming, rejecting other call types. Verify with tests against a tonic-web server and the bidi rejection error.
- [ ] 4.3 Decode `google.rpc.Status` details using an embedded descriptor set. Verify with tests rendering `BadRequest`, `ErrorInfo` and an unknown detail type (type URL plus base64).

## 5. Assertions, runner, history

- [ ] 5.1 Assertion types `grpc_status` and `metadata`, JSONPath over unary and `$.messages`, and captures from json and metadata. Verify with tests for the `expected OK, got NOT_FOUND (5)` message and the streamed-length assertion.
- [ ] 5.2 Runner and history integration with `protocol: grpc`. Verify with an end-to-end test of an HTTP login capture feeding gRPC metadata, with both exchanges in history.
- [ ] 5.3 Document the `kind: grpc` request file format in `docs/request-files.md`, with examples for unary and streaming calls. Verify with the docs example files loading in a test.

## 6. CLI, TUI, agent tools

- [ ] 6.1 `rustman grpc list|describe|call|health` with `--json` JSON-lines output and exit codes. Verify with CLI tests (assert_cmd), including health exit codes 0, 1 and 3.
- [ ] 6.2 TUI gRPC pane: method browser, message editor seeded from the template, metadata tab, status and details view, and a timeline with direction and text filter, plus interactive bidi send and half-close. Verify with ratatui `TestBackend` snapshot tests.
- [ ] 6.3 Conditional agent tools `grpc_list`, `grpc_describe` and `grpc_call` (unary and server streaming only) through the safety policy. Verify with tests for a blocked host, a redacted result, and the absence of the tools when the agent interface isn't built.
- [ ] 6.4 Add a gRPC section to the README and CLI help. Verify that `rustman grpc --help` lists all subcommands.
