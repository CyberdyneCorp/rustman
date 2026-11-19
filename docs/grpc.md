# gRPC

Rustman calls gRPC services from `kind: grpc` request files, with no generated code: message types come from the server (reflection), from local `.proto` files compiled in-process, or from a binary descriptor set. No `protoc` is needed.

This page covers the request file, where schemas come from, the descriptor cache, the method browser with its message templates, how calls are sent (message encoding, call types and the message timeline, stream bounds, deadlines, metadata, TLS and gRPC-Web, and status details), assertions and captures, `kind: grpc` in `rustman run` and history, and the `rustman grpc` commands. The behavior is specified in [`openspec/changes/add-grpc-client`](../openspec/changes/add-grpc-client/).

## Request files

```yaml
version: 1
kind: grpc
name: Get product
target: "{{grpc_host}}"             # host:port, grpc://host:port or grpcs://host:port
method: shop.v1.Catalog/GetProduct  # package.Service/Method
message:
  id: "p-1"
metadata:
  - name: authorization
    value: "Bearer {{token}}"
deadline_ms: 5000
```

| Field | Required | Meaning |
|---|---|---|
| `target` | yes | `host:port`, `grpc://host:port` (plaintext HTTP/2) or `grpcs://host:port` (TLS). A bare `host:port` uses TLS on port 443 only. The host is a DNS name, an IPv4 address or a bracketed IPv6 address (`[::1]:50051`). `{{variables}}` work. |
| `method` | yes | The fully qualified method, `package.Service/Method`. |
| `message` | one of | The request message (unary and server streaming), as YAML or JSON in the proto3 JSON mapping. |
| `messages` | one of | A list of request messages, sent in order (client and bidi streaming). Exactly one of `message` or `messages` is allowed. |
| `metadata` | no | An ordered list of `{name, value}` pairs sent as request headers. Names ending in `-bin` take base64 values. `enabled: false` keeps a row without sending it. |
| `deadline_ms` | no | The call deadline, sent as `grpc-timeout`. |
| `tls` | no | `enabled`, `insecure`, `ca_bundle`, `client_cert`, `client_key`, `client_pkcs12`, `client_pkcs12_password`: the same options as for HTTP requests. `enabled` overrides the choice made from the target. |
| `transport` | no | `grpc` (default) or `grpc-web`. |
| `schema` | no | `protos` and `import_paths`, or `descriptor_set` (see below). Without it, reflection is used. |
| `stream` | no | `max_messages` (default 100) and `duration_ms` (default 30000): the bounds of a headless stream. |
| `variables`, `assertions`, `captures`, `description`, `tags` | no | As for other kinds. |

Load errors name the line, for example `grpc: exactly one of message or messages is allowed`, `grpc: method must be fully qualified as package.Service/Method` or `grpc: target must be host:port or a grpc:// or grpcs:// URL`.

### Interpolation and redaction

`{{variables}}` are resolved in the target, metadata names and values, TLS settings, schema paths and the string values of messages (keys stay as written). Every missing variable is reported at once.

Secrets keep their placeholder wherever the call is recorded: metadata `authorization: "Bearer {{token}}"` reaches the server with the real token, and history stores `Bearer «redacted:token»`. Literal credentials in `authorization` metadata are redacted the same way as the HTTP `Authorization` header.

Redaction matches secret values as text. Binary (`-bin`) metadata and `bytes` message fields are recorded in base64, so a secret the server returns inside binary data is not recognized there; keep secrets out of binary fields or treat those records as sensitive.

## Schema sources

### Server reflection

With no `schema`, Rustman asks the server through `grpc.reflection.v1.ServerReflection`, and falls back to `grpc.reflection.v1alpha` when v1 answers `UNIMPLEMENTED`. It lists the services, fetches the file declaring each one, and follows imports transitively. Well-known types (`google/protobuf/*.proto`) a server does not serve come from Rustman's bundled copies. A listed service whose descriptors the server cannot return (such as a health service registered apart from reflection) is left out. Reflection, connecting included, must finish within the run's timeout (`--timeout`, else `[defaults] timeout_ms`, else 30 s), or it fails with `grpc: reflection at <target> failed: no answer within <ms> ms` (exit code 3). A schema of more than 4096 files or 64 MiB of descriptors is refused as too large.

When neither version is implemented, the call fails with exit code 2:

```text
grpc: server reflection unavailable at grpc://localhost:50051; set schema.protos or schema.descriptor_set
```

### Local `.proto` files

```yaml
schema:
  protos: [protos/shop/v1/catalog.proto]
  import_paths: [protos]
```

