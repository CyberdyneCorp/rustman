# messaging-requests Specification

## Purpose
Lets users produce and consume messages on Kafka, MQTT and AMQP brokers from saved request files, the CLI and the TUI, generate those files from AsyncAPI documents, validate payloads against message schemas, and assert on consumed messages, all under the safety policy.

## Requirements

### Requirement: Kind registration
This change SHALL register request kinds `kafka`, `mqtt` and `amqp` in the `workspace` kind registry and a messaging pane for them through the `tui` protocol pane extension point.

#### Scenario: Kinds listed
- **GIVEN** a build that includes this change
- **WHEN** a request file has an unknown kind
- **THEN** the error's list of registered kinds SHALL include `kafka`, `mqtt` and `amqp`

### Requirement: Operation field
Every messaging request file SHALL declare `operation: produce` or `operation: consume`, plus `brokers` (or `broker` for mqtt/amqp), the destination (`topic`, `exchange`/`routing_key` or `queue`) and connection settings (`tls`, plus `security` for Kafka or `auth` for mqtt/amqp). `{{variable}}` placeholders SHALL resolve in all string fields. A missing or other operation SHALL fail to load with `messaging: operation must be produce or consume`.

#### Scenario: Missing operation
- **GIVEN** a `kind: kafka` file without `operation`
- **WHEN** the workspace loads
- **THEN** loading SHALL fail with `messaging: operation must be produce or consume`

### Requirement: Message payloads
A produce file SHALL accept `messages` (a list) where each entry has `value` (JSON/YAML value, text, or `{base64: ...}`/`{file: path}` for bytes), optional `key`, and `headers` (ordered name/value pairs; Kafka record headers hold one value per name and are sent in name order, so a repeated name SHALL keep its last value with a warning). JSON values SHALL be serialized compactly as UTF-8. File paths SHALL NOT escape the workspace root.

#### Scenario: JSON value with key and headers
- **GIVEN** a produce entry `{key: "order-1", value: {id: 1, total: 9.5}, headers: [{name: source, value: rustman}]}`
- **WHEN** it is produced
- **THEN** the broker SHALL receive key `order-1`, value `{"id":1,"total":9.5}` and header `source: rustman`

### Requirement: Kafka produce
Kafka produce SHALL send each message to `topic`, to `partition` if set or else to the partition chosen by murmur2 hashing of the key (round-robin for keyless messages), with `acks: all`. `acks: leader` and `acks: none` SHALL be accepted and sent as `acks: all` with a warning, because the pure-Rust client always waits for every in-sync replica (design D1). The exchange SHALL record partition and offset per message. Unknown topics SHALL fail with `kafka: unknown topic '<name>'`.

#### Scenario: Offsets recorded
- **GIVEN** a produce of 2 messages to topic `orders` with partition 0
- **WHEN** the produce succeeds
- **THEN** the exchange SHALL record partition 0 and the two assigned offsets

### Requirement: Kafka consume
Kafka consume SHALL read `topic` from `partitions` (default all) starting at `offset`: `earliest`, `latest` (default), an absolute number, or `{timestamp: RFC 3339}`, without joining a consumer group and without committing offsets. Each message SHALL be recorded with partition, offset, timestamp, key, headers and value.

#### Scenario: Read from a timestamp
- **GIVEN** `offset: {timestamp: "2026-11-17T09:00:00Z"}` on a topic with messages before and after that time
- **WHEN** the consume runs
- **THEN** only messages with timestamps at or after 09:00 SHALL be returned

#### Scenario: No side effects on groups
- **GIVEN** any Kafka consume request
- **WHEN** it runs
- **THEN** no consumer group SHALL be joined and no offsets SHALL be committed on the broker

### Requirement: Kafka consume memory bounds
A Kafka response frame SHALL be at most the larger of the 4 MiB fetch size and `limits.max_message_bytes`, plus 1 MiB, and the fetched records waiting to be recorded SHALL take at most 64 MiB.

#### Scenario: Oversized frame
- **GIVEN** a consume with the default `limits.max_message_bytes` (1 MiB)
- **WHEN** the broker answers a fetch with a 100 MB frame
- **THEN** the consume SHALL fail without reading the frame body

### Requirement: Kafka security
Kafka connections SHALL support `PLAINTEXT`, `SSL` (rustls with verification on, custom CA and client certificates as in `http-requests`), and SASL `PLAIN`, `SCRAM-SHA-256` and `SCRAM-SHA-512` over either. SASL passwords SHALL be supplied through `environments` secrets and redacted everywhere; a password written as a literal SHALL be redacted as `«redacted:password»`. An authentication failure SHALL produce `kafka: authentication failed (<mechanism>)`.

