# Proposal

## Why

gRPC is the other API style most backend teams run, alongside REST and GraphQL. Today users need a separate tool (grpcurl, BloomRPC, Postman's gRPC beta) with no Git-friendly collections, no assertions, and no CI runner. Rustman already has all three for HTTP. Adding gRPC to the same workspace and runner closes the biggest remaining protocol gap.

## What Changes

- Register request kind `grpc` in the `workspace` kind registry. A gRPC request file names a target, a fully qualified method, metadata, a deadline, TLS settings and one or more request messages written as YAML/JSON (proto3 JSON mapping).
- Schema sources: server reflection (`grpc.reflection.v1` with fallback to `v1alpha`), local `.proto` files with import paths compiled in-process (no `protoc` needed), or binary descriptor sets.
- A service/method browser and message templates generated from the schema.
- All four call types: unary, server streaming, client streaming and bidirectional streaming, with a message timeline.
- Transport: TLS (default), mTLS, plaintext HTTP/2 (`h2c`), and optional gRPC-Web.
- Status handling: gRPC status code and message, plus `google.rpc.Status` details (`ErrorInfo`, `BadRequest`, `RetryInfo`…) decoded and displayed.
- Assertions on the status code and on response messages (JSONPath over the JSON mapping), plus captures. Streaming calls are bounded in headless runs.
- A health-check helper for `grpc.health.v1`.
- CLI: `rustman grpc list`, `rustman grpc describe`, `rustman grpc call`. `rustman run` runs `kind: grpc` files.
- A TUI gRPC pane registered through the `tui` protocol pane extension point.
- When the agent interface is present, read-only gRPC tools are registered conditionally.

## Capabilities

### New Capabilities

- `grpc-requests`: defining, discovering and executing gRPC calls (all four call types) from saved request files, the CLI and the TUI, including schema sources, transport security, status/error-detail decoding, assertions and health checks.

### Modified Capabilities

None. This builds on `workspace` (kind registry), `environments`, `http-requests` (TLS and proxy settings), `assertions`, `cli-runner`, `request-history`, `tui` and `agent-mcp-interface` without changing their requirements.

## Out of Scope

- gRPC server mocking (a later extension of `mock-server`).
- Load testing gRPC (covered by `load-testing`).
- Connect protocol (`application/connect+json`) and Thrift.
- Generating client code or `.proto` files.

## Impact

- New crate `crates/rustman-grpc` (tonic, prost, prost-reflect, protox).
- `rustman-cli` gains the `grpc` subcommand; `rustman-tui` gains the gRPC pane.
- No new system libraries: TLS reuses rustls, and `.proto` compilation is pure Rust.
