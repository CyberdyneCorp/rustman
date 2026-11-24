# Design

## Context

See proposal.md for motivation. This change sits on top of:
- `jsonrpc-requests` (`add-jsonrpc-client`): JSON-RPC 2.0 over HTTP and WebSocket, batches and subscriptions. EVM requests go through it rather than through a second RPC stack.
- `graphql-requests`: blockchain GraphQL presets are display and helper layers over the existing GraphQL client.
- The safety policy and `test-reports` from `add-openapi-explorer`, used by `chain-exploration`.
- `agent-mcp-interface` (conditional read-only tools) and `visual-dashboards` (probe dashboards).
- The kind registry in `workspace`, where this change registers `evm-call` and `evm-tx`.

## Goals / Non-Goals

**Goals:**
- One crate, `rustman-evm`, that owns all EVM knowledge (ABI, units, decoding, chains, signing) and exposes plain functions to the CLI, TUI and agent adapters.
- Signing that is impossible to reach by accident: a single code path, gated at compile-visible boundaries, with no agent route.
- Correct numerics: every quantity is a 256-bit integer; no floating point anywhere in parsing or display.

**Non-Goals:**
- Our own RPC transport (reuse `rustman-jsonrpc`).
- Running a light client or verifying state proofs.
- Non-EVM chains.

## Decisions

