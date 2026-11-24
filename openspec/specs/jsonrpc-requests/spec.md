# jsonrpc-requests Specification

## Purpose
Lets users define, send and test JSON-RPC 2.0 calls, batches, notifications and subscriptions over HTTP and WebSocket against any JSON-RPC service (EVM nodes, Bitcoin Core, Solana and others). Chain-specific behavior belongs elsewhere.

## Requirements

### Requirement: JSON-RPC request kind
The system SHALL register `kind: jsonrpc` in the `workspace` request-kind registry. A `jsonrpc` file SHALL support `endpoint` (an `http`, `https`, `ws` or `wss` URL), `method` (a non-empty string), optional `params`, optional `id`, `notification` (default `false`), plus `headers`, `auth` and `settings` with the same meaning as for `kind: http`.

#### Scenario: Minimal call
- **GIVEN** a file with `kind: jsonrpc`, `endpoint: "{{rpc_url}}"`, `method: eth_blockNumber`
- **WHEN** the request is sent
- **THEN** the body SHALL be `{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}` with `Content-Type: application/json`

#### Scenario: Missing method
- **GIVEN** a `jsonrpc` file without `method`
- **WHEN** the workspace loads
- **THEN** loading SHALL fail with `<path>:<line>: jsonrpc request requires 'method'`

### Requirement: Positional and named params
`params` SHALL be either a YAML sequence, sent as a JSON array, or a YAML mapping, sent as a JSON object. Omitted `params` SHALL omit the member from the envelope. Scalars SHALL be rejected at load time. Variables SHALL be interpolated inside params values before serialization.

#### Scenario: Positional params
- **GIVEN** `method: getblockhash` and `params: [800000]`
- **WHEN** the request is sent
- **THEN** the envelope SHALL contain `"params":[800000]`

#### Scenario: Named params
- **GIVEN** `params: {address: "{{wallet}}", commitment: finalized}` and `wallet=Abc1`
- **THEN** the envelope SHALL contain `"params":{"address":"Abc1","commitment":"finalized"}`

#### Scenario: Scalar params rejected
- **GIVEN** `params: 5`
- **WHEN** the workspace loads
- **THEN** loading SHALL fail with `<path>:<line>: params must be a list or a map`

### Requirement: Request id strategy
When `id` is omitted, the system SHALL assign sequential integer ids starting at 1 per connection. An explicit `id` (string or integer) SHALL be sent unchanged, and assigned ids SHALL skip the explicit ids of the same request. Two calls of one batch with the same explicit id SHALL be a load error (`duplicate id <id> in batch`). A response whose `id` doesn't match the request SHALL produce the error `response id <x> does not match request id <y>`.

#### Scenario: Explicit string id
- **GIVEN** `id: "req-7"`
- **THEN** the envelope SHALL contain `"id":"req-7"`

#### Scenario: Mismatched id
- **GIVEN** a server answering a request with `id: 1` using `id: 2`
- **THEN** the exchange SHALL be an error with `response id 2 does not match request id 1`

### Requirement: Notifications
With `notification: true`, the envelope SHALL omit `id`, and the system SHALL NOT wait for a JSON-RPC response. Over HTTP the exchange SHALL complete when the HTTP response arrives, and any body SHALL be recorded but not parsed as a result. `result` assertions on a notification SHALL be rejected at load time.

#### Scenario: Fire-and-forget over HTTP
- **GIVEN** `notification: true` and a server replying `204 No Content`
- **WHEN** the request is sent
- **THEN** the exchange SHALL pass with no result

#### Scenario: Result assertion on notification
- **GIVEN** `notification: true` and an assertion on `$.result`
- **THEN** loading SHALL fail with `<path>:<line>: notifications have no result to assert on`

