# Tasks

## 1. Crate and request model

- [x] 1.1 Create `crates/rustman-messaging` with `rskafka`, `rumqttc`, `lapin` (rustls features) and `apache-avro` pinned. Verify that `cargo build -p rustman-messaging` passes on Linux, macOS and Windows CI, and that `cargo tree` shows no `openssl-sys` or `rdkafka-sys`.
- [x] 1.2 Register kinds `kafka`, `mqtt` and `amqp`, and parse the common fields (operation, destination, messages, limits, tls, auth, start). Verify with tests for valid files, the `operation must be produce or consume` error, and the registered-kind list.
- [x] 1.3 Payload encoding (JSON compact, text, base64, file within the workspace root) and variable interpolation. Verify with tests for the key/value/headers scenario and a rejected path escape.

## 2. Kafka

- [x] 2.1 Produce with explicit partition or murmur2 key hashing and acks modes, recording partition and offset. Verify with a broker test in the `messaging-kafka` CI job (Redpanda service container) and a unit test checking murmur2 partition choice against Java-client reference vectors. (`kafka_broker.rs` against Redpanda in the `messaging-kafka` job and `murmur2_matches_the_java_client_reference_vectors`; passed in CI run 45089029769 on main.)
- [x] 2.2 Consume by partition and offset (`earliest`, `latest`, absolute, timestamp) with no group and no commits. Verify with a test that the group list is unchanged after consuming, plus the timestamp scenario. (`a_timestamp_offset_returns_only_later_messages`, and the job's "No consumer group was joined" step checks `rpk group list`; passed in CI run 45089029769 on main.)
- [x] 2.3 TLS and SASL PLAIN/SCRAM-256/512 with secret redaction. Verify with a broker test in the `messaging-kafka` CI job against a broker configured for SCRAM, plus a test that history contains `«redacted:kafka_pw»`. (`scram_sha_512_authenticates_and_history_hides_the_password` against the SCRAM broker of the `messaging-kafka` job; passed in CI run 45089029769 on main.)
- [x] 2.4 Schema Registry client with Avro, Protobuf and JSON Schema decode/encode and a per-run cache. Verify with tests against a fake registry (wiremock-style local hyper server) for Avro id 7 and the raw-bytes warning when a fetch fails.

## 3. MQTT and AMQP

- [x] 3.1 MQTT publish (v3.1.1/v5, QoS 0/1/2, retain, v5 properties) and subscribe (wildcards, clean session, generated client id, persistent-session warning). Verify with Mosquitto tests in the `messaging-mqtt-amqp` CI job for QoS 2 completion and wildcard topics. (`qos_2_completes_and_wildcards_match_on_mosquitto` in the `messaging-mqtt-amqp` job; passed in CI run 45089029769 on main.)
- [x] 3.2 MQTT TLS, client certificates and WebSocket transport. Verify with a Mosquitto container configured for mTLS. (`mutual_tls_connects_with_the_client_certificate` against Mosquitto with `require_certificate` in the `messaging-mqtt-amqp` job, plus `the_websocket_transport_carries_mqtt`; passed in CI run 45089029769 on main.)
- [x] 3.3 AMQP publish with properties, publisher confirms and mandatory/unroutable handling. Verify with a RabbitMQ test in the `messaging-mqtt-amqp` CI job that the run exits 3 with `amqp: unroutable`. (`an_unroutable_mandatory_message_fails` against RabbitMQ in the `messaging-mqtt-amqp` job; passed in CI run 45089029769 on main.)
- [x] 3.4 AMQP consume through a temporary exclusive queue, with named-queue consume requiring `destructive: true` and requeue mode. Verify with tests that the temporary queue is gone afterwards, the load-time refusal error, and requeue leaving the messages in place. (`a_temporary_queue_receives_matching_messages_and_is_deleted`, `requeue_leaves_the_messages_in_place` and `a_destructive_consume_removes_only_what_it_records` in the `messaging-mqtt-amqp` job; passed in CI run 45089029769 on main.)

## 4. AsyncAPI and validation

- [x] 4.1 Serde models for AsyncAPI 2.6 and 3.0, normalized into one internal model, reusing the `openapi-import` `$ref` resolver. Verify with fixture documents for both versions and the `unsupported version 2.0` error (exit code 2).
- [x] 4.2 `rustman import asyncapi`: servers become environments, operations become request files with example or synthesized payloads. Verify with an `insta` snapshot of the generated workspace tree for a fixture document.
- [x] 4.3 Payload validation before produce and after consume (implicit `asyncapi_schema` assertion). Verify with tests for the missing-`id` produce refusal and a consumed-message violation reported as a failure.

## 5. Runner, safety, assertions, history

- [x] 5.1 Headless consume bounds and `start: before_next` subscription readiness. Verify with an end-to-end test where an HTTP POST makes a fake service emit a Kafka event that is captured, plus the quiet-topic 2 s scenario. (`kafka_before_next_consume_captures_the_event_an_http_post_causes` and `a_quiet_topic_ends_after_its_duration_with_no_messages` in `tests/runner.rs`, run by the `messaging-kafka` job; passed in CI run 45089029769 on main.)
- [x] 5.2 Assertions (`messages` counts, JSONPath over values and keys, headers) and captures. Verify with tests for the `count_eq: 1` scenario.
- [x] 5.3 Safety classification (produce is a mutation, named-queue consume is destructive) through the governed gate for agent paths. Verify with tests for the read-only agent refusal (exit code 4, audit entry) and that human CLI produce is allowed.
- [x] 5.4 History exchanges with 64 KiB payload previews and redaction. Verify with the 200 KiB truncation test.
- [x] 5.5 Document the messaging request file formats and AsyncAPI import in `docs/`. Verify with the docs examples loading in a test.

## 6. CLI and TUI

- [x] 6.1 `rustman kafka|mqtt|amqp produce|consume` with flags, `--json` lines and stdin produce. Verify with assert_cmd tests against the CI broker containers for the consume-to-JSON-lines and stdin-produce scenarios. (`tests/messaging.rs` in rustman-cli, including `kafka_consume_prints_json_lines_with_partition_and_offset` and `stdin_lines_are_published_one_message_each`, run by the `messaging-kafka` job; passed in CI run 45089029769 on main.)
- [x] 6.2 TUI messaging pane: broker/destination form, produce composer, live consumer list with pause, value rendering (JSON, decoded schema registry, hex) and a buffer cap of 10000 messages and 64 MiB (the oldest dropped beyond it, with a notice). Verify with ratatui `TestBackend` snapshot tests.

## 7. Security hardening

- [x] 7.1 Refuse numeric last host labels that are not strict dotted-decimal IPv4 for Kafka, MQTT (`mqtt`/`tcp`/`mqtts`/`ssl`) and AMQP brokers, and dial the parsed IP. Verify with endpoint tests for `0177.0.0.1`, `1.2.3.0x`, `2130706433`, `0x7f.1`, `012.0.0.01`, `0177.0.0.01` and `127.1` on every scheme.
- [x] 7.2 Redact literal messaging credentials (security/auth passwords, broker and registry URL passwords raw and percent-decoded, registry auth) in MCP output, and show brokers without userinfo in `list_requests`. Verify with an MCP test that `list_requests` and `get_request` never contain them.
- [x] 7.3 Never echo a broker in invalid-broker errors, and read the userinfo of an unparseable broker up to its last `@`, registering its password as a secret. Verify with error-text and redactor tests for `amqp://guest:SLASH/PW@rabbit/`.
- [x] 7.4 Cap the AMQP `frame_max` at 128 KiB (never 0) and keep acknowledged consumes at most 100 deliveries in flight. Verify with a fake AMQP server announcing 16 MiB and 0 that Tune-Ok carries 131072.
- [x] 7.5 Do not follow Schema Registry redirects in agent runs. Verify with a test that a redirect to another host is not followed with the agent runner and is with a human's.
- [x] 7.6 Keep a window of the newest 10000 messages and 64 MiB in unbounded consume exchanges, counting `dropped`. Verify with exchange unit tests.
