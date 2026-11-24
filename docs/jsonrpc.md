# JSON-RPC

Rustman sends JSON-RPC 2.0 calls, batches, notifications and subscriptions over HTTP and WebSocket to any JSON-RPC service: EVM nodes, Bitcoin Core, Solana, Starknet and LSP-style services. It is chain-agnostic: method names are never interpreted and values are never decoded, so `"0x10"` stays `0x10`. The request file format is in [request-files.md](request-files.md#json-rpc-requests); the behavior is specified in [`openspec/changes/add-jsonrpc-client`](../openspec/changes/add-jsonrpc-client/).

## Envelopes and ids

Each call becomes `{"jsonrpc":"2.0","method":…,"params":…,"id":…}`, members in that order, sent with `Content-Type: application/json`. `params` is left out when the file has none. The minimal file

```yaml
version: 1
kind: jsonrpc
name: Block number
endpoint: '{{rpc_url}}'
method: eth_blockNumber
```

sends exactly `{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}`.

- Without `id`, ids are sequential integers from 1 per connection. Over HTTP every request is its own connection, so a batch carries ids 1, 2, …; over a reused WebSocket the numbering continues.
- An explicit `id` (string or integer) is sent unchanged and takes no sequence number. Sequential ids skip the explicit ids of the same request, so in a batch of `{method: a, id: 1}` and `{method: b}` the second call gets id 2.
- Two calls of a batch with the same explicit id are a load error, `<path>:<line>: duplicate id 1 in batch`: their answers could not be told apart.
- A notification (`notification: true`) carries no `id`. Over HTTP the exchange completes when the HTTP response arrives (`204 No Content` passes); any body is recorded but not parsed.

## Responses

| Response | Outcome |
|---|---|
| `result` | The call succeeded. |
| `error` object | The call failed with `rpc error <code>: <message>`; `code`, `message` and `data` are kept. |
| both or neither member, or `jsonrpc` other than `"2.0"` | Exchange error `invalid JSON-RPC response: <reason>`. |
| another `id` (single call over HTTP) | Exchange error `response id <x> does not match request id <y>`. |
| non-2xx status with a JSON-RPC error body | That error object. |
| non-2xx status with any other body (an HTML gateway page) | Exchange error `HTTP <status>` (exit-code class 3). |

A failed call fails the request. An exchange error means no usable answer came back.

An answer may be at most `[http] max_body_bytes` (default 10 MiB): the HTTP response body, or each WebSocket message. A larger one is refused as soon as the limit is passed, without being parsed or stored: the exchange error `HTTP: the response body is larger than the <n> byte limit (max_body_bytes)`, or `WebSocket: a message is larger than the <n> byte limit (max_body_bytes)` (exit-code class 3). It is not retried, and history records the error but none of the body, so a recorded response is never larger than the limit.

## Batches

A `batch` of 1 to 100 calls goes out as one JSON array (one HTTP request, or one WebSocket frame). Responses are matched to calls by `id`, in any order, and each call keeps its own assertions and captures. A call without a response fails with `no response for id <x>`. A server that answers a batch with a single error object (many public nodes disable batches) fails every call with that error, reported once with the note `server rejected the batch`. The request passes only when every call that expects an answer passes.

## Timeouts and retries

Calls use the `kind: http` timeouts: `settings.timeout_ms` (default 30000) for the response and `settings.connect_timeout_ms` (default 10000) for connecting, per HTTP request and per WebSocket call. A timeout is reported as `timeout after <n> ms (<phase>)`.

JSON-RPC is POST-only, so HTTP semantics cannot tell reads from writes. Retries therefore only apply to files marked `idempotent: true`:

```yaml
idempotent: true
settings:
  retry: {attempts: 2, backoff_ms: 500}
```

`attempts` (default 0) counts the tries after the first. A try is repeated after a connection error, a timeout, HTTP 429, 502, 503 or 504, or error code `-32005` (limit exceeded). The wait starts at `backoff_ms` (default 500) and doubles each time; a `Retry-After` header (seconds or an HTTP date, at most 60 seconds) replaces it. Every attempt is recorded with its status, error and wait. Without `idempotent: true`, nothing is retried.

## WebSocket

For `ws://` and `wss://` endpoints the call goes out as a text frame on a WebSocket opened with the same stack as GraphQL subscriptions: the request's headers, auth and cookies in the handshake, `[http]` proxies (HTTP `CONNECT`, HTTPS, SOCKS5) and TLS settings (`ca_bundle`, client certificates, `verify_tls`). The answer is the frame whose `id` matches; unrelated frames, such as subscription notifications, are skipped. Outside `rustman run` (`rustman rpc call`, `rpc batch` and the TUI), the connection closes when the exchange ends. Within one `rustman run`, files with the same endpoint, handshake headers, TLS settings and proxy share one connection. Status and header assertions are not available over WebSocket.

## Subscriptions

JSON-RPC has no standard subscription shape, so a subscription names three methods:

```yaml
version: 1
kind: jsonrpc
name: New heads
endpoint: wss://{{node_host}}/ws
params: [newHeads]
subscription:
  subscribe: eth_subscribe
  unsubscribe: eth_unsubscribe
  notification: eth_subscription
  max_events: 3
assertions:
  - json: {path: $.events, length: 3}
```

Rustman calls `subscribe` with `params`, takes its result as the subscription id, and records every `notification` message whose id (at `notification_id_path`, default `$.params.subscription`) equals it as an event. Headless (`rustman run`, `rustman rpc call`), the subscription ends after `max_events` events (default 10) or `max_duration_ms` (default 30000), whichever comes first; then `unsubscribe` is called with the id. Events are assertable as a list at `$.events`, next to the subscribe call's `$.result`. The same triple covers Solana (`accountSubscribe` / `accountNotification` / `accountUnsubscribe`, with `notification_id_path: $.params.subscription`) and Starknet.

A subscription over an `http://` or `https://` endpoint fails to load with `subscriptions require a ws:// or wss:// endpoint`.

## Assertions and captures

Assertions and captures evaluate against the whole response envelope (not just `result`), so `$.result` addresses the result and `$.error` the error object, exactly as `--raw` prints them. In a batch, each call's assertions and captures read that call's own envelope, and failures name the call (`call 2 (eth_blockNumber): …`). Status and header assertions read the HTTP response; over WebSocket they are rejected at load time. Values are kept as received: capturing `$.result` of `eth_blockNumber` gives `0x12a05f2`, not a number.

```yaml
assertions:
  - json: {path: $.result, type: string}
  - rpc_error: none
captures:
  - {var: head, from: json, path: $.result}
```

`rpc_error` checks how the call ended:

| Assertion | Passes when |
|---|---|
| `rpc_error: {code: -32602}` | the call failed with error code `-32602` |
| `rpc_error: {code: -32602, message_contains: invalid}` | … and its message contains `invalid` (interpolated) |
| `rpc_error: none` | the call returned a `result` |

Its failures read `rpc_error: expected code -32602, got result` and `rpc_error: expected code -32602, got code -32601`. A call that fails with an error object fails the request unless it has an `rpc_error: {code: …}` assertion; with one, the assertion decides.

### Snapshots

A [`snapshot` assertion](snapshots.md) records how a call answered and compares later runs against it. The snapshot body is the call's response envelope, the same document other assertions read: `jsonrpc`, `id` and either `result` or the `error` object. Over HTTP, the status is compared too. For a subscription, the body is the list of events collected instead. In a batch, each call's snapshot holds that call's own envelope, matched by id. Each snapshot is a file named after the request file and the snapshot name, so names must be unique across all calls of a batch. A repeated name fails to load with `duplicate snapshot name 'default' (give each call's snapshot its own name)`.

```yaml
# chain/head-block.rman.yaml
version: 1
kind: jsonrpc
name: Head block
endpoint: '{{rpc_url}}'
batch:
  - method: eth_chainId
    assertions:
      - snapshot: {name: chain}
  - method: eth_getBlockByNumber
    params: ['0x1', false]
    assertions:
      - snapshot: {name: block, ignore: [$.result.timestamp]}
