# Design

## Context

`bootstrap-core-engine` provides the kind registry, variables, secrets and redaction, assertions, history and `rustman run`. `api-exploration` defines the safety policy (mutation and destructive classes), which `agent-mcp-interface` enforces for agents. `add-grpc-client` brings `prost-reflect`, which we reuse for Protobuf decoding. The project rule is that the default binary needs no system libraries. See proposal.md for the motivation.

## Goals / Non-Goals

**Goals:**
- Inspect and test brokers without side effects by default: no consumer-group membership, no offset commits, no draining of shared queues.
- One request-file shape across the three brokers (`operation`, destination, `messages`, `limits`).
- Pure-Rust clients only, in the default build.

**Non-Goals:**
- Being a production-grade consumer, or doing broker administration.
- Full AsyncAPI coverage: bindings beyond the basics (Kafka partitions and keys, MQTT QoS and retain, AMQP exchange type) are ignored with a warning.

## Decisions

### D1. Kafka: `rskafka` (rejected: `rdkafka`, `kafka-rust`)
`rdkafka` is the most complete client, but it wraps librdkafka, which needs a C toolchain plus CMake, zlib and zstd, and OpenSSL for TLS. That breaks the project's build rule. `rskafka` is pure Rust (tokio, rustls). It offers per-partition produce and fetch, SASL PLAIN/SCRAM and compression, with no consumer-group coordination. That limitation actually matches our side-effect-free consume model (partition-level reads at explicit offsets). `kafka-rust` is unmaintained and lacks modern SASL.

We don't offer an `rdkafka` cargo feature. Consumer groups are out of scope, so a second client would add build complexity for no spec-visible benefit. If transactions or consumer groups are ever needed, a later change can revisit this.

Default partitioning implements murmur2 the way the Java client does, so keyed messages land on the same partitions as with standard producers.

### D2. MQTT: `rumqttc` (rejected: `paho-mqtt`)
`rumqttc` is pure Rust with MQTT 3.1.1 and 5 support, rustls, and WebSocket transport. `paho-mqtt` wraps the Paho C library.

### D3. AMQP 0-9-1: `lapin` (rejected: `amqprs`)
`lapin` is mature, supports publisher confirms, server-named exclusive queues and manual ack/nack with requeue, and has a rustls feature. `amqprs` is newer and has a smaller ecosystem. Both would work; we picked `lapin` for its track record.

### D4. Schema Registry
The registry client is a small layer over `rustman-http` (auth, TLS and proxies come for free), with an in-memory cache per run. Avro uses `apache-avro`, which is pure Rust. Protobuf uses `prost-reflect` with descriptors built from the registry's `.proto` text by `protox` (shared with `rustman-grpc`). JSON Schema uses the validator that `assertions` already uses.

### D5. AsyncAPI parsing: our own serde models (rejected: an external crate)
There's no maintained Rust AsyncAPI crate. We model the subset we need for 2.6 and 3.0 (servers, channels, operations, messages, payload schemas, examples, and basic bindings), normalize both versions into one internal model, and reuse the `openapi-import` `$ref` resolver (local refs, remote refs disabled by default).

### D6. Safety classification
Each messaging operation maps to a policy class: produce/publish is `mutation`, consume through a temporary queue or Kafka/MQTT subscribe is `read`, and a named-queue consume without requeue is `destructive`. The classification is evaluated by the same `GovernedSender`-style gate as HTTP for agent-triggered paths, so there's one audit and refusal format. Human CLI and TUI use is not gated, consistent with HTTP request files.

### D7. `start: before_next`
The runner starts the consume task, waits until it's subscribed (Kafka: offsets resolved to concrete numbers at "latest"; MQTT: SUBACK; AMQP: queue bound and consume-ok), then runs the next step. It joins the consumer after that step and before evaluating assertions. That avoids the classic race where the event arrives before the consumer subscribes.

## Risks / Trade-offs

- [rskafka lacks some features, e.g. idempotent producer, transactions, OAUTHBEARER] → Documented as unsupported. Producing for tests doesn't need exactly-once. OAUTHBEARER can come later.
- [rskafka always produces with `acks: all`, keeps record headers in a name-keyed map, and connects to the brokers the cluster metadata advertises] → `acks: leader`/`none` are sent as `acks: all` with a warning; repeated header names keep the last value with a warning; the safety policy can check only the bootstrap brokers, so docs/messaging.md tells users to allow only bootstrap brokers they trust.
- [lapin assembles a whole AMQP body before handing it over] → `limits.max_message_bytes` trims what is recorded, but a hostile broker can still make the client buffer one large body (lapin 4.12.3 has no hook to refuse a delivery by its header's body size); the prefetch cap (at most 100 for acknowledged consumes) bounds how many bodies are held at once.
- [lapin sizes its buffers (32 frames each) from the negotiated frame size and takes the server's when the client sets none] → the client always asks for `frame_max` ≤ 128 KiB, never 0; lapin keeps the smaller of the client's and the server's value.
- [An interactive consume runs for hours] → the exchange keeps a window of the newest 10000 messages and 64 MiB, counting the dropped ones, so memory and history stay bounded.
- [Reading every partition of large topics from `earliest` floods the user] → Headless bounds plus a TUI consumer that pauses fetching when its buffer exceeds 10000 messages, and drops its oldest messages beyond 64 MiB.
- [A hostile Kafka broker sends huge frames] → rskafka's `max_message_size` is set to the larger of the 4 MiB fetch size and `limits.max_message_bytes`, plus 1 MiB, instead of its 100 MB default, and the reader queue holds at most 64 MiB of records.
- [The allowlist check and the connector read a broker differently (`ws://evil\@allowed/`)] → `endpoint.rs` parses every broker once; the policy origin and the dialed host and port come from that parse, and backslashes, control characters, whitespace and non-DNS hosts are refused, as are numeric last labels that are not strict dotted-decimal IPv4 (`0177.0.0.1` is 127.0.0.1 to URL parsers but may be 177.0.0.1 to getaddrinfo). Invalid-broker errors never echo the broker text.
- [Agents authorize only the Schema Registry base URL] → agent runs disable registry redirects.
- [Literal credentials in messaging files reach agents through `list_requests`/`get_request`] → the MCP output redactor registers them (raw and percent-decoded) like HTTP literals, and summaries show brokers without userinfo.
- [Consuming from a named queue is destructive] → Load-time refusal without `destructive: true`, plus a requeue mode and agent gating.
- [Broker integration tests need Docker] → broker tests run in dedicated Linux CI jobs (`messaging-kafka`, `messaging-mqtt-amqp`) whose service containers (Redpanda, Mosquitto, RabbitMQ) are started by the job, like the existing `jsonrpc-anvil` and `secrets-vault` jobs; the tests read the broker addresses from environment variables and return early without them, so the Linux, macOS and Windows test jobs stay hermetic. This replaces the `testcontainers` crate (which would add a Docker client dependency to every test build). Unit tests use protocol-level fakes where possible.

## Migration Plan

Additive. There's nothing to migrate.

## Open Questions

- Support for Avro schemas declared inline in AsyncAPI (`schemaFormat: application/vnd.apache.avro`) as well as JSON Schema. This can be added later without changing the specs.