### D1. `alloy` crate family (rejected: `ethers-rs`, hand-rolled)
`ethers-rs` is deprecated in favor of `alloy`. We use these `alloy` crates: `alloy-primitives` (U256, Address with EIP-55, keccak), `alloy-json-abi` and `alloy-dyn-abi` (runtime ABI encode/decode; our ABIs are loaded at runtime, so `sol!` macros don't apply), `alloy-rpc-types` (typed blocks, receipts, logs, traces), `alloy-consensus` and `alloy-signer-local` (transaction building, keystore decryption), `alloy-signer-ledger` and `alloy-signer-trezor` behind feature `hw-wallet`. We do **not** use `alloy-provider`/`alloy-transport`: requests go through `rustman-jsonrpc` so history, redaction, safety policy and timing apply uniformly. A thin adapter maps typed `alloy` request/response structs to `rustman-jsonrpc` calls.

### D2. Signing behind one gate type
`rustman-evm::signing` exposes a single entry point `SigningGate::sign_and_send(PreparedTx, Confirmation) -> Receipt`. `Confirmation` can only be constructed by the CLI interactive prompt (a TUI confirmation dialog is deferred: the TUI does not sign in this change), or by `LocalHeadless` (which checks chain id 31337 or `headless_signing_chains`). The agent crate does not depend on `rustman-evm::signing` at all (enforced by a `cargo deny`/dependency test), so "agents can never sign" is a build property, not a runtime check. Signer secrets (`LocalSigner`, keystore passwords) are wrapped in `Secret<T>` from `rustman-core` and zeroized on drop (`zeroize`).

### D3. Send-method blocklist enforced in the JSON-RPC layer
`rustman-jsonrpc` exposes a pre-send hook. When `rustman-evm` is linked, it registers the hook that refuses `eth_sendRawTransaction`, `eth_sendTransaction` and the signing methods for every `jsonrpc` request, except via the signing gate (and the local-chain opt-in). This keeps raw `jsonrpc` files from bypassing the gate.

### D4. ABI loading and caching
ABI sources normalize to `alloy_json_abi::JsonAbi`. Caching is by SHA-256 of the source bytes (in memory) and by `<chain>/<address>` for explorer fetches (`.rustman/abi-cache/`, git-ignored). Explorer fetch uses the Etherscan V2 multichain API shape (`chainid` parameter) with per-chain overrides, and the fetch goes through `http-requests` with the safety policy (only the configured explorer host). EIP-1967 proxies are detected by reading the implementation slot `0x3608…bbc` with `eth_getStorageAt`.

### D5. Units and formatting
A `Quantity` display model holds a `U256` and a unit context (native decimals, token decimals and symbol). Parsing accepts `<decimal>[ unit]` and hex; conversion uses integer arithmetic (`alloy_primitives::utils::parse_units`/`format_units`). Token metadata (`decimals`, `symbol`) is cached per `(chain, token)` for the session.

### D6. Streams
`eth_subscribe` over WebSocket via `rustman-jsonrpc` subscriptions. The HTTP fallback uses `eth_newFilter` + `eth_getFilterChanges` on a 2 s interval, recreating the filter on `filter not found`. Removed logs (`removed: true`) are surfaced as reorgs.

### D7. Traces
Try `debug_traceCall` with `{"tracer":"callTracer"}` first, then `trace_call` with `["trace"]`, then degrade. Both normalize into one `CallFrame` tree. Support is cached per endpoint.

### D8. Anvil as an external process
`rustman evm fork` shells out to `anvil` (found on `PATH`) rather than embedding a node: embedding would pull a whole EVM implementation into the default binary. The child is tied to the command's lifetime as far as the command can see it end: it is killed on drop, after Ctrl+C, SIGTERM or SIGHUP, and on the forced exit of a second interrupt. On Unix it runs in a process group of its own, so a terminal's Ctrl+C reaches only Rustman, which removes the environment before stopping the node. A command killed outright (SIGKILL) cannot stop it: `PR_SET_PDEATHSIG` would need `unsafe` code (`pre_exec`), which the workspace forbids, and Windows has no job-object helper in the codebase. The temporary environment records Anvil's pid, and the next fork removes files whose Anvil is gone (Unix).

### D9. Fees and nonces
EIP-1559 suggestion from `eth_feeHistory(10, "latest", [50])`: `max_priority_fee` = median of the 50th percentile rewards; `max_fee` = 2 × latest base fee + priority. Legacy uses `eth_gasPrice`. Gas limit = estimate × 1.2. Nonces come from `pending` count, with an in-run counter per signer so a collection of `evm-tx` files does not race.

### D10. Probe engine reuse
`chain-exploration` reuses the exploration runner's scheduling, budget and findings types from `rustman-explore`, with an EVM-specific probe catalog. Reports use `rustman.report/v1` `kind: exploration` with an additive `subject: evm-rpc` field, so existing renderers and `report diff` work unchanged.

As built, the probe reuses the safety layer under the exploration runner (`SafetyPolicy`, `Governor`/`Limits` pacing, concurrency, time budget and 429 backoff through `GovernedSender`) and the `rustman-report` exploration types (findings, coverage, probe results, severities), but not `rustman-explore`'s runner itself: that runner plans probes from OpenAPI operations, while a chain probe is a fixed sequence of dependent JSON-RPC checks per provider (head first, then the archive block below it, then comparisons at a common block). The per-provider `--budget` is counted by the probe's own meter, which also enforces the read-only method allowlist and records per-method latency (time on the wire, so the governor's pacing is never counted); the governor's `max_probes` stays the global ceiling. Report assembly helpers (totals, ids) mirror the exploration runner's small ones rather than adding a dependency from `rustman-evm` on `rustman-explore`.

### D11. GraphQL presets as a result decorator
Presets don't change how GraphQL is sent. They add a schema-based detector, a scalar formatter applied at display time, pagination drivers that re-issue the operation with updated variables, and actionable-value detection in the TUI.

## Risks / Trade-offs

- [A user bypasses the gate by calling a node's `personal_*` or `eth_sendTransaction` with an unlocked node account] → covered by the D3 blocklist for all `jsonrpc`/`evm-call` requests.
- [Fee suggestion is wrong on chains with unusual fee markets (some L2s)] → suggestions are labeled `suggested`, always shown in the confirmation, and overridable in the file.
- [Hardware-wallet crates need `libudev` on Linux] → feature `hw-wallet` is off for plain `cargo build`, on for release artifacts; keystore and keychain signers always work.
- [Rate-limit discovery could annoy a provider or burn quota] → opt-in flag, low maximum rate, stops at the first limit, budget-capped.
- [Explorer API keys could leak] → they are secrets; redaction applies to URLs and headers in history and reports.
- [`alloy` minor versions move fast] → pin minor versions; keep all `alloy` types behind `rustman-evm`'s own API so upgrades stay local.
- [Reorgs change "latest" results between calls] → probes compare at a fixed block number at least 10 blocks below head.

## Migration Plan

Additive. Workspaces without `[evm]` keep working; EVM kinds simply aren't used. `rustman init` adds `.rustman/abi-cache/` to the generated `.gitignore` (already covered by `.rustman/`).

## Open Questions

- Whether to ship more chains in the built-in registry (e.g. BNB Chain, Avalanche C-Chain, zkSync Era) or keep it minimal and rely on `[evm.chains]`. Doesn't change any requirement.
- Whether `evm-tx` should later support EIP-712 typed-data signing for off-chain messages. Deferred; it would be a new change.
