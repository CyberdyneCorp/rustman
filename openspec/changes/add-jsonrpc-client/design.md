# Design

## Context

`bootstrap-core-engine` provides the request-kind registry (`workspace`), the hyper-based transport (`rustman-http`), the assertion engine, the runner and history. `add-terminal-ui` provides the pane registry, and `add-agent-mcp-server` (capability `agent-mcp-interface`) provides conditional tools. This change adds one kind on top of those. See proposal.md for the motivation.

## Goals / Non-Goals

**Goals:**
- One chain-agnostic JSON-RPC 2.0 client crate that `add-evm-toolkit` (and possibly future Solana or Bitcoin presets) can wrap without forking.
- The same exchange model as `kind: http`, so history, reports, snapshots and viz work unchanged.

**Non-Goals:**
- Typed method bindings for any chain.
- Connection pooling across separate CLI invocations.
- A JSON-RPC server.

## Decisions

### D1. Own a thin envelope layer over `rustman-http` instead of using `jsonrpsee`
`jsonrpsee` is a full client and server framework. It hides the raw envelope, owns its own HTTP stack (and so bypasses our timing breakdown, proxies, mTLS and redaction), and makes it awkward to send deliberately malformed envelopes or to record the exact bytes. We need byte-exact recording and tolerance of non-conformant servers, so we write a ~500-line envelope module: serialize, match ids, classify responses. HTTP goes through `rustman-http`, so every HTTP feature applies.
- *Rejected:* `jsonrpsee`, for the reasons above. `jsonrpc-core`, because it's unmaintained.

### D2. WebSocket via `tokio-tungstenite` + rustls
This is the same crate `add-graphql-client` uses for subscriptions, so there's one WebSocket stack in the workspace. The handshake reuses `rustman-http`'s TLS config (custom CA, mTLS) and proxy CONNECT support. A per-run connection cache is keyed by `(endpoint, resolved headers)`.
- *Rejected:* `fastwebsockets`, because it lacks the higher-level message API and its rustls glue isn't mature.

### D3. Envelope-wide JSONPath root
Assertions evaluate against the whole envelope (`$.result`, `$.error`) instead of rebasing on `result`. That keeps one mental model for success and error paths, and it matches what users see in raw mode. Batches evaluate per item against that item's envelope.

### D4. Subscriptions as a generic triple
JSON-RPC has no standard subscription shape. `{subscribe, notification, unsubscribe}` plus the convention "subscription id = subscribe result; notifications carry it at `params.subscription`" covers Ethereum, Solana (`accountSubscribe` / `accountNotification`) and Starknet. A `notification_id_path` override (default `$.params.subscription`) handles servers that put the id elsewhere.

### D5. OpenRPC catalogs with `jsonschema`
Catalogs are parsed with `serde_json` into a small internal model. Param schemas are validated with the `jsonschema` crate, which `assertions` already uses for JSON Schema assertions. Fuzzy "did you mean" suggestions use `strsim`.

### D6. Retry only on explicit idempotency
JSON-RPC is POST-only, so HTTP semantics can't tell reads from writes. Automatic retries could duplicate a write (e.g. `sendrawtransaction`). So we require `idempotent: true` on the request file. `add-evm-toolkit` sets it for known read methods.

### D7. Agent read-only allowlist by glob
Because JSON-RPC methods aren't classified as reads or writes, agents may only call methods matching `[jsonrpc] agent_read_only_methods` globs (empty by default, so nothing is allowed). This keeps the agent interface safe by default.

## Risks / Trade-offs

- [Servers that answer batches with a non-array] → treated as a batch-level error applied to every call, shown once with the note `server rejected the batch`.
- [Huge subscription streams in the TUI] → the event list is a ring of the newest 10,000 events, and history is capped at 1000 events per exchange.
- [Hex values look "wrong" to humans] → this is intentional for chain-agnosticism. `add-evm-toolkit` adds decoded views.
- [Per-run WebSocket reuse leaks state between files] → reuse only happens within one run, and each subscription unsubscribes when it ends.

## Migration Plan

Additive. Workspaces without `jsonrpc` files are unaffected. Builds without this change report `kind: jsonrpc` files as unknown kinds, per the registry rule.

## Open Questions

- Whether to support IPC transports (Unix socket or Windows named pipe) for local nodes later. This doesn't affect the specs here.