The files are compiled in-process with [protox](https://crates.io/crates/protox). Imports are looked up in `import_paths` (each proto's own folder when there are none); `google/protobuf/*.proto` imports always resolve. Compile errors read `<file>:<line>:<col>: <message>`, with the file named as imported:

```text
catalog.proto:7:18: expected ';' or '[', but found '}'
```

A missing token is reported right after the token before it.

### Descriptor sets

```yaml
schema:
  descriptor_set: shop.binpb
```

A binary `FileDescriptorSet`, as written by `protoc --descriptor_set_out --include_imports` or `buf build -o`. A descriptor set larger than 64 MiB is refused.

### Paths

Schema paths are relative to the request file's folder and may not leave the workspace: `../` escapes (written or produced by a variable) fail with `file path escapes workspace: <path>`. Imports are read only from inside their import path: an import reached through a symlink pointing out of it fails with `import '<name>' resolves outside its import path`.

## Descriptor cache

Descriptors obtained by reflection are cached in `.rustman/grpc/descriptors/<sha256(target)>.bin`, with a `.json` sidecar recording the target (secrets redacted) and when it was fetched. The key is the canonical target (`grpc://host:port` or `grpcs://host:port`), hashed so no host name or secret appears in file names.

A cached schema is reused until it is refreshed. When the cache answers, Rustman shows a notice that the data may be stale:

```text
schema from the descriptor cache (fetched 2026-11-19T10:00:00Z); it may be stale, refresh with `rustman grpc list --refresh`
```

A refresh asks the server again and rewrites the cache. When the server cannot be reached, the cached schema stands in, with the same notice, so services, methods and templates can be browsed offline. `.rustman/` is local state; keep it out of Git.

## Method browser and templates

Services are listed by name with their methods and call types: `unary`, `server streaming`, `client streaming` or `bidi`. A description of a method shows its call type, request and response types and a request template:

```text
shop.v1.Catalog/ListProducts
  call type: server streaming
  request:   shop.v1.ListProductsRequest
  response:  shop.v1.Product
  request template:
    {
      "pageSize": 0,
      "category": "CATEGORY_UNSPECIFIED"
    }
```

Templates use the proto3 JSON mapping (lowerCamelCase names) with every field at its default:

- nested messages are expanded three levels deep; deeper ones are `{}`
- repeated fields hold one element, maps one entry (`{"": 0}`)
- enums take their first value name
- only the first field of a `oneof` is included
- 64-bit integers are strings (`"0"`), floating-point numbers `0.0`, bytes `""`
- well-known types take their canonical JSON form: `Timestamp` `"1970-01-01T00:00:00Z"`, `Duration` `"0s"`, wrappers their inner default, `Struct` `{}`, `Value` `null`

## Calls

### Message encoding

Messages are written in YAML or JSON and encoded with the proto3 JSON mapping: field names in lowerCamelCase or as written in the `.proto` (`pageSize` or `page_size`), enums by name or number, 64-bit integers as strings or numbers, and well-known types in their canonical JSON forms (`Timestamp` `"2026-11-17T10:00:00Z"`, `Duration` `"1.5s"`, …).

Everything that can fail without the network is checked before anything is sent: the method must exist in the schema, every field must exist in its message type (nested, repeated and map messages included), unary and server-streaming methods take exactly one message, and metadata must be valid. An unknown field names the field and its type, with exit code 2:

```text
grpc: unknown field 'idd' in shop.v1.GetProductRequest
```

Responses are shown in the canonical proto3 JSON form: lowerCamelCase names, default values left out, 64-bit integers as strings.

### Call types and the exchange

Every call type is supported. A unary call sends one message and gets one response; server streaming collects every response in order; client streaming sends each entry of `messages` in order, then half-closes, and gets one response; bidi streaming sends `messages` in order while receiving concurrently, then half-closes.

The exchange of a call holds:

- `status`: code, name and message, shown as `OK (0)` or `NOT_FOUND (5): product missing not found`, plus decoded `details` (below)
- `headers` and `trailers`: the response metadata, recorded separately; binary (`-bin`) values in base64
- `messages`: every received message as JSON; a unary or client-streaming response is the first one
- `timeline` (streaming calls only): one entry per message with `direction` (`sent` or `received`), `at_ms` since the call started, `size_bytes` (encoded size) and `message`
- `timings`: `connect_ms`, `tls_ms`, `first_byte_ms` (response headers) and `total_ms`

JSONPath assertions see the response message of a unary or client-streaming call at `$`, and the received messages of a server-streaming or bidi call at `$.messages`.

### Headless stream bounds

Under `rustman run`, receiving stops after `stream.max_messages` messages (default 100) or `stream.duration_ms` (default 30000), whichever comes first. The call is then cancelled (the server sees the cancellation), the exchange is marked `stopped` and ends with `CANCELLED (1): stopped by rustman after 100 messages (stream.max_messages)`, and assertions evaluate over the messages collected so far. Interactive streams run until they are stopped or their deadline expires.

### Deadlines

Every call carries a deadline: `deadline_ms`, else `rustman.toml` `[defaults] timeout_ms` (which `--timeout` of `rustman run` and `rustman grpc call` replaces), else 30000 ms. It is sent as `grpc-timeout` (in milliseconds, or whole seconds when that would need more than eight digits) and also enforced on the client for the whole call, streams included. An expired deadline ends the call with `DEADLINE_EXCEEDED (4)`, which counts as a request error (exit code 3).

### Metadata

`metadata` entries are sent in order as request headers; names are lowercased. Names ending in `-bin` take base64 (padded or not) and are sent as binary metadata: `{name: trace-bin, value: "AQID"}` reaches the server as the bytes `01 02 03`. Invalid base64, invalid names and values that are not printable ASCII fail before sending with `grpc: invalid metadata '<name>': <reason>`; the value is never shown.

### Transport security

`grpcs://` targets, and targets with `tls.enabled: true` (the default for a bare `host:port` on port 443), use TLS with HTTP/2 negotiated by ALPN. Verification is on, and `tls` takes the same options as HTTP requests, through the same TLS builder: `ca_bundle` (added to the platform roots, plus the workspace `[http] ca_bundle`), `client_cert`/`client_key` or `client_pkcs12` for mutual TLS, and `insecure: true` to skip verification. `grpc://` targets and `tls.enabled: false` use plaintext HTTP/2 (h2c).

A certificate that cannot be verified fails the call with exit code 3, naming the target:

```text
grpc: TLS error at grpcs://localhost:8443: invalid peer certificate: UnknownIssuer
```

An unreachable server fails with `grpc: cannot connect to <target>: <reason>`, also exit code 3.

### gRPC-Web

`transport: grpc-web` sends the call with the gRPC-Web protocol (`application/grpc-web+proto`) through Rustman's HTTP client, over HTTP/1.1 or HTTP/2, so HTTP settings such as proxies and TLS apply. The call is a `POST` to `http(s)://<host:port>/<package.Service>/<Method>`; the trailers arrive at the end of the body. Binary metadata is sent in base64. A response frame longer than `[http] max_body_bytes` (10 MiB by default) is refused as soon as its length arrives, ending the call with `RESOURCE_EXHAUSTED (8)`; trailer lines may end with `\r\n` or `\n`. No compression is negotiated, so a compressed response frame ends the call with `UNIMPLEMENTED (12)`. Only unary and server-streaming calls fit the protocol; client streaming and bidi fail before sending:

```text
grpc: grpc-web supports only unary and server-streaming calls
```

### Status details

When a call fails with a `grpc-status-details-bin` trailer, it is decoded as `google.rpc.Status` using a descriptor set of the standard `google/rpc` protos bundled with Rustman, and each detail is rendered by type:

```text
INVALID_ARGUMENT (3): invalid product id
  field violation: id: must start with p-
```

| Detail type | Rendered as |
|---|---|
| `BadRequest` | `field violation: <field>: <description>`, one line per violation |
| `ErrorInfo` | `error info: reason <reason>, domain <domain>`, then `key=value` metadata |
| `RetryInfo` | `retry after <delay>` |
| `QuotaFailure` | `quota violation: <subject>: <description>` |
| `PreconditionFailure` | `precondition violation: <type> <subject>: <description>` |
| `ResourceInfo` | `resource: <type> <name> (owner <owner>): <description>` |
| `Help` | `help: <description> <url>` |
| `LocalizedMessage` | `localized message (<locale>): <message>` |
| `DebugInfo` | `debug info: <detail> [<stack entries>]` |
| `RequestInfo` | `request info: <request id> <serving data>` |

A detail of any other type is shown as its type URL plus the encoded value in base64, such as `type.example.com/acme.Mystery: AQID`.

## Assertions and captures

gRPC requests take the [assertions](assertions.md) list, with two gRPC checks:

```yaml
assertions:
  - grpc_status: OK                          # a code name (any case) or number 0-16
  - metadata: {name: x-served-by, equals: catalog}
  - json: {path: $.id, equals: p-1}
  - latency: {total_lt: 500}
captures:
  - {var: product_id, from: json, path: $.id}
  - {var: served_by, from: metadata, name: x-served-by}
```

- `grpc_status` passes when the call ended with that code. A request with no assertions at all expects `grpc_status: OK`, except a stream the client stopped at its [headless bounds](#headless-stream-bounds), which ends `CANCELLED` by the client's own choice and passes. A failure reads `grpc_status: expected OK, got NOT_FOUND (5)`.
- `metadata` takes the operators of `header` (`equals`, `contains`, `matches`, `exists`) and looks at the response headers, then the trailers; names match case-insensitively.
- `json` assertions read the response message of a unary or client-streaming call at `$`, and the received messages of a server-streaming or bidi call at `$.messages`: `- json: {path: "$.messages", length: 3}` passes when 3 messages arrived (after a headless stream bound, over the messages collected).
- `latency` reads `first_byte_ms` (`ttfb_lt`) and `total_ms` (`total_lt`).
- Captures take `from: json` (same document as `json` assertions) and `from: metadata` (`name`: the first value among headers, then trailers).

`status`, `header`, `body` and `schema` assertions are evaluated as for HTTP against a response with no HTTP status (`0`); prefer the gRPC checks.

## In `rustman run`

`rustman run` executes `kind: grpc` files in collection order with the other kinds, sharing variables and captures: an HTTP login can capture a token that a later gRPC request sends as metadata. Without assertions a call passes on `OK` (or when stopped at its stream bounds) and fails on any other status (exit code 1). An expired deadline (`DEADLINE_EXCEEDED (4)`) and a call that ends without a status (unreachable server, TLS error, no schema, unknown field) make the request `errored` (exit code 3). Server-streaming and bidi calls stop at their [headless bounds](#headless-stream-bounds).

Each call is recorded in [history](history.md) with the request as `GRPC <target>/<package.Service/Method>`, its metadata with secrets replaced by their placeholders (`Bearer «redacted:token»`), and the messages sent. The record and the JSON report hold a `grpc` object with the exchange (`protocol: grpc`, `call_type`, `transport`, `status`, `headers`, `trailers`, `messages`, `timeline`, `timings`), redacted like every other output; a call that ended without a status records `error: {kind, message}` instead, with `kind` `config` (the request cannot be sent as written) or `request`. gRPC calls are not replayed by `rustman history replay`.

## In the TUI

The TUI opens `kind: grpc` requests in the [gRPC editor](tui.md#the-grpc-editor): a method browser over the target's schema (with the descriptor cache for offline browsing and `Ctrl+G` to refresh it), a message editor seeded from the picked method's template, the metadata and assertions as YAML, and a response viewer with the messages, the status with its decoded details, headers and trailers, and the message timeline filtered by direction (`d`) and text (`f`). During a bidi call, `Ctrl+R` sends the Message tab on the open stream and `Alt+X` half-closes it. Interactive streams are not bounded by `stream`; they run until the server ends them, the deadline expires, or you cancel them with `Esc`.

## Agent tools

The [agent MCP server](agent.md) registers three gRPC tools when it is built with gRPC support (the default `grpc` feature of `rustman-mcp`; a build without it lists none of them):

| Tool | Arguments | What it does |
|---|---|---|
| `grpc_list` | `target` or `path`, `refresh`, `vars` | The services and methods of a target or saved `kind: grpc` request, with their call types |
| `grpc_describe` | `target` or `path`, `symbol`, `refresh`, `vars` | One service, method (with a request template) or message |
| `grpc_call` | `path`, or `target` with `method`, `message`, `metadata` and `deadline_ms`; `vars` | One unary or server-streaming call, through the runner: assertions, captures and history apply |

Every tool checks the target against the session's allowlist before any traffic, reflection included: a target is checked as the `http` or `https` origin it is reached over (`grpc://127.0.0.1:50051` as `http://127.0.0.1:50051`, `grpcs://` as `https://`, a bare `host:port` by its TLS setting). List those origins in `[agent].allowed_hosts`, or let the active environment's `grpc://` and `grpcs://` values count, as its `http(s)` values do. A target outside the allowlist is refused with `code: "blocked"` and `host not allowed`, and the refusal is in the audit log. Production hosts are refused as for every agent request. Each reach is admitted and paced by the session's `[safety].rate` and `[safety].concurrency`, shared with the other requests of the session.

`grpc_call` follows the read-only default of agents. gRPC has no read verbs (every call is an HTTP/2 POST), so the proto decides: a method marked free of side effects is a read, and agents may call it freely:

```proto
rpc GetProduct(GetProductRequest) returns (Product) {
  option idempotency_level = NO_SIDE_EFFECTS;
}
```

Any other method is a mutation. It is refused with `code: "blocked"` and `mutations not allowed for agents` unless `[agent] allow_mutations = true`, and then the human must confirm it (MCP elicitation; `[agent] require_confirmation = false` accepts it unattended), as for an HTTP `POST`. `grpc_list` shows `read_only: true` on the methods agents may call without that. Nothing is sent before these checks pass.

`grpc_call` refuses client-streaming and bidi methods before sending anything (`code: "blocked"`, `only unary and server-streaming methods are available to agents (shop.v1.Catalog/Chat is bidi)`). Server streams stop at the request's [headless bounds](#headless-stream-bounds) (100 messages or 30 s by default). Results carry the status (with decoded details), the response headers and trailers, the received messages and the assertions, redacted with the session's secrets and capped by `[agent].max_output_bytes` like every agent result: a server echoing a secret shows `«redacted:token»`.

## CLI commands

A target is `host:port`, `grpc://host:port`, `grpcs://host:port` or a `{{variable}}` holding one (resolved in the active environment). Commands that take `<target|request>` also accept a `kind: grpc` request file, id or name, whose target, TLS options and `schema` apply. `--var k=v`, `--env`, `--workspace` and `--insecure` work as for other commands.

### `rustman grpc list`

`rustman grpc list <target|request>` lists every service with its methods, call types and message types; `--json` prints one JSON object per service. Inside a workspace, reflection results are cached; `--refresh` asks the server again. A cached answer prints the stale-data notice on stderr.

```text
$ rustman grpc list grpc://localhost:50051
service shop.v1.Catalog
  GetProduct (unary): shop.v1.GetProductRequest -> shop.v1.Product
  ListProducts (server streaming): shop.v1.ListProductsRequest -> shop.v1.Product
  UploadProducts (client streaming): shop.v1.Product -> shop.v1.UploadSummary
  Chat (bidi): shop.v1.ChatMessage -> shop.v1.ChatMessage
```

### `rustman grpc describe`

`rustman grpc describe <target|request> <symbol>` describes a service, a method (`package.Service/Method`: its call type, request and response types and a request [template](#method-browser-and-templates)) or a message (its fields and template); `--json` prints the description as JSON.

### `rustman grpc call`

`rustman grpc call <request>` sends a `kind: grpc` request; `rustman grpc call --target <t> --method <m> --data <json>` sends an ad-hoc one (`--data` defaults to `{}`; a JSON array sends each entry, for client and bidi streaming). `--metadata "name: value"` (repeatable) and `--deadline-ms` complete an ad-hoc call. Calls go through the runner: variables, captures, assertions, history and redaction apply as in `rustman run`.

The response messages are printed on stdout as indented JSON, and the status (with its decoded details) on stderr. With `--json`, each received message is one JSON line, followed by a final status line:

```text
$ rustman grpc call --target grpc://localhost:50051 --method shop.v1.Catalog/ListProducts --data '{}' --json
{"id":"p-1","name":"Product p-1"}
{"id":"p-2","name":"Product p-2"}
{"id":"p-3","name":"Product p-3"}
{"status":{"code":0,"name":"OK","message":"","details":[]},"headers":[...],"trailers":[...]}
```

### `rustman grpc health`

`rustman grpc health <target> [--service <name>]` calls `grpc.health.v1.Health/Check` (the whole server without `--service`) and prints the serving status: `SERVING`, `NOT_SERVING`, `SERVICE_UNKNOWN` or `UNKNOWN`. `--json` prints `{"status": "<status>"}`; `--timeout` bounds the check (default: `[defaults] timeout_ms`).

### Exit codes

| Command | 0 | 1 | 2 | 3 |
|---|---|---|---|---|
| `rustman grpc call` | the call passed (`OK` without assertions) | another status, or a failed assertion | the call cannot be sent as written: no schema (`server reflection unavailable`), unknown method or field, invalid metadata | no status came back (connect, TLS) or the deadline expired |
| `rustman grpc health` | `SERVING` | `NOT_SERVING`, `SERVICE_UNKNOWN`, `UNKNOWN` | invalid target | the check failed, `UNIMPLEMENTED` included |
| `rustman grpc list` / `describe` | listed | | no schema, unknown symbol | unreachable server with nothing cached |

Ctrl+C stops a call and exits with 130.
