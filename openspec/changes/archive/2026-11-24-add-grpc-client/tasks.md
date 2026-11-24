# Tasks

## 1. Crate and request model

- [x] 1.1 Create `crates/rustman-grpc` with `tonic`, `prost`, `prost-reflect` (serde feature) and `protox` pinned. Verify that `cargo build -p rustman-grpc` passes on Linux, macOS and Windows CI with no `protoc` installed.
- [x] 1.2 Register kind `grpc` in the workspace kind registry and parse the request file (target, method, message/messages, metadata, deadline_ms, tls, transport, schema, stream). Verify with unit tests for a valid file, the `exactly one of message or messages` error, and the unregistered-kind list containing `grpc`.
- [x] 1.3 Interpolate variables in target, metadata and message string values, and redact secrets in recorded metadata. Verify with a test where history stores `Bearer «redacted:token»`.

## 2. Schema sources and cache

- [x] 2.1 Reflection client (v1 with fallback to v1alpha), resolving dependencies transitively into a `DescriptorPool`. Verify with integration tests against an in-process tonic server exposing v1 only, v1alpha only, or no reflection (expected error, exit code 2).
- [x] 2.2 Compile local `.proto` files with `protox` (import paths, workspace-root containment), plus descriptor-set loading. Verify with tests for a successful compile, a `file:line:col` syntax error, and a rejected `../` path escape.
- [x] 2.3 Descriptor cache in `.rustman/grpc/descriptors/` with refresh. Verify with a test that browsing works with the server stopped and shows the stale notice.
- [x] 2.4 Service/method/message browser API and template generation (depth 3, defaults). Verify with `insta` snapshots of templates for nested, repeated, enum, oneof and map fields.

## 3. Message encoding and calls

- [x] 3.1 Dynamic codec for `DynamicMessage`, plus the proto3 JSON mapping including well-known types and unknown-field rejection. Verify with tests for the Timestamp scenario and the `unknown field 'idd'` error raised before any network I/O.
- [x] 3.2 Unary call with headers, trailers, status and timings. Verify with a test against an in-process server asserting the exchange shape.
- [x] 3.3 Server, client and bidi streaming with the timeline. Verify with tests for 3 server messages, 4 client messages plus half-close, and a 2-sent/3-received bidi timeline.
- [x] 3.4 Headless stream bounds (100 messages / 30 s) with cancellation. Verify with a test against an endless stream that stops at 100 and cancels the call.
- [x] 3.5 Deadlines (`grpc-timeout`) with the default chain. Verify that a `DEADLINE_EXCEEDED` test makes `rustman run` exit 3.
- [x] 3.6 Metadata, including `-bin` base64 handling. Verify with a test where the server receives bytes `01 02 03`.

## 4. Transport and errors

- [x] 4.1 TLS, mTLS and h2c through the shared `TlsSettings` builder. Verify with tests for an h2c local server, a self-signed rejection, success with a custom CA, and mTLS with a client certificate.
- [x] 4.2 gRPC-Web framing over `rustman-http` for unary and server streaming, rejecting other call types. Verify with tests against a tonic-web server and the bidi rejection error.
- [x] 4.3 Decode `google.rpc.Status` details using an embedded descriptor set. Verify with tests rendering `BadRequest`, `ErrorInfo` and an unknown detail type (type URL plus base64).

## 5. Assertions, runner, history

- [x] 5.1 Assertion types `grpc_status` and `metadata`, JSONPath over unary and `$.messages`, and captures from json and metadata. Verify with tests for the `expected OK, got NOT_FOUND (5)` message and the streamed-length assertion.
- [x] 5.2 Runner and history integration with `protocol: grpc`. Verify with an end-to-end test of an HTTP login capture feeding gRPC metadata, with both exchanges in history.
- [x] 5.3 Document the `kind: grpc` request file format in `docs/request-files.md`, with examples for unary and streaming calls. Verify with the docs example files loading in a test.

## 6. CLI, TUI, agent tools

- [x] 6.1 `rustman grpc list|describe|call|health` with `--json` JSON-lines output and exit codes. Verify with CLI tests (assert_cmd), including health exit codes 0, 1 and 3.
- [x] 6.2 TUI gRPC pane: method browser, message editor seeded from the template, metadata tab, status and details view, and a timeline with direction and text filter, plus interactive bidi send and half-close. Verify with ratatui `TestBackend` snapshot tests.
  - Note: `crates/rustman-tui/src/grpc_pane.rs` (editor: Method browser, Message seeded from the template, Metadata, Assertions), `grpc_view.rs`, `grpc.rs` (schema loads off the render loop through the descriptor cache, `Ctrl+G` refresh, `Ctrl+R` sends the Message tab on a running bidi stream, `Alt+X` half-closes, live timeline), `response/grpc.rs` (`Messages`, `Status` with decoded details, headers and trailers, `Timeline` filtered by direction with `d` and by text with `f`). Interactive streams use `rustman_grpc::Interactive` (redacted live feed, typed messages until half-close). Snapshots in `grpc_tests.rs`: `grpc_method_browser`, `grpc_message_template`, `grpc_status_details`, `grpc_live_timeline`, `grpc_timeline_filtered`; offline browsing with the stale notice; a metadata secret never drawn.
- [x] 6.3 Conditional agent tools `grpc_list`, `grpc_describe` and `grpc_call` (unary and server streaming only) through the safety policy. Verify with tests for a blocked host, a redacted result, and the absence of the tools when the agent interface isn't built.
  - Note: `rustman_grpc::agent::AgentGrpc` checks the target as its `http(s)` origin against the session policy before any traffic (reflection included), paces every reach on the session governor, refuses client-streaming and bidi methods, and treats any method not marked `idempotency_level = NO_SIDE_EFFECTS` as a mutation (`[agent] allow_mutations` plus the human's confirmation, design D9); `rustman-mcp` registers `grpc_list`, `grpc_describe` and `grpc_call` (`server/tools/grpc.rs`) behind its default `grpc` feature. Tests: `crates/rustman-mcp/tests/grpc.rs` (blocked host refused and audited, redacted result, list/describe, bidi refusal, mutations refused by default, confirmation required, allowed unattended), the gRPC calls in `tests/canary.rs`, and `tools::tests::the_grpc_tools_are_absent_without_the_grpc_feature` (`cargo test -p rustman-mcp --no-default-features --lib tools::tests`, a CI step on every OS: Cargo feature unification keeps `grpc` on in `cargo test --workspace`).
- [x] 6.4 Add a gRPC section to the README and CLI help. Verify that `rustman grpc --help` lists all subcommands.
