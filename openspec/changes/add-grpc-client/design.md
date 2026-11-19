# Design

## Context

`bootstrap-core-engine` provides the workspace loader with its kind registry, variable resolution and redaction, assertions, history and `rustman run`. `http-requests` owns TLS configuration (rustls, custom CA, mTLS) and proxy settings. `add-terminal-ui` provides the protocol pane extension point. This change adds a `rustman-grpc` crate that plugs into those seams. See proposal.md for the motivation.

## Goals / Non-Goals

**Goals:**
- Dynamic calls with no generated code. Rustman works from descriptors discovered at runtime.
- No `protoc`, no system libraries: `cargo build` stays enough.
- Streaming calls are first-class, and testable headlessly.

**Non-Goals:**
- Client code generation, gRPC server mocking, or load testing (those belong to other capabilities).

## Decisions

### D1. Transport: `tonic` with a dynamic codec (rejected: `grpcio`)
`tonic` is pure Rust on hyper and works with rustls. We use its generic `Grpc<Channel>` client with a custom `Codec` that encodes and decodes `prost_reflect::DynamicMessage`, so no per-service generated code is needed. `grpcio` wraps the C-core library, which would break the "no system libraries" rule.

### D2. Dynamic messages: `prost-reflect` (rejected: hand-written descriptor walking)
`prost-reflect` provides `DescriptorPool`, `DynamicMessage` and the proto3 JSON mapping (through its `serde` feature), including well-known types. Writing that ourselves would just reproduce a well-tested library.

### D3. `.proto` compilation: `protox` (rejected: shelling out to `protoc`, `protobuf-parse`)
`protox` compiles `.proto` files to `FileDescriptorSet` in pure Rust with line/column errors, and feeds `prost-reflect` directly. Imports are opened through a resolver that refuses files resolving (through symlinks) outside their import path. Shelling out would make `protoc` a runtime dependency. `protobuf-parse` belongs to the `rust-protobuf` ecosystem, which is a poorer fit with prost.

### D4. Reflection client
We use the `tonic-reflection` client protos for v1 and v1alpha. We fetch `list_services`, then `file_containing_symbol` for each service, following dependencies transitively into a `DescriptorPool`. The serialized `FileDescriptorSet` is cached at `.rustman/grpc/descriptors/<sha256(target)>.bin` with a JSON sidecar recording when it was fetched. Reflection, connecting included, is bounded by the run's timeout, and a schema is limited to 4096 files and 64 MiB of descriptors, so a server that stalls or names dependencies without end cannot hold or exhaust the client.

### D5. TLS reuses the `http-requests` configuration
A shared `TlsSettings` → `rustls::ClientConfig` builder in `rustman-http` is reused, with ALPN `h2`. That keeps CA, mTLS and the insecure flag behaving the same across protocols. Plaintext uses tonic's h2c endpoint.

### D6. gRPC-Web goes over `rustman-http` (rejected: `tonic-web` client)
gRPC-Web is plain HTTP framing (a 5-byte prefix per message, plus trailers in the body). Implementing it as a small framing layer over `rustman-http` gives us proxies, timings and cookies for free. `tonic-web` targets the server side. A frame longer than `[http] max_body_bytes` is refused from its length prefix (`RESOURCE_EXHAUSTED`), as native calls are bounded by tonic's 4 MiB decode limit.

### D7. Exchange model
A gRPC exchange extends the core exchange with `protocol: grpc`, `call_type`, `status {code, name, message, details[]}`, `headers`, `trailers` and `timeline[]`. JSONPath assertions see the unary response at `$` and streams at `$.messages[*]`. That's how the runner can evaluate all kinds uniformly.

### D8. Error details
`grpc-status-details-bin` is decoded with `prost-reflect` using a bundled descriptor set for `google/rpc/*.proto` (`status`, `error_details`), embedded at build time through `protox` in `build.rs`.

### D9. Agent calls: the proto marks reads (rejected: every call a read, a method glob list)
Every gRPC call is an HTTP/2 `POST`, so the HTTP method rule of agents cannot classify it. `grpc_call` reads `google.protobuf.MethodOptions.idempotency_level` instead: `NO_SIDE_EFFECTS` is a read, anything else is a mutation and goes through the same gate as an HTTP `POST` (`[agent] allow_mutations`, then the human's confirmation). The classification comes from the server's own schema, so no new setting is needed; a method glob list like `[jsonrpc] agent_read_only_methods` can be added later if unannotated servers need it. The runner checks the confirmed method again before sending.

## Risks / Trade-offs

- [Version drift of tonic, prost, prost-reflect and protox] → "Pinned" follows the workspace convention: caret requirements in `[workspace.dependencies]`, exact versions recorded in `Cargo.lock`, and CI builds with `--locked`.
- [Reflection implementations differ, e.g. missing dependencies in `file_containing_symbol`] → Fall back to `file_by_filename` for unresolved imports. Integration tests run against tonic and grpc-go reflection servers (the latter in a container, CI only).
- [Large descriptor pools slow down TUI startup] → Load lazily per target, and keep the cache on disk.
- [Bidi streams in the TUI make state complex] → One background task per call, with channels to the UI. Cancel through the same token model as `tui` HTTP sends.
- [int64 JSON mapping confuses users] → Accept both string and number on input. Output uses strings, as the proto3 JSON spec says.

## Migration Plan

Additive. Workspaces without `kind: grpc` files are unaffected.

## Open Questions

- Should descriptor caches be shareable through Git (a committed `grpc/descriptors/`) for teams without reflection in production? This can be added later without changing the specs.
