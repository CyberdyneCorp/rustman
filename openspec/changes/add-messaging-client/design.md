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
- [Reading every partition of large topics from `earliest` floods the user] → Headless bounds plus a TUI consumer that pauses fetching when its buffer exceeds 10000 messages.
- [Consuming from a named queue is destructive] → Load-time refusal without `destructive: true`, plus a requeue mode and agent gating.
- [Broker integration tests need Docker] → testcontainers-based tests run in a dedicated CI job on Linux. Unit tests use protocol-level fakes where possible.

## Migration Plan

Additive. There's nothing to migrate.

## Open Questions

- Support for Avro schemas declared inline in AsyncAPI (`schemaFormat: application/vnd.apache.avro`) as well as JSON Schema. This can be added later without changing the specs.