### Requirement: Success and error responses
A response with `result` SHALL be a successful call. A response with an `error` object SHALL be a failed call whose `code`, `message` and `data` are shown. A response with both or neither member, or `jsonrpc` other than `"2.0"`, SHALL produce `invalid JSON-RPC response: <reason>`. A failed call SHALL fail the request unless it has an `rpc_error` assertion.

#### Scenario: Error object
- **GIVEN** a response `{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}`
- **THEN** the request SHALL fail with `rpc error -32601: Method not found`

#### Scenario: Malformed response
- **GIVEN** a response with both `result` and `error`
- **THEN** the exchange SHALL be an error with `invalid JSON-RPC response: both result and error present`

### Requirement: HTTP transport errors
For HTTP endpoints, a non-2xx status whose body isn't a valid JSON-RPC response SHALL be an exchange error `HTTP <status>` (exit-code class 3). A non-2xx status carrying a valid JSON-RPC error object SHALL be treated as that error object.

#### Scenario: Gateway error
- **GIVEN** an endpoint returning `502` with an HTML body
- **THEN** the exchange SHALL be an error `HTTP 502` and `rustman run` SHALL exit 3

### Requirement: Response size limit
An HTTP response body or a WebSocket message larger than `[http] max_body_bytes` SHALL be refused without being parsed or stored in full: the exchange error SHALL be `HTTP: the response body is larger than the <n> byte limit (max_body_bytes)` or `WebSocket: a message is larger than the <n> byte limit (max_body_bytes)` (exit-code class 3). The refused exchange SHALL NOT be retried, and history SHALL record the error without the body.

#### Scenario: Oversized HTTP answer
- **GIVEN** `[http] max_body_bytes = 4096` and an endpoint answering with a 64 KiB JSON-RPC body
- **WHEN** the call is sent
- **THEN** the exchange SHALL be an error `HTTP: the response body is larger than the 4096 byte limit (max_body_bytes)`, `rustman rpc call` SHALL exit 3, and the history entry SHALL NOT contain the body

#### Scenario: Oversized WebSocket message
- **GIVEN** `[http] max_body_bytes = 4096` and a WebSocket endpoint answering with a 64 KiB message
- **THEN** the exchange SHALL be an error `WebSocket: a message is larger than the 4096 byte limit (max_body_bytes)`

### Requirement: rpc_error assertion
The `assertions` list SHALL accept `rpc_error: {code: <int>}`, optionally with `message_contains: <text>`. It SHALL pass when the call failed with that code (and message substring), and SHALL fail with `rpc_error: expected code <c>, got result` or `got code <d>` otherwise. `rpc_error: none` SHALL assert success.

#### Scenario: Expected error
- **GIVEN** `- rpc_error: {code: -32602}` and a response with error code `-32602`
- **THEN** the assertion SHALL pass and the request SHALL pass

#### Scenario: Unexpected success
- **GIVEN** `- rpc_error: {code: -32602}` and a response with a `result`
- **THEN** the assertion SHALL fail with `rpc_error: expected code -32602, got result`

### Requirement: Result assertions and captures
JSONPath assertions and `from: json` captures from `assertions` SHALL evaluate against the full response envelope, so `$.result...` addresses the result and `$.error...` the error. Status and header assertions SHALL apply to the HTTP response for HTTP endpoints and SHALL be rejected at load time for WebSocket endpoints.

#### Scenario: Capture block number
- **GIVEN** `captures: [{var: head, from: json, path: "$.result"}]` and result `"0x12a05f2"`
- **THEN** `head` SHALL equal `0x12a05f2` for later requests

#### Scenario: Status assertion over WebSocket
- **GIVEN** a `wss://` endpoint and `- status: 200`
- **THEN** loading SHALL fail with `<path>:<line>: status assertions are not available over WebSocket`

### Requirement: Batch requests
A file MAY define `batch:` as a list of 1 to 100 calls (each with `method`, optional `params`, `id`, `notification`, `assertions`, `captures`) instead of a single call. They SHALL be sent as one JSON array. More than 100 entries SHALL fail to load with `batch exceeds 100 calls`.

