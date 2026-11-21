# Tasks

## 1. Crate and request model

- [x] 1.1 Create `crates/rustman-messaging` with `rskafka`, `rumqttc`, `lapin` (rustls features) and `apache-avro` pinned. Verify that `cargo build -p rustman-messaging` passes on Linux, macOS and Windows CI, and that `cargo tree` shows no `openssl-sys` or `rdkafka-sys`.
- [x] 1.2 Register kinds `kafka`, `mqtt` and `amqp`, and parse the common fields (operation, destination, messages, limits, tls, auth, start). Verify with tests for valid files, the `operation must be produce or consume` error, and the registered-kind list.
- [x] 1.3 Payload encoding (JSON compact, text, base64, file within the workspace root) and variable interpolation. Verify with tests for the key/value/headers scenario and a rejected path escape.

## 2. Kafka

- [ ] 2.1 Produce with explicit partition or murmur2 key hashing and acks modes, recording partition and offset. Verify with a broker test in the `messaging-kafka` CI job (Redpanda service container) and a unit test checking murmur2 partition choice against Java-client reference vectors.
- [ ] 2.2 Consume by partition and offset (`earliest`, `latest`, absolute, timestamp) with no group and no commits. Verify with a test that the group list is unchanged after consuming, plus the timestamp scenario.
- [ ] 2.3 TLS and SASL PLAIN/SCRAM-256/512 with secret redaction. Verify with a broker test in the `messaging-kafka` CI job against a broker configured for SCRAM, plus a test that history contains `«redacted:kafka_pw»`.
- [x] 2.4 Schema Registry client with Avro, Protobuf and JSON Schema decode/encode and a per-run cache. Verify with tests against a fake registry (wiremock-style local hyper server) for Avro id 7 and the raw-bytes warning when a fetch fails.

## 3. MQTT and AMQP

- [ ] 3.1 MQTT publish (v3.1.1/v5, QoS 0/1/2, retain, v5 properties) and subscribe (wildcards, clean session, generated client id, persistent-session warning). Verify with Mosquitto tests in the `messaging-mqtt-amqp` CI job for QoS 2 completion and wildcard topics.
- [ ] 3.2 MQTT TLS, client certificates and WebSocket transport. Verify with a Mosquitto container configured for mTLS.
- [ ] 3.3 AMQP publish with properties, publisher confirms and mandatory/unroutable handling. Verify with a RabbitMQ test in the `messaging-mqtt-amqp` CI job that the run exits 3 with `amqp: unroutable`.
- [ ] 3.4 AMQP consume through a temporary exclusive queue, with named-queue consume requiring `destructive: true` and requeue mode. Verify with tests that the temporary queue is gone afterwards, the load-time refusal error, and requeue leaving the messages in place.

## 4. AsyncAPI and validation

- [x] 4.1 Serde models for AsyncAPI 2.6 and 3.0, normalized into one internal model, reusing the `openapi-import` `$ref` resolver. Verify with fixture documents for both versions and the `unsupported version 2.0` error (exit code 2).
- [x] 4.2 `rustman import asyncapi`: servers become environments, operations become request files with example or synthesized payloads. Verify with an `insta` snapshot of the generated workspace tree for a fixture document.
- [x] 4.3 Payload validation before produce and after consume (implicit `asyncapi_schema` assertion). Verify with tests for the missing-`id` produce refusal and a consumed-message violation reported as a failure.

## 5. Runner, safety, assertions, history

- [ ] 5.1 Headless consume bounds and `start: before_next` subscription readiness. Verify with an end-to-end test where an HTTP POST makes a fake service emit a Kafka event that is captured, plus the quiet-topic 2 s scenario.
- [x] 5.2 Assertions (`messages` counts, JSONPath over values and keys, headers) and captures. Verify with tests for the `count_eq: 1` scenario.
- [x] 5.3 Safety classification (produce is a mutation, named-queue consume is destructive) through the governed gate for agent paths. Verify with tests for the read-only agent refusal (exit code 4, audit entry) and that human CLI produce is allowed.
- [x] 5.4 History exchanges with 64 KiB payload previews and redaction. Verify with the 200 KiB truncation test.
- [x] 5.5 Document the messaging request file formats and AsyncAPI import in `docs/`. Verify with the docs examples loading in a test.

## 6. CLI and TUI

- [ ] 6.1 `rustman kafka|mqtt|amqp produce|consume` with flags, `--json` lines and stdin produce. Verify with assert_cmd tests against the CI broker containers for the consume-to-JSON-lines and stdin-produce scenarios.
- [x] 6.2 TUI messaging pane: broker/destination form, produce composer, live consumer list with pause, value rendering (JSON, decoded schema registry, hex) and a buffer cap of 10000 messages and 64 MiB (the oldest dropped beyond it, with a notice). Verify with ratatui `TestBackend` snapshot tests.