```

`rustman run`, `rustman rpc call` and `rustman rpc batch` record, compare and update these snapshots like any other: `--update-snapshots` (`-u`) rewrites one that differs, and `--ci` (or `CI=true`) never writes one. Agents compare in CI mode. `rustman snapshot list` and `prune` include them.

## History

Every call, batch and subscription is recorded in [history](history.md) as one exchange. Next to the usual `exchange` (the HTTP request and response, for HTTP endpoints), the record's `jsonrpc` part holds the transport, each call's envelope sent and received with its outcome (`result`, `error`, `notification`, `failed`), every retry attempt (status, error, wait), a subscription's id, event count and first 1000 events, and the note `server rejected the batch` when it applies. Secrets are redacted everywhere, including inside params: a param `{{api_key}}` is stored as `«redacted:api_key»`. HTTP exchanges can be replayed; WebSocket ones cannot.

## OpenRPC catalogs

An endpoint can have an [OpenRPC](https://spec.open-rpc.org/) document listing its methods, param names and schemas, and result schemas. Link a file in `rustman.toml` (JSON, or YAML for `.yaml` / `.yml`), keyed by the endpoint as request files write it or as it resolves:

```toml
[jsonrpc.catalogs]
"{{rpc_url}}" = "openrpc/node.json"
```

or fetch it from a node that implements `rpc.discover`:

```console
$ rustman rpc discover {{rpc_url}} --env dev
42 methods cached in .rustman/cache/openrpc/3f1c….json
```

The cache file is named by the SHA-256 of the resolved endpoint (scheme, host, port, path and query, so `?network=main` and `?network=test` get catalogs of their own), so an API key in the URL never appears in a file name, and the document is stored with secrets redacted. A node answering `rpc.discover` with `-32601` makes the command exit 1 with `endpoint does not support rpc.discover; provide an OpenRPC file`.

With a catalog, every call is checked before anything is sent, and each violation is printed with its param path:

- `unknown method 'eth_blockNumbr' (did you mean eth_blockNumber?)`
- `params[0]: expected string, got integer` (positional params; named params read `params.<name>`)
- `params.address: missing required param 'address'`
- any other schema violation, as `params[1]: <reason>`

A refused send exits 2. `settings.validate: false` in a request file, or `--no-validate`, skips the check. Without a catalog, nothing is validated.

## rustman rpc call

`rustman rpc call <endpoint|request> [method] [params-json]` sends one call and prints its result, or its error object, as pretty JSON. The target is an endpoint (`http`, `https`, `ws`, `wss`, or a `{{variable}}` holding one) with a method and optional params (a JSON array or object), or a `kind: jsonrpc` request file, id or name (whose assertions are checked).

```console
$ rustman rpc call http://127.0.0.1:8332 getblockcount --header "Authorization: Basic {{btc_auth}}" --env dev
800000
$ rustman rpc call {{rpc_url}} eth_getBalance '["0x00000000000000000000000000000000000000aa", "latest"]'
0 ETH
$ rustman rpc call wss://node.example/ws eth_subscribe '["newHeads"]' \
    --subscribe eth_subscription --unsubscribe eth_unsubscribe --max-events 3