#### Scenario: Two-call batch
- **GIVEN** a batch with `eth_chainId` and `eth_blockNumber`
- **WHEN** the request is sent
- **THEN** one HTTP request SHALL carry a JSON array of two envelopes with ids 1 and 2

### Requirement: Batch result matching
Batch responses SHALL be matched to calls by `id`, independent of order. Each call's assertions SHALL run against its own response. A call with no matching response SHALL fail with `no response for id <x>`. The batch request SHALL pass only if every non-notification call passes.

#### Scenario: Out-of-order responses
- **GIVEN** a two-call batch and a server returning the response for id 2 before id 1
- **THEN** each call's assertions SHALL be evaluated against its own response and both SHALL pass

#### Scenario: Missing item
- **GIVEN** a two-call batch and a server returning only the response for id 1
- **THEN** call 2 SHALL fail with `no response for id 2` and the request SHALL fail

#### Scenario: Server rejects batches
- **GIVEN** a server answering a batch with a single error object
- **THEN** every call SHALL fail with that error, shown once with the note `server rejected the batch`

### Requirement: WebSocket transport
For `ws` and `wss` endpoints, the system SHALL open one WebSocket connection, send the envelope as a text frame and match the response by id, ignoring unrelated frames. Outside `rustman run`, the connection SHALL close when the exchange ends. Within one `rustman run`, files with the same endpoint, handshake headers, TLS settings and proxy SHALL reuse one connection; files that differ in any of them SHALL NOT.

#### Scenario: Unrelated frame ignored
- **GIVEN** a server that sends a subscription notification before the response to id 1
- **THEN** the call SHALL complete with the response to id 1

#### Scenario: Connections with different TLS settings
- **GIVEN** two files in one run for the same `wss` endpoint and headers, one with `verify_tls: false`
- **THEN** they SHALL use two connections

### Requirement: Subscriptions
A file MAY define `subscription: {subscribe, unsubscribe, notification}` method names over a WebSocket endpoint. The system SHALL call `subscribe` with `params`, take the result as the subscription id, collect `notification` messages carrying that id, then call `unsubscribe` with the id when the subscription ends.

#### Scenario: Ethereum new heads
- **GIVEN** `subscription: {subscribe: eth_subscribe, unsubscribe: eth_unsubscribe, notification: eth_subscription}` and `params: [newHeads]`
- **WHEN** the request runs
- **THEN** each `eth_subscription` message whose `params.subscription` equals the returned id SHALL be recorded as an event

#### Scenario: Subscription over HTTP rejected
- **GIVEN** a `subscription` block with an `https://` endpoint
- **THEN** loading SHALL fail with `<path>:<line>: subscriptions require a ws:// or wss:// endpoint`

### Requirement: Headless subscription bounds
In `rustman run` and `rustman rpc call`, a subscription SHALL end after `max_events` events (default 10) or `max_duration_ms` (default 30000), whichever comes first, then unsubscribe. Events SHALL be assertable as a list at `$.events`. In the TUI, a subscription SHALL run until the user stops it.

#### Scenario: Event bound
- **GIVEN** `max_events: 3` and a node emitting a block every second
- **WHEN** the request runs headless
- **THEN** it SHALL record 3 events, call `eth_unsubscribe` and complete in about 3 seconds

#### Scenario: Assertion on events
- **GIVEN** `- json: {path: "$.events", length: 3}`
- **THEN** the assertion SHALL pass after 3 events

### Requirement: Timeouts
JSON-RPC calls SHALL use the total and connect timeouts defined by `http-requests` (default 30000 ms and 10000 ms) for HTTP and for each WebSocket call. Subscription waiting time SHALL be governed only by the subscription bounds. A timeout SHALL produce `timeout after <n> ms (<phase>)`.