#### Scenario: SCRAM over TLS
- **GIVEN** `security: {protocol: SASL_SSL, mechanism: SCRAM-SHA-512, username: app, password: "{{kafka_pw}}"}`
- **WHEN** a consume runs against a broker configured for SCRAM-SHA-512
- **THEN** the connection SHALL authenticate and the password SHALL appear as `«redacted:kafka_pw»` in history

### Requirement: Schema Registry decoding
When `schema_registry.url` is set, values and keys in the Confluent wire format (magic byte 0 plus a 4-byte schema id) SHALL be decoded by fetching the schema by id (cached per run) and rendering Avro, Protobuf or JSON Schema payloads as JSON. Produce MAY encode with `schema_registry.subject` and `schema_version`. A fetch failure SHALL show the raw bytes with a warning, not fail the consume.

#### Scenario: Avro decode
- **GIVEN** a consumed value with magic byte 0 and schema id 7 registered as an Avro record
- **WHEN** the message is displayed
- **THEN** the value SHALL be shown as JSON with the record's fields and the schema id noted

### Requirement: MQTT publish
MQTT publish SHALL support protocol versions `3.1.1` and `5` (default `5`), QoS 0, 1 and 2 (default 1), `retain` (default false), and for v5 user properties, content type, message expiry and response topic. The exchange SHALL record when the broker acknowledged each message (PUBACK or PUBCOMP).

#### Scenario: QoS 2 publish
- **GIVEN** a publish with `qos: 2` to `sensors/room1/temp`
- **WHEN** it runs
- **THEN** the exchange SHALL record completion of the PUBREC/PUBREL/PUBCOMP flow

### Requirement: MQTT subscribe
MQTT subscribe SHALL accept topic filters with `+` and `#` wildcards and a QoS. It SHALL connect with a clean session (v3.1.1) or clean start (v5) and a generated client id `rustman-<random 8 hex>` unless `client_id` is set. Each received message SHALL be recorded with topic, QoS, retain flag, properties and payload.

#### Scenario: Wildcard subscription
- **GIVEN** a subscription to `sensors/+/temp`
- **WHEN** messages arrive on `sensors/room1/temp` and `sensors/room2/temp`
- **THEN** both SHALL be recorded with their exact topics

#### Scenario: Persistent session opt-in warning
- **GIVEN** `clean_session: false` with a fixed `client_id`
- **WHEN** the subscribe starts
- **THEN** a warning SHALL state that queued messages for this client id will be consumed from the broker

### Requirement: MQTT security
MQTT connections SHALL support `mqtt://` (port 1883), `mqtts://` (port 8883, rustls with verification on, custom CA and client certificates), and `ws://`/`wss://` transports, with username/password from `environments` secrets.

#### Scenario: TLS with client certificate
- **GIVEN** an `mqtts://` broker requiring a client certificate and `tls.client_cert` configured
- **WHEN** the client connects
- **THEN** the TLS handshake SHALL present the client certificate and the connection SHALL succeed

### Requirement: AMQP publish
AMQP publish SHALL send to `exchange` (empty string for the default exchange) with `routing_key` and message properties (`content_type`, `correlation_id`, `reply_to`, `headers`, `delivery_mode`, `expiration`), using publisher confirms. A negative confirm or an unroutable message published with `mandatory: true` SHALL fail with `amqp: message not confirmed` or `amqp: unroutable`.

#### Scenario: Unroutable message
- **GIVEN** `mandatory: true` and a routing key with no bound queue
- **WHEN** the publish runs
- **THEN** it SHALL fail with `amqp: unroutable` and the run SHALL exit with code 3

### Requirement: AMQP consume
AMQP consume SHALL by default declare a server-named, exclusive, auto-delete queue, bind it to `exchange` with `binding_key`, and consume it, so existing queues are untouched. Consuming from an existing named `queue` SHALL require `destructive: true` in the file because it removes messages; `ack: manual` with `requeue: true` SHALL be offered to inspect without removal.

#### Scenario: Temporary queue
- **GIVEN** a consume with `exchange: orders` and `binding_key: "order.*"`
- **WHEN** it runs
- **THEN** a temporary exclusive queue SHALL receive the matching messages and SHALL be deleted when the consume ends

#### Scenario: Named queue without opt-in
- **GIVEN** a consume from `queue: payments` without `destructive: true`
- **WHEN** the file loads
- **THEN** it SHALL fail with `amqp: consuming from an existing queue removes messages; set destructive: true or use requeue`

### Requirement: AMQP consume bounds
Every AMQP consume SHALL set the prefetch count to the message bound (at most 100 when deliveries are acknowledged) and, unless `requeue` is set, acknowledge each delivery only after it is recorded, so a consume never removes or buffers more than `max_messages` messages. The client SHALL ask for an AMQP `frame_max` of at most 128 KiB, never 0, so the negotiated frame size never exceeds it whatever the server announces.