```

| Flag | Meaning |
|---|---|
| `--header`, `-H "Name: value"` | Add a header (repeatable, interpolated) |
| `--id <id>` | Send this id (an integer, or else a string) |
| `--notify` | Send a notification: no id, nothing printed |
| `--subscribe <notification> --unsubscribe <method>` | Subscribe with the method, print the subscription id and then each event (one per line) |
| `--max-events <n>`, `--duration <d>` | Bound a subscription (defaults: the request's, else 10 events and 30 s) |
| `--raw` | Print the response envelopes (and event messages) as received |
| `--json` | Print the whole exchange (status, calls, attempts, events, assertions) as one JSON object |
| `--no-validate` | Skip the OpenRPC check |
| `--param name=value` | A typed param of a built-in `eth_*`, `net_*` or `web3_*` method (repeatable), instead of the params JSON; see [Typed JSON-RPC methods](evm.md#typed-json-rpc-methods) |
| `--env`, `--workspace`, `--var`, `--timeout`, `--insecure`, `--no-history` | As for `rustman send` |

A native amount (`eth_getBalance`, `eth_gasPrice`) prints as `1 ETH`, its wei, gwei and hex forms on stderr; `--raw` and `--json` keep the hex. Outside a workspace, endpoint calls still work (history goes to the platform data folder). Every output comes from the redacted record, so no secret is ever printed.

## rustman rpc batch

`rustman rpc batch <endpoint> <file>` reads calls from a JSON array, a YAML list, or JSON lines (one `{"method": …, "params": …}` per line); `-` reads stdin. The calls are sent as one batch (more than 100 are split into consecutive batches of 100) and one line is printed per call, in input order: the result, or `{"error": {…}}`. `--json` prints them as one array; `--header` and `--no-validate` work as for `rpc call`.

```console
$ printf '{"method":"eth_chainId"}\n{"method":"eth_blockNumber"}\n' | rustman rpc batch {{rpc_url}} -
"0x1"
"0x12a05f2"
```

## Exit codes

| Code | `rpc call` / `rpc batch` | `rpc discover` |
|---|---|---|
| 0 | Every call returned a result (and every assertion passed) | The catalog was cached |
| 1 | A call returned a JSON-RPC error, or an assertion failed | The node answered with an error (`-32601`: no `rpc.discover`) or not with an OpenRPC document |
| 2 | Bad flags or input, or the OpenRPC catalog refused a call | Bad flags, no workspace |
| 3 | No usable answer: network, TLS, timeout, `HTTP <status>`, an invalid response | Same |
| 4 | A method guard refused a call (nothing was sent) | |
| 130 | Ctrl+C | Ctrl+C |

## In `rustman run`

`rustman run` sends `kind: jsonrpc` files like any other request, in collection order, with captures shared across files and one WebSocket connection per endpoint and handshake headers for the whole run. Subscriptions are headless: they stop at `max_events` / `max_duration_ms`, which `--max-events` and `--duration` replace for every subscription of the run. The JSON report (`--reporter json`, `rustman.report/v1`) gives every JSON-RPC request a `jsonrpc` member next to `exchange`: the transport (`http` or `ws`), whether it was a batch, every call with the envelope sent, the envelope that answered it, its outcome (`result`, `error`, `notification` or `failed`) and failure, every HTTP attempt, a subscription's id, events (the first 1000), event count, why it ended and whether it unsubscribed, and a `refusal` (`validation` or `guard`) when nothing was sent. Every value in it is redacted. For a WebSocket exchange `exchange` is `null`: the `jsonrpc` member is the record. A run exits 3 when a request errored and 4 when the errors include a call a method guard refused.

```json
"jsonrpc": {
  "transport": "http",
  "batch": false,
  "calls": [{
    "method": "eth_blockNumber", "id": 1,
    "request": {"jsonrpc": "2.0", "method": "eth_blockNumber", "id": 1},
    "response": {"jsonrpc": "2.0", "id": 1, "result": "0x12a05f2"},
    "outcome": "result"
  }],
  "attempts": [{"status": 200, "error": null, "elapsed_ms": 1.4}],
  "elapsed_ms": 1.45
}
```

CI runs a fixture collection (a call, a batch, a subscription and an error assertion) against in-process stubs on every OS, and against a local [Anvil](https://getfoundry.sh/anvil/) node (`--block-time 1`) in the `jsonrpc-anvil` job: set `RUSTMAN_ANVIL_URL=http://127.0.0.1:8545` to run that part locally (`cargo test -p rustman-cli --test rpc_run`).