#### Scenario: Slow node
- **GIVEN** `settings.timeout_ms: 2000` and a node that doesn't answer
- **THEN** the exchange SHALL be an error `timeout after 2000 ms (response)`

### Requirement: Retry for idempotent calls
A file MAY set `settings.retry: {attempts, backoff_ms}` (defaults 0 and 500). Retries SHALL apply only to calls marked `idempotent: true`. They SHALL be triggered by connection errors, timeouts, HTTP 429/502/503/504 or error code `-32005` (limit exceeded), with exponential backoff that honors `Retry-After`. Each attempt SHALL appear in history.

#### Scenario: Rate-limited read retried
- **GIVEN** `idempotent: true`, `retry: {attempts: 2}` and a node returning 429 once, then a result
- **THEN** the call SHALL succeed on the second attempt and history SHALL show two attempts

#### Scenario: Non-idempotent not retried
- **GIVEN** `retry: {attempts: 2}` without `idempotent: true` and a timeout
- **THEN** the call SHALL fail without retrying

### Requirement: OpenRPC method catalog
An endpoint MAY be linked to an OpenRPC document via `rustman.toml [jsonrpc.catalogs]` (a file path) or fetched with `rustman rpc discover <endpoint>`, which calls `rpc.discover` and caches the result in `.rustman/cache/openrpc/`. The catalog SHALL provide method names, param names and schemas, and result schemas.

#### Scenario: Discover and cache
- **GIVEN** a node that implements `rpc.discover`
- **WHEN** the user runs `rustman rpc discover {{rpc_url}}`
- **THEN** the document SHALL be cached and the number of methods printed

#### Scenario: Discover unsupported
- **GIVEN** a node answering `rpc.discover` with error `-32601`
- **THEN** the command SHALL exit 1 with `endpoint does not support rpc.discover; provide an OpenRPC file`

### Requirement: Param validation against the catalog
When a catalog is present, the system SHALL validate method name, required params and param schemas before sending and print each violation with its param path. `settings.validate: false` or `--no-validate` SHALL skip validation. Without a catalog, no validation SHALL happen.

#### Scenario: Unknown method
- **GIVEN** a catalog without `eth_blockNumbr`
- **WHEN** the user sends `method: eth_blockNumbr`
- **THEN** sending SHALL be refused with `unknown method 'eth_blockNumbr' (did you mean eth_blockNumber?)` and exit code 2

#### Scenario: Wrong param type
- **GIVEN** a catalog requiring `params[0]` to be a string and `params: [42]`
- **THEN** sending SHALL be refused with `params[0]: expected string, got integer`

### Requirement: rpc call command
`rustman rpc call <endpoint|request-file> [method] [params-json]` SHALL send one call and print the result (or error object) as pretty JSON, or the raw envelope with `--raw`. It SHALL accept `--env`, `--var`, `--header`, `--id`, `--notify`, `--subscribe <notification-method> --unsubscribe <method>` and `--json`. Exit codes SHALL follow the ROADMAP table.

#### Scenario: Bitcoin Core call
- **WHEN** the user runs `rustman rpc call http://127.0.0.1:8332 getblockcount --header "Authorization: Basic {{btc_auth}}"`
- **THEN** the block count SHALL be printed and the exit code SHALL be 0

#### Scenario: Error result exit code
- **WHEN** a call returns a JSON-RPC error
- **THEN** the error object SHALL be printed to stdout and the exit code SHALL be 1

### Requirement: rpc batch command
`rustman rpc batch <endpoint> <file>` SHALL read calls from a JSON array, a YAML list or JSON lines (one `{method, params}` per line, `-` for stdin), send them as one batch, and print results in input order. More than 100 calls SHALL be split into consecutive batches of 100.

#### Scenario: Piped batch
- **GIVEN** stdin with three JSON lines
- **WHEN** the user runs `rustman rpc batch {{rpc_url}} -`
- **THEN** one batch of three calls SHALL be sent and three results printed in input order