#### Scenario: Server announces a huge frame size
- **GIVEN** a server whose Connection.Tune announces `frame_max` 16 MiB (or 0)
- **WHEN** the client connects
- **THEN** its Tune-Ok SHALL carry `frame_max` 131072

#### Scenario: Destructive consume of part of a queue
- **GIVEN** a named queue holding 5 messages and a consume with `destructive: true` and `limits.max_messages: 2`
- **WHEN** it runs
- **THEN** 2 messages SHALL be recorded and 3 SHALL remain in the queue

### Requirement: Safety gating
Producing (Kafka produce, MQTT publish, AMQP publish) SHALL be classified as a mutation, and consuming from a named AMQP queue without requeue or through a persistent MQTT session (`clean_session: false`) as destructive, under the safety policy semantics of `api-exploration`. Interactive and CLI use by a human SHALL be allowed. Through `agent-mcp-interface`, produce SHALL require `[agent].allow_mutations` and destructive consume `[agent].allow_destructive`.

#### Scenario: Read-only agent produces
- **GIVEN** `[agent].allow_mutations = false`
- **WHEN** an agent runs a kafka produce request file
- **THEN** it SHALL be refused with `blocked: produce requires allow_mutations`, exit code 4 on CLI-mediated paths, and an audit log entry

### Requirement: Broker endpoint checks
Through `agent-mcp-interface`, every broker SHALL be checked against the agent allowlist as the `http`/`https` origin of the endpoint the client connects to, from one parse shared by the check and the connector. A broker containing a backslash, a control character or whitespace, or whose host is neither an IP literal nor a DNS name, SHALL be refused before anything connects, and audit entries SHALL never contain broker credentials.

#### Scenario: Broker read differently by two parsers
- **GIVEN** an agent allowlist naming `http://allowed.example` and an MQTT consume with `broker: 'ws://evil.example\@allowed.example/mqtt'`
- **WHEN** an agent runs it
- **THEN** it SHALL be refused as invalid input and nothing SHALL connect to either host

#### Scenario: Broker credentials stay out of the audit
- **GIVEN** an MQTT consume with `broker: tcp://app:pw@127.0.0.1:1883` outside the agent allowlist
- **WHEN** an agent runs it
- **THEN** it SHALL be refused with `blocked: host not allowed` and the audit entry SHALL name `http://127.0.0.1:1883`

### Requirement: Numeric broker hosts
A broker host whose last label is numeric (digits, or `0x` with hex digits) SHALL be refused unless it is a strict dotted-decimal IPv4 address without leading zeros, which is then the address dialed, so the policy and the resolver can never read it differently.

#### Scenario: Octal host
- **GIVEN** an MQTT consume with `broker: mqtt://0177.0.0.1:1883`
- **WHEN** it runs
- **THEN** it SHALL be refused as invalid input and nothing SHALL connect

### Requirement: Invalid broker errors
An invalid-broker error SHALL show only `<scheme>://«invalid broker»`, never the broker text, and a broker URL that does not parse SHALL have its userinfo read up to its last `@`, its password redacted as a secret.

#### Scenario: Password holding a slash
- **GIVEN** an AMQP consume with `broker: 'amqp://guest:SLASH/PW@rabbit/'`
- **WHEN** it runs
- **THEN** the error SHALL name `amqp://«invalid broker»` and no output SHALL contain `SLASH/PW`

### Requirement: Agent Schema Registry redirects
Agent runs SHALL NOT follow Schema Registry redirects, since only the registry's base URL is checked against the allowlist.

#### Scenario: Registry redirect in an agent run
- **GIVEN** an allowed `schema_registry.url` that answers with a redirect to another host
- **WHEN** an agent runs the request
- **THEN** the redirect SHALL NOT be followed

### Requirement: Literal credentials in agent views
Through `agent-mcp-interface`, `list_requests` SHALL show messaging brokers without their userinfo and `get_request` SHALL redact the literal credentials of messaging files: `security.password`, `auth.password`, broker and Schema Registry URL passwords (as written and percent-decoded) and the registry's `auth`.

#### Scenario: Literal AMQP password
- **GIVEN** an AMQP request file with `broker: 'amqp://guest:AMQP-url-pw@rabbit.example/'`
- **WHEN** an agent calls `list_requests` and `get_request`
- **THEN** neither result SHALL contain `AMQP-url-pw`

### Requirement: Headless consume bounds
Under `rustman run`, a consume SHALL stop after `limits.max_messages` (default 10) messages or `limits.duration_ms` (default 30000), whichever comes first, then disconnect cleanly. Assertions SHALL evaluate over the collected messages. Interactive consumers SHALL run until stopped.

