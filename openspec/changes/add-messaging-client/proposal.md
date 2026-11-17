# Proposal

## Why

Event-driven systems put Kafka, MQTT and RabbitMQ between services. Testing them today means kcat, mosquitto_pub/sub, the RabbitMQ admin UI and ad-hoc scripts, with no shared collections and no assertions. AsyncAPI is to these systems what OpenAPI is to REST, and no terminal client turns an AsyncAPI document into runnable, assertable messages. This change brings message brokers into the same workspace, runner and safety model.

## What Changes

- Register request kinds `kafka`, `mqtt` and `amqp` in the `workspace` kind registry. Each request file is either a `produce` (publish) or a `consume` (subscribe) operation.
- Kafka: produce with key, value, headers and an optional partition; consume from chosen partitions and offsets (`earliest`, `latest`, an absolute offset, or a timestamp) without joining a consumer group or committing offsets; SASL PLAIN/SCRAM and TLS.
- Optional Schema Registry decoding (Confluent wire format) for Avro, Protobuf and JSON Schema payloads.
- MQTT 3.1.1 and 5: publish with QoS 0/1/2 and retain; subscribe with topic wildcards; clean sessions with generated client ids by default; TLS.
- AMQP 0-9-1 (RabbitMQ): publish to an exchange with a routing key and properties; consume through a temporary exclusive queue bound to an exchange. Consuming from an existing named queue is treated as destructive.
- AsyncAPI 2.6 and 3.0 import: channels, operations and message schemas become request files plus environments for servers.
- Validation of payloads against AsyncAPI message schemas, before producing and after consuming.
- Assertions and captures on consumed messages; headless bounds for consumers.
- Safety: producing is a mutation and consuming from a named AMQP queue is destructive. Both follow the safety policy semantics: an agent in read-only mode can't produce.
- CLI: `rustman kafka|mqtt|amqp produce|consume`, `rustman import asyncapi`, with JSON-lines output. `rustman run` runs these kinds.

## Capabilities

### New Capabilities

- `messaging-requests`: producing and consuming messages on Kafka, MQTT and AMQP brokers from saved request files, the CLI and the TUI, AsyncAPI import, schema validation and decoding, assertions on consumed messages, and safety gating of produce and destructive consume.

### Modified Capabilities

None. This builds on `workspace`, `environments`, `assertions`, `cli-runner`, `request-history`, `tui`, `agent-mcp-interface` and the safety policy defined by `api-exploration`, without changing their requirements.

## Out of Scope

- Consumer-group membership and offset commits (Rustman is an inspection and testing tool, not a consumer of record).
- Kafka admin operations (create or delete topics, ACLs), MQTT broker administration, and RabbitMQ management API operations.
- AMQP 1.0 (Azure Service Bus, ActiveMQ Artemis), NATS, Pulsar, Google Pub/Sub and SQS/SNS. They're candidates for later changes or `plugins`.
- Load testing brokers (`load-testing` may extend to them later).
- AsyncAPI code generation and mock brokers.

## Impact

- New crate `crates/rustman-messaging` (rskafka, rumqttc, lapin, apache-avro, plus prost-reflect shared with `rustman-grpc`).
- `rustman-cli` gains the `kafka`, `mqtt`, `amqp` and `import asyncapi` subcommands; `rustman-tui` gains a messaging pane.
- All clients are pure Rust with rustls: no librdkafka, OpenSSL or CMake.
