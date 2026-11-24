# Messaging: Kafka, MQTT and AMQP

Rustman produces and consumes broker messages from request files of kind `kafka`, `mqtt` and `amqp`. Each file is one operation: `produce` (publish) or `consume` (subscribe). The behavior is specified in [`openspec/specs/messaging-requests`](https://github.com/CyberdyneCorp/rustman/blob/main/openspec/specs/messaging-requests/spec.md). All clients are pure Rust over rustls; the build needs no librdkafka, OpenSSL or CMake.

This page covers the request file format, the Kafka, MQTT and AMQP clients, AsyncAPI import, payload validation, running messaging requests in collections (assertions, captures, `start: before_next`, history), the safety rules for agents, the CLI commands and the TUI pane.

## Request files

```yaml
version: 1
kind: kafka
name: Create order
operation: produce
brokers: ['{{kafka_host}}:9092']
topic: orders
partition: 0
messages:
  - key: order-1
    value: {id: 1, total: 9.5}
    headers:
      - {name: source, value: rustman}
```

Common fields:

| Field | Meaning |
|---|---|
| `operation` | `produce` or `consume`. A missing or other value fails to load with `messaging: operation must be produce or consume`. |
| `brokers` | Kafka bootstrap brokers: a list of `host:port`, or one comma-separated string. Required for `kind: kafka`. |
| `broker` | The MQTT or AMQP broker URL. Required for `kind: mqtt` and `kind: amqp`. |
| `topic` | The Kafka topic, or the MQTT topic (a filter with `+` and `#` for subscribe). |
| `exchange`, `routing_key`, `binding_key`, `queue` | The AMQP destination. |
| `messages` | Produce only: the messages to send, in order (see below). At least one is required. |
| `limits` | `max_messages` (default 10) and `duration_ms` (default 30000) bound a headless consume; `max_message_bytes` (default 1048576, 1 MiB) is the most of a value kept per message. Zero is refused. |
| `start` | `immediately` (default) or `before_next`: within a run, the consume subscribes, the next step runs, then the consume ends. |
| `tls` | `verify`, `ca_bundle`, `client_cert`, `client_key`, `client_pkcs12` and `client_pkcs12_password`, as for `kind: ws`. |
| `auth` | MQTT and AMQP credentials: `username` and `password`. Kafka uses `security` instead. |
| `schema_registry` | A Confluent-compatible Schema Registry (see below). |
| `asyncapi` | `{document, message}`: validate payloads against an AsyncAPI message's payload schema (see [Payload validation](#payload-validation)). |
| `variables`, `assertions`, `captures` | As for other kinds. |

A field that does not apply to the kind or operation fails to load, for example `kafka: 'partition' does not apply to consume requests`. `{{variables}}` and `{{secret:...}}` references resolve in every string field, including keys, values and headers.

### Message payloads

Each entry of `messages` has a `value`, an optional `key` and optional `headers` (ordered `name`/`value` pairs). A key or value is:

- a string, sent as UTF-8 text;
- any other YAML/JSON value, sent as compact JSON (`{id: 1, total: 9.5}` is sent as `{"id":1,"total":9.5}`);
- `{base64: AAE=}`, sent as the decoded bytes;
- `{file: fixtures/blob.bin}`, the bytes of a file relative to the request file. A path that leaves the workspace fails with `file path escapes workspace`.

A missing or `null` value sends no value (a Kafka tombstone).

## Kafka

### Produce

```yaml
operation: produce
brokers: localhost:9092
topic: orders
partition: 0      # optional
acks: all         # all (default), leader or none
```

Each message goes to `partition` when set, else to the partition the Java client's default partitioner picks for its key (murmur2), so keyed messages land where standard producers put them. Keyless messages go round robin. The exchange records the partition and offset of every message. An unknown topic fails with `kafka: unknown topic '<name>'`, an unknown partition with `kafka: topic '<name>' has no partition <n>`.

Limitations of the client (rskafka):

- Produce always waits for every in-sync replica. `acks: leader` and `acks: none` are accepted and sent as `acks: all`, with a warning.
- A header name holds one value; a repeated name keeps the last value, with a warning. Headers are sent in name order.
- Messages are sent uncompressed. Consumed batches compressed with gzip or snappy are read; lz4 and zstd batches are not supported.
- No idempotent producer, transactions or OAUTHBEARER.

### Consume

```yaml
operation: consume
brokers: localhost:9092
topic: orders
partitions: [0, 1]                          # default: all
offset: earliest                            # earliest, latest (default), 42 or {timestamp: ...}
limits: {max_messages: 5, duration_ms: 10000}
```

A consume reads each partition from its start offset without joining a consumer group and without committing offsets, so it never changes what other consumers see. An absolute offset is kept within the partition's current range. With `{timestamp: "2026-11-17T09:00:00Z"}` (RFC 3339), only messages with timestamps at or after that time are returned.

Each message is recorded with its partition, offset, timestamp, key, headers and value. Values are shown as JSON when they parse as JSON, as text when they are UTF-8, else as `{"base64": "..."}`. A value larger than `limits.max_message_bytes` is cut to that size and marked `truncated`, with its full `size`. Under `rustman run` a consume stops at `max_messages` or `duration_ms`, whichever comes first.

Memory is bounded against a hostile broker: a response frame larger than the 4 MiB fetch size or `limits.max_message_bytes` (whichever is larger) plus 1 MiB fails the consume (with the default limit, 5 MiB), and the fetched records waiting to be recorded take at most 64 MiB (each partition reader also holds the one response it is reading). To read records larger than that, raise `limits.max_message_bytes`.

### Security

```yaml
security:
  protocol: SASL_SSL        # PLAINTEXT (default), SSL, SASL_PLAINTEXT, SASL_SSL
  mechanism: SCRAM-SHA-512  # PLAIN, SCRAM-SHA-256, SCRAM-SHA-512
  username: app
  password: '{{kafka_pw}}'
tls:
  ca_bundle: certs/ca.pem
```

`SSL` and `SASL_SSL` (or any `tls` block) connect with rustls through the same TLS settings as HTTP requests: verification on, the OS trust store plus `ca_bundle`, and client certificates for mutual TLS. `SASL_*` protocols require `mechanism`, `username` and `password`. Keep the password in an environment secret or a `{{secret:...}}` reference: it is redacted everywhere, so the exchange record shows `«redacted:kafka_pw»`. A rejected login fails with `kafka: authentication failed (<mechanism>)`. A broker that cannot be reached fails after the connect timeout (10 s).

### Schema Registry

```yaml
schema_registry:
  url: 'http://{{registry_host}}:8081'
  subject: orders-value       # produce: encode values with this subject
  schema_version: latest      # or a version number
  auth: {type: basic, username: sr, password: '{{sr_pw}}'}
```

Consumed keys and values in the Confluent wire format (magic byte 0 and a 4-byte schema id) are decoded by fetching the schema by id and shown as JSON, with `schema_id` noted. Avro, Protobuf (the registry's `.proto` text, compiled in-process; imports other than the Google well-known types are not resolved) and JSON Schema payloads are supported. Schemas are fetched once per run. When a schema cannot be fetched or does not match, the raw bytes are shown and the exchange carries a warning such as `schema registry: schema id 42: the registry answered HTTP 404; showing the raw bytes`; the consume does not fail.

With `subject`, produce encodes each JSON value with that subject's schema (`schema_version`, default `latest`) and sends it in the wire format.

Registry requests follow redirects as HTTP requests do, except in agent runs (see [Safety](#safety)): there a redirect is not followed and its schema fetch fails with `the registry answered HTTP 302` (a warning on consume).

## MQTT

```yaml
version: 1
kind: mqtt
name: Room temperature
operation: produce
broker: 'mqtts://{{mqtt_host}}'
topic: sensors/room1/temp
protocol_version: 5     # 5 (default) or "3.1.1"
qos: 2                  # 0, 1 (default) or 2
retain: false
properties:             # MQTT 5 only
  content_type: application/json
  response_topic: replies/room1
  message_expiry: 60    # seconds
  correlation_id: c-1
auth: {username: app, password: '{{mqtt_pw}}'}
messages:
  - value: {celsius: 21.5}
    headers: [{name: unit, value: C}]   # MQTT 5 user properties
```

The broker URL picks the transport: `mqtt://` (TCP, port 1883), `mqtts://` (TLS, port 8883), `ws://` and `wss://` (WebSocket with the `mqtt` subprotocol; the path is part of the URL). TLS uses the same settings as HTTP requests, so `tls.ca_bundle` and `tls.client_cert`/`client_key` (or `client_pkcs12`) work for mutual TLS. MQTT messages have no key; message headers are sent as MQTT 5 user properties, so headers and `properties` need protocol version 5.

A publish waits for each message's acknowledgement and records it: `PUBACK` for QoS 1, `PUBREC/PUBREL/PUBCOMP` for QoS 2 (nothing for QoS 0). A refused publish fails with `mqtt: publish to '<topic>' refused (<reason>)`.

```yaml
operation: consume
broker: mqtt://localhost
topic: 'sensors/+/temp'   # + and # wildcards
qos: 1
client_id: probe-1        # default: rustman-<8 random hex digits>
clean_session: false      # default true; false needs a fixed client_id
limits: {max_messages: 5, duration_ms: 10000}
```

A subscribe connects with a clean session (3.1.1) or clean start (5) and a generated client id, so it leaves no state on the broker. Each message is recorded with its exact topic, QoS, retain flag, user properties (as headers), MQTT 5 properties and payload. With `clean_session: false` the broker hands over messages it queued for that client id, which removes them from the broker (agents need `allow_destructive` for it, see [Safety](#safety)); the exchange warns: `mqtt: clean_session is false: queued messages for client id 'probe-1' will be consumed from the broker`. A rejected login fails with `mqtt: authentication failed (username/password)`.

The client uses `rumqttc`'s MQTT 3.1.1 and 5 packet codecs over Rustman's own TCP, rustls and WebSocket connection.

## AMQP

```yaml
version: 1
kind: amqp
name: Order created
operation: produce
broker: 'amqps://{{rabbit_host}}/%2f'
exchange: orders          # "" for the default exchange
routing_key: order.created
mandatory: true           # fail when no queue is bound
properties:
  content_type: application/json
  correlation_id: c-1
  reply_to: replies
  delivery_mode: 2        # 1 transient, 2 persistent
  expiration: 60000       # per-message TTL in ms
auth: {username: app, password: '{{rabbit_pw}}'}
messages:
  - value: {id: 1}
    headers: [{name: source, value: rustman}]   # the AMQP headers table
```

Publishes use publisher confirms and record `confirm` per message. A negative confirm fails with `amqp: message not confirmed`; a `mandatory` message that no queue receives fails with `amqp: unroutable` (a request failure, exit code 3 in a run). `auth` overrides credentials in the URL. `amqps://` uses the shared TLS settings.

```yaml
operation: consume
broker: amqp://localhost
exchange: orders
binding_key: 'order.*'    # default '#'
```

By default a consume declares a server-named, exclusive, auto-delete queue, binds it to `exchange` with `binding_key`, reads it, and deletes it when the consume ends, so existing queues are untouched. Each message is recorded with its exchange, routing key, redelivered flag, headers, properties and payload.

Reading an existing queue removes its messages, so `queue` needs an opt-in, or the file fails to load with `amqp: consuming from an existing queue removes messages; set destructive: true or use requeue`:

```yaml
operation: consume
broker: amqp://localhost
queue: payments
requeue: true        # inspect without removing
# or: destructive: true, with ack: auto (default) or manual
```

Every consume sets the prefetch to `limits.max_messages` (at most 100 when each delivery is acknowledged as it is recorded; with `requeue: true` it is `limits.max_messages`, since every delivery stays unacknowledged), so the broker never delivers more messages than are recorded and the client holds at most that many. `limits.max_message_bytes` trims what is recorded; the AMQP client still receives each message body whole (it cannot refuse a delivery by its announced size), so the memory in flight is at most the prefetch times the broker's largest message: consume only from brokers you trust. The client asks for a frame size of at most 128 KiB (a `frame_max` in the broker URL can lower it, and `frame_max=0` means the cap), and keeps the smaller of that and the broker's, so a broker announcing a huge or unlimited `frame_max` cannot make it allocate large buffers. With `requeue: true` deliveries stay unacknowledged and are all handed back to the queue when the consume ends. With `destructive: true` each message is acknowledged (and so removed) right after it is recorded, whether `ack` is `auto` or `manual`; messages beyond `max_messages` stay in the queue.

## AsyncAPI import

```sh
rustman import asyncapi asyncapi.yaml            # or an http(s) URL
rustman import asyncapi asyncapi.yaml --into events
```

AsyncAPI 2.6 and 3.0 documents (JSON or YAML) are read with the same reference resolver as OpenAPI import: local `$ref`s and files next to the document; remote references only with `--allow-remote-refs`. Other versions fail with `asyncapi: unsupported version 2.0 (supported: 2.6, 3.0)` and exit code 2.

- Each server with a Kafka, MQTT or AMQP protocol becomes `environments/<server>.yaml` with a `brokers` (Kafka) or `broker` (MQTT, AMQP) variable; server variables take their defaults. Servers of other protocols are skipped with a warning.
- Each operation becomes `<collection>/<channel>/<operation>.rman.yaml` (kebab-case names): `produce` for `publish` (2.6) and `send` (3.0), `consume` for `subscribe` and `receive`. A produce gets one message per documented message, with the first example payload, else one synthesized from the payload schema (and a Kafka key from the `kafka` binding's key schema).
- The document is treated as untrusted: a `{{` in an example or schema default string, a channel address, an exchange or a queue is written as `\{{`, so an imported request never resolves a variable or a `{{secret:...}}` reference the document planted (object keys are never interpolated).
- Channel parameters (`orders/{id}`) become `{{id}}` variables, or `+` in an MQTT subscription. MQTT `qos`/`retain` and AMQP exchange and queue bindings are used; other bindings are ignored with a warning. An AMQP consume without an exchange binding reads its queue with `requeue: true`.
- The bundled document is written as `<collection>/asyncapi.json`, and requests whose message has a payload schema reference it with `asyncapi`, so payloads are validated.

The collection folder defaults to the document title in kebab-case; files that already exist are kept and reported.

## Payload validation

```yaml
asyncapi:
  document: ../asyncapi.json   # relative to the request file, inside the workspace
  message: OrderCreated        # the message's name or its key
```

Before a produce, every value is validated against the message's payload schema (references into the document's `components` resolve). If one does not match, nothing is sent and the error lists up to 10 violations with their instance paths, such as `asyncapi: message 1 does not match 'OrderCreated', nothing was sent: /: missing required property 'id'`. After a consume, each message records its `schema_violations`, and the implicit `asyncapi_schema` assertion fails when any message breaks the schema. Values cut at `limits.max_message_bytes` are not checked.

## Runs, assertions and history

`rustman run` runs messaging requests in collection order with the other kinds, sharing variables and captures. A produce passes when every message was sent and acknowledged; a consume passes when it ends at its bounds, even with no message. A failure to connect, authenticate or send makes the request `errored` (exit code 3), and a failed assertion makes it `failed` (exit code 1).

### Headless bounds

Under `rustman run` a consume stops after `limits.max_messages` messages (default 10) or `limits.duration_ms` (default 30000), whichever comes first, then disconnects cleanly. A consume of a quiet topic with `limits: {duration_ms: 2000}` ends after 2 s with zero messages. `rustman kafka|mqtt|amqp consume` takes the same bounds, which `--max-messages` and `--duration` replace; the TUI consumes until you stop it.

### Assertions and captures

Assertions and captures read the exchange as a JSON document: `$.messages[*]` for a consume (each with `partition`, `offset`, `timestamp`, `topic`, `routing_key`, `key`, `headers`, `value` and `size`, as the broker gives them) and `$.produced[*]` for a produce (with `partition` and `offset`, or the broker's `ack`). Values that are JSON are matched as JSON. `messages: {count_gte | count_lte | count_eq}` counts the consumed messages, and `header` assertions read the headers of every consumed message.

```yaml
version: 1
kind: kafka
name: Order events
operation: consume
brokers: '{{kafka_brokers}}'
topic: orders
start: before_next
limits: {max_messages: 1, duration_ms: 10000}
assertions:
  - messages: {count_eq: 1}
  - json: {path: '$.messages[0].value.type', equals: OrderCreated}
  - header: {name: source, equals: shop}
captures:
  - var: order_id
    from: json
    path: $.messages[0].value.id
```

A consume validated against an AsyncAPI message also gets the implicit `asyncapi_schema` assertion (see [Payload validation](#payload-validation)).

### Produce then consume: `start: before_next`

A consume with `start: before_next` subscribes first, then the next request of its folder runs while it collects, and its assertions are evaluated once both are done. The subscription is ready when Kafka offsets are resolved, the MQTT SUBACK arrived, or the AMQP queue is bound and consuming, so an event the next request causes cannot be missed. With the consume above saved as `orders/1-events.rman.yaml`, this request runs while it listens:

```yaml
version: 1
kind: http
name: Create order
method: POST
url: '{{base_url}}/orders'
body:
  type: json
  content: {item: book}
assertions:
  - status: 201
```

Set `limits.max_messages` to the number of events you expect, so the consume ends as soon as they arrived instead of at `duration_ms`. Without a next request in the folder, `before_next` runs the consume on its own.

### History

Each produce or consume is one history entry with protocol `kafka`, `mqtt` or `amqp` under `record.messaging`: the brokers, the destination, each message's metadata, and its key and value. An unbounded consume (the TUI) keeps only its newest 10000 messages and 64 MiB (of messages as JSON) in the exchange, so memory and the history entry stay bounded; the oldest beyond that are dropped and counted in `dropped`. Keys and values larger than 64 KiB are kept as a 64 KiB text preview with `truncated: true` and their full `size` (a 200 KiB value is stored as a 64 KiB preview and `size: 204800`). Secrets are replaced by `«redacted:name»` before anything is stored or printed, including credentials written as literals (`security.password`, `auth.password` and the password of a broker URL, shown as `«redacted:password»`; a URL that does not parse, such as `amqp://guest:pa/ss@rabbit/` with an unencoded `/`, has its userinfo read up to the last `@`, so such a password is redacted too) and secrets that appear in payloads or headers. Messaging entries are not replayed by `rustman history replay`.

## Safety

Producing (Kafka produce, MQTT publish, AMQP publish) is a mutation, and consuming from a named AMQP queue without `requeue`, or through a persistent MQTT session (`clean_session: false`), is destructive; every other consume is a read. A human running `rustman run`, the CLI commands or the TUI is not gated, as for HTTP requests.

Agents run messaging request files through `send_request` and `run_collection` of `rustman mcp serve`. Before anything connects:

- a produce needs `[agent] allow_mutations = true`, else it is refused with `blocked: produce requires allow_mutations`; a named-queue consume needs `[agent] allow_destructive = true` (`blocked: consuming from an existing queue requires allow_destructive`), and so does an MQTT consume with `clean_session: false` (`blocked: consuming through a persistent session (clean_session: false) requires allow_destructive`);
- every broker must pass the agent allowlist, checked as an origin: a Kafka `host:port` as `http://host:port` (port 9092 when none is given), `mqtt://`, `tcp://` and `amqp://` as `http://`, `mqtts://`, `ssl://` and `amqps://` as `https://`, and `ws://`/`wss://` as their HTTP origin, with the protocol's default port when the URL has none (1883, 8883, 5672, 5671, 80, 443). The host and port checked are the ones the client connects to: one parse serves both, hosts are lowercased and lose a trailing dot, and a broker containing a backslash, a control character or whitespace, or whose host is neither an IP address nor a DNS name, is refused as invalid input (as is a Kafka broker with credentials or a path). A host whose last label is numeric (`127.1`, `0177.0.0.1`, `0x7f.1`, `2130706433`, `1.2.3.0x`) must be a dotted-decimal IPv4 address without leading zeros, since URL parsers and resolvers read the other forms differently. An invalid-broker error never repeats the broker: it shows `<scheme>://«invalid broker»`. Audit entries name the checked origins, never the broker's credentials. Broker URL values of the active environment count for the allowlist; Kafka brokers, which are not URLs, need `[agent] allowed_hosts`, such as `["http://localhost:9092"]`;
- a Kafka request's `schema_registry.url` must pass the allowlist too, as a read, and registry redirects are not followed, since only that URL was checked;
- only the Kafka bootstrap brokers are checked: the client then connects to the brokers the cluster advertises in its metadata, so allow only bootstrap brokers you trust;
- a produce or a destructive consume is confirmed by the human, as for an HTTP `POST`.

`list_requests` shows messaging brokers without their userinfo, and `get_request` returns the file with its literal broker credentials (`security.password`, `auth.password`, broker and Schema Registry URL passwords, as written and percent-decoded, and the registry's `auth`) replaced by `«redacted:password»`; as for HTTP literals, values shorter than 6 characters are not redacted, so keep credentials in secrets.

A refusal is the tool's `blocked` error and gets an audit log entry. Agent consumes stop after at most 100 messages or 60 s. In a run, a refused operation is an error, and the run exits 4 instead of 3.

## `rustman kafka|mqtt|amqp produce` and `consume`

Each command runs one operation: a request file (its path, id or name), or one described with flags. Without a workspace only flags work.

```sh
rustman kafka consume --brokers localhost:9092 --topic orders --offset earliest --max-messages 5 --json
rustman kafka produce --brokers localhost:9092 --topic orders --key order-1 --value '{"id":1}'
printf '21.5\n22.0\n' | rustman mqtt produce --broker mqtt://localhost --topic t/1
rustman mqtt consume --broker mqtt://localhost --topic 'sensors/+/temp'
rustman amqp produce --broker amqp://localhost --exchange orders --routing-key order.created --mandatory --value hi
rustman amqp consume --broker amqp://localhost --exchange orders --binding-key 'order.*' --json
rustman kafka consume events/orders.rman.yaml --duration 10s
```

- Produce sends one message per `--value`; without `--value`, each line of a piped stdin is one message (a line's text is sent as is, so a JSON line stays JSON). `--key` (Kafka) and `--header "name: value"` apply to every message. It prints where each message landed: `produced partition 0 offset 41`, or the acknowledgement (`produced (PUBACK)`).
- Consume prints each message as it arrives: where it came from (`p0@41`, the MQTT topic, the AMQP routing key) and its value. With `--json` it prints one JSON object per message with `partition`, `offset`, `timestamp`, `key`, `headers` and `value` (Kafka), or `topic`, `qos` and `retain` (MQTT), or `exchange`, `routing_key` and `properties` (AMQP).
- Flags: `--brokers`, `--topic`, `--partition` (repeatable for consume) and `--offset` (`earliest`, `latest`, a number or an RFC 3339 timestamp) for Kafka; `--broker`, `--topic`, `--qos` and `--retain` for MQTT; `--broker`, `--exchange`, `--routing-key`, `--mandatory`, `--binding-key`, `--queue`, `--destructive` and `--requeue` for AMQP. A request file defines its operation, so these flags cannot be combined with one. `--var`, `--env`, `--workspace`, `--no-history` and `--json` work for both.

The exit code is 0 when the operation passed, 1 when an assertion of the request failed, 3 when it failed (unreachable broker, unknown topic, unroutable message, failed login), 4 when the safety policy refused it, and 130 on Ctrl+C. Printed messages, errors and warnings are redacted like history.

## TUI

Messaging requests open in the messaging pane. The URL row shows the operation (`PRODUCE` or `CONSUME`) and the broker address, and the tabs are:

- **Messages**: the live consumer list. Each row shows where the message came from, its key and its value; the selected message's value is shown below, pretty-printed when it is JSON, noted as `decoded from Schema Registry id 7` when the Schema Registry decoded it, as text, or as a hex dump when it is binary. `j`/`k` move the selection, `g`/`G` jump to the first or newest message, and `p` (or Space) pauses the list, which then shows how many messages arrived since. The pane keeps at most 10000 messages: when the buffer is full the consumer stops, so it fetches nothing more, and what it collected is kept. It also keeps at most 64 MiB of messages: beyond that the oldest are dropped, and the status line says how many (`3 oldest dropped (over 64 MiB)`).
- **Compose**: the messages a produce sends, as YAML.
- **Broker**: the destination and connection settings (topic, partitions and offset, exchange, keys and queue, QoS, security, auth, TLS, Schema Registry, limits) as YAML.
- **Assertions**: the request's assertions, as YAML.

`Ctrl+R` runs the operation. A consume runs until `Esc` stops it (the exchange is then shown and recorded) or its buffer is full. Messages reach the pane redacted.

## Testing against real brokers

Unit and integration tests use in-process fakes: a fake Schema Registry and an MQTT broker (3.1.1 and 5 over TCP, mutual TLS and WebSocket) on loopback. The runner, CLI and TUI tests use that MQTT broker; their Kafka variants (`crates/rustman-messaging/tests/runner.rs` and `crates/rustman-cli/tests/messaging.rs`) run in the CI `messaging-kafka` job. The MQTT and AMQP tests in `tests/mosquitto.rs` and `tests/rabbitmq.rs` run only in the CI `messaging-mqtt-amqp` job against Mosquitto (with a mutual-TLS listener) and RabbitMQ, and return early elsewhere (`RUSTMAN_MQTT_BROKER`, `RUSTMAN_MQTTS_BROKER` with `RUSTMAN_MQTT_TLS_DIR`, `RUSTMAN_AMQP_BROKER`). The Kafka broker tests in `crates/rustman-messaging/tests/kafka_broker.rs` run only in the CI `messaging-kafka` job, which starts Redpanda (PLAINTEXT and SCRAM-SHA-512) and checks that no consumer group exists after consuming. To run them locally:

```sh
docker run -d --name redpanda -p 9092:9092 redpandadata/redpanda:v24.2.27 redpanda start \
  --mode dev-container --smp 1 \
  --kafka-addr PLAINTEXT://0.0.0.0:9092 --advertise-kafka-addr PLAINTEXT://127.0.0.1:9092
RUSTMAN_KAFKA_BROKERS=127.0.0.1:9092 cargo test -p rustman-messaging --test kafka_broker
```

```sh
docker run -d --name rabbitmq -p 5672:5672 rabbitmq:4.1.4-alpine
RUSTMAN_AMQP_BROKER=amqp://guest:guest@127.0.0.1:5672/%2f cargo test -p rustman-messaging --test rabbitmq
```
