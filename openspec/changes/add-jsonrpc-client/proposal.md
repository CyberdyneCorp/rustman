# Proposal

## Why

Many important APIs speak plain JSON-RPC 2.0 over HTTP or WebSocket instead of REST or GraphQL: Ethereum and other EVM nodes, Bitcoin Core, Solana, Starknet, and LSP-style services. Sending these as `kind: http` with a hand-written envelope loses everything that makes JSON-RPC usable: id matching, batches, error objects, subscriptions and method catalogs. A chain-agnostic JSON-RPC request kind is also the foundation `add-evm-toolkit` builds on, so it needs to exist first and stay free of any Ethereum assumptions.

## What Changes

- Register a new request kind `jsonrpc` in the `workspace` request-kind registry. It has an endpoint (`http`, `https`, `ws`, `wss`), a method, positional or named params, an id strategy and a notification flag.
- **Batches**: several calls sent in one request. Results are matched to calls by id even when they arrive out of order, and each item gets its own assertions.
- **Subscriptions over WebSocket**: one generic pattern (subscribe method, notification method, unsubscribe method), e.g. `eth_subscribe` / `eth_subscription` / `eth_unsubscribe`. Headless runs stop at an event or time bound.
- **Errors**: JSON-RPC error objects are rendered and can be asserted on with a new `rpc_error` assertion. `result` is reachable by the existing JSONPath assertions and captures.
- **Transport policy**: timeouts reuse `http-requests`. Calls can be retried only when they're marked idempotent.
- **Method catalogs**: optionally load an OpenRPC document (from a file or `rpc.discover`) to list methods, validate params before sending, and provide completion data to the TUI.
- **CLI**: `rustman rpc call <endpoint> <method> [params]` and `rustman rpc batch <file>`.
- **TUI**: a `jsonrpc` editor pane registered through the `tui` protocol pane extension point.
- **Agent hook**: an optional read-only `rpc_call` tool, registered through `agent-mcp-interface`'s conditional-tool mechanism.

Out of scope:
- Anything chain-specific (ABI encoding, hex-quantity decoding, ENS, signing). That's `add-evm-toolkit`.
- JSON-RPC 1.0 and XML-RPC.
- Raw TCP or IPC socket transports (e.g. a geth `.ipc` file).
- Acting as a JSON-RPC server or mock.

## Capabilities

### New Capabilities

- `jsonrpc-requests`: defining, sending and checking JSON-RPC 2.0 calls, batches, notifications and subscriptions over HTTP and WebSocket, with OpenRPC method catalogs, the `rpc_error` assertion, the `rustman rpc` CLI and the TUI pane.

### Modified Capabilities

None. The new kind plugs into the existing request-kind registry (`workspace`), assertion engine (`assertions`), runner (`cli-runner`), pane registry (`tui`) and conditional agent tools (`agent-mcp-interface`) without changing their requirements.

## Impact

- New crate `crates/rustman-jsonrpc`. It depends on `rustman-core` and reuses the `rustman-http` transport for HTTP calls.
- WebSocket support adds `tokio-tungstenite` over `rustls`. This is the same dependency `add-graphql-client` uses for subscriptions, so it isn't new to the workspace.
- `rustman-cli` gains the `rpc` subcommand, and `rustman-tui` gains a pane.
- History, reports and the viz companion get `jsonrpc` exchanges through the existing exchange model.