## In the TUI

A `kind: jsonrpc` request opens in its own editor (see [Terminal UI](tui.md#the-json-rpc-editor)): the endpoint on the URL row (`RPC`, or `SUB` for a subscription), and the tabs `Call` (the method, completed from the endpoint's OpenRPC catalog with each method's param names, and the params as JSON), `Batch`, `Headers`, `Auth` and `Assertions` (each that part of the file as YAML). The response viewer adds the tabs `Result`, `Error` (code, message and `data` of each failed call) and `Events`. A subscription runs until you stop it: events are listed live with a counter, and `Esc` sends `unsubscribe` and stops it, keeping the events.

## Method guards

Capabilities built on this one (such as the EVM toolkit) can register method guards, checked before any `jsonrpc` request is sent from any face: `rustman rpc`, `rustman run`, the TUI and the agent tool. For a subscription, guards are also asked about its `subscribe` and `unsubscribe` methods before anything is sent. A guard that refuses a method stops the whole request with its own message, sends nothing, and exits 4; the history and report record show `refusal: guard`. Guards win over the request file, over `--no-validate`, and over `agent_read_only_methods`.

## Agent tool

The [agent MCP server](agent.md) registers the `rpc_call` tool. It takes either `path` (a saved `kind: jsonrpc` request) or `endpoint` with `method` and optional `params` (a JSON array or object), plus `vars` and `timeout_ms`, and returns each call's `result` or `error` object, redacted, with the request's assertions. Because JSON-RPC methods are not classified as reads or writes, an agent may only call methods matching a glob of `[jsonrpc] agent_read_only_methods`; the list is empty by default, so nothing is allowed until you add globs:

```toml
[jsonrpc]
agent_read_only_methods = ["eth_get*", "eth_blockNumber", "eth_chainId", "@evm-read"]
```

Entries starting with `@` are presets other capabilities register (for example `@evm-read`); an unknown one stops the agent server at start with `unknown method preset '@solana-read' (registered: …)` and exit code 2. Each call is checked in this order, and nothing is sent unless all pass: the method guards (their own message), the safety policy (the endpoint's host must be in `[agent] allowed_hosts`), subscriptions (refused: `subscriptions are not available to agents`), then the allowlist (`method not marked read-only for agents`). A refusal is a tool error with `code: "blocked"` and that message; an unknown preset is `code: "invalid_input"`. Allowed calls go through the session's governed sender: every POST and redirect hop is checked against the agent allowlist and the production rule, and paced by `[safety].rate` and `[safety].concurrency`, which `rpc_call` shares with the other requests of the session. A call to a `ws://` or `wss://` endpoint is admitted and paced by the same limiter before it is sent. The HTTP method rule of `[agent]` doesn't apply: every JSON-RPC call is a POST, and `agent_read_only_methods` takes its place. Calls are recorded in history like any other.