#### Scenario: Quiet topic
- **GIVEN** a consume with `limits.duration_ms: 2000` on a topic that receives nothing
- **WHEN** it runs in `rustman run`
- **THEN** it SHALL end after 2 s with zero messages collected

### Requirement: Unbounded consume window
An unbounded (interactive) consume SHALL keep at most its newest 10000 messages and 64 MiB (of messages as JSON) in its exchange, dropping the oldest and recording how many were `dropped`.

#### Scenario: Long interactive consume
- **GIVEN** an interactive consume that receives 10005 messages
- **WHEN** it is stopped
- **THEN** the exchange SHALL hold the newest 10000 messages and `dropped: 5`

### Requirement: Produce then consume chaining
Within one `rustman run`, a consume step SHALL be able to start before a later produce step (`start: before_next`), subscribing first so that messages caused by the next request are not missed. Collected messages SHALL become available to assertions after the produce step completes.

#### Scenario: Assert an event caused by an HTTP call
- **GIVEN** a kafka consume with `start: before_next` followed by an `http` POST that makes the service emit an `OrderCreated` event
- **WHEN** the collection runs
- **THEN** the consume SHALL capture the event emitted after the POST and its assertions SHALL pass

### Requirement: AsyncAPI import
`rustman import asyncapi <file|url>` SHALL import AsyncAPI 2.6 and 3.0 documents (JSON or YAML): servers become environments (protocol, host, variables), and each operation becomes a request file under a folder per channel (`produce` for publish/send, `consume` for subscribe/receive) with an example payload from `examples`, else synthesized from the schema. Other versions SHALL fail with exit code 2.

#### Scenario: AsyncAPI 3.0 import
- **GIVEN** an AsyncAPI 3.0 document with a Kafka server and a `send` operation on channel `orders`
- **WHEN** it is imported
- **THEN** an environment SHALL be created for the server and `orders/send-order.rman.yaml` SHALL be a `kind: kafka`, `operation: produce` file with an example payload

#### Scenario: Unsupported version
- **GIVEN** an AsyncAPI 2.0 document
- **WHEN** it is imported
- **THEN** the import SHALL fail with `asyncapi: unsupported version 2.0 (supported: 2.6, 3.0)` and exit code 2

### Requirement: Payload schema validation
When a request file references an AsyncAPI message (`asyncapi: {document, message}`), produced payloads SHALL be validated against the message's payload schema before sending (failing with up to 10 violations, each with instance path), and consumed payloads SHALL be validated, with violations reported as failures of an implicit `asyncapi_schema` assertion.

#### Scenario: Invalid produce payload
- **GIVEN** a message schema requiring `id` and a produce payload without it
- **WHEN** the produce is attempted
- **THEN** nothing SHALL be sent and the error SHALL list `/: missing required property 'id'`

### Requirement: Assertions and captures
The `assertions` capability SHALL apply to messaging exchanges: for consume, `json` and `body` assertions over `$.messages[*].value` and `$.messages[*].key`, `header` assertions on message headers, and `messages: {count_gte | count_lte | count_eq}`; for produce, success and broker acknowledgement. Captures SHALL support `from: json` over consumed messages.

#### Scenario: Expect one matching event
- **GIVEN** `- messages: {count_eq: 1}` and `- json: {path: "$.messages[0].value.type", equals: "OrderCreated"}`
- **WHEN** exactly one `OrderCreated` message is consumed
- **THEN** both assertions SHALL pass

### Requirement: History recording
Each produce or consume SHALL be recorded in `request-history` as one exchange with protocol `kafka`, `mqtt` or `amqp`, including destination, per-message metadata and payload previews (values over 64 KiB truncated with their size), redacted per the `environments` contract.

#### Scenario: Large value truncated
- **GIVEN** a consumed message with a 200 KiB value
- **WHEN** the exchange is recorded
- **THEN** history SHALL store a 64 KiB preview and `size: 204800`

### Requirement: CLI commands
`rustman kafka|mqtt|amqp produce <request-file | flags>` and `... consume <request-file | flags>` SHALL run one operation; consume SHALL print one JSON line per message with `--json`, honor `--max-messages`/`--duration`, and read values from stdin lines for produce when stdin is not a TTY. Exit codes SHALL follow `cli-runner`.

#### Scenario: Consume to JSON lines
- **WHEN** the user runs `rustman kafka consume --brokers localhost:9092 --topic orders --offset earliest --max-messages 5 --json`
- **THEN** at most 5 JSON lines with `partition`, `offset`, `timestamp`, `key`, `headers` and `value` SHALL be printed and the exit code SHALL be 0

#### Scenario: Produce from stdin
- **GIVEN** stdin piped with 3 lines
- **WHEN** the user runs `rustman mqtt produce --broker mqtt://localhost --topic t/1`
- **THEN** 3 messages SHALL be published, one per line