### Requirement: TUI JSON-RPC pane
The system SHALL register a `jsonrpc` pane through the `tui` protocol pane extension point. It SHALL have tabs Call (method with catalog completion, params editor), Batch, Headers, Auth and Assertions. The viewer SHALL show Result, Error (code, message, data), Events for subscriptions, and Timing.

#### Scenario: Method completion
- **GIVEN** an endpoint with a cached OpenRPC catalog
- **WHEN** the user types `eth_get` in the method field
- **THEN** matching methods SHALL be suggested with their param names

#### Scenario: Live subscription
- **GIVEN** a running `newHeads` subscription in the TUI
- **THEN** events SHALL be appended live with a counter, and `Esc` SHALL unsubscribe and stop it

### Requirement: Chain-agnostic behavior
The `jsonrpc` kind SHALL NOT interpret method names or decode values: hex quantities, base58 strings and numbers SHALL be shown exactly as received. Chain-specific decoding SHALL come only from other capabilities that build on this one.

#### Scenario: Hex left as is
- **GIVEN** a result `"0x10"`
- **THEN** the viewer and captures SHALL show `0x10`, not `16`

### Requirement: Agent read-only tool
When the build includes `agent-mcp-interface`, the system SHALL register an `rpc_call` tool through its conditional-tool mechanism. It SHALL accept an endpoint or request file, method and params, SHALL pass every call through the safety policy (endpoint host must be allowlisted), and SHALL refuse subscriptions and calls not marked read-only.

#### Scenario: Host outside allowlist
- **GIVEN** an agent calling `rpc_call` with an endpoint host not in `[agent] allowed_hosts`
- **THEN** the tool SHALL return the safety policy refusal and send nothing

#### Scenario: Method outside the allowlist
- **GIVEN** `[jsonrpc] agent_read_only_methods = ["getblock*"]`
- **WHEN** an agent calls `sendtoaddress`
- **THEN** the tool SHALL refuse with `method not marked read-only for agents`

#### Scenario: Guard wins over the allowlist
- **GIVEN** `[jsonrpc] agent_read_only_methods = ["eth_*"]` and `add-evm-toolkit` is present
- **WHEN** an agent calls `eth_sendRawTransaction`
- **THEN** the tool SHALL refuse with the method guard's error, because guards are evaluated before the allowlist

### Requirement: Method guards
Other capabilities SHALL be able to register method guards that are evaluated before any `jsonrpc` request is sent, from any face (TUI, CLI, agent). A guard that refuses a method SHALL stop the send with its own message and exit code 4. Guards SHALL take precedence over `agent_read_only_methods` and over any request-file setting.

#### Scenario: Registered guard refuses
- **GIVEN** a guard registered for `eth_sendRawTransaction`
- **WHEN** a user runs `rustman rpc call https://rpc.example eth_sendRawTransaction '["0x..."]'`
- **THEN** nothing SHALL be sent, the guard's message SHALL be printed, and the exit code SHALL be 4

### Requirement: Allowlist presets
Entries in `agent_read_only_methods` starting with `@` SHALL be presets that other capabilities register (for example `@evm-read`). An unknown preset SHALL be a configuration error (exit 2) naming the preset and the registered ones.

#### Scenario: Unknown preset
- **GIVEN** `agent_read_only_methods = ["@solana-read"]` and no capability registers it
- **WHEN** the agent server starts
- **THEN** it SHALL fail with `unknown method preset '@solana-read'` and exit code 2

### Requirement: Exchange recording and redaction
Every call, batch and subscription SHALL be recorded in `request-history` as one exchange containing the envelope(s), responses, events (capped at 1000 per exchange) and timings, with secrets redacted as required by `environments`, including secrets inside params and auth headers.

#### Scenario: Redacted params
- **GIVEN** params containing `{{api_key}}` where `api_key` is a secret
- **THEN** the stored exchange SHALL show `«redacted:api_key»` in the params
