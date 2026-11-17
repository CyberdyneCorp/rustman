# Proposal

## Why

Ethereum nodes speak JSON-RPC, but raw JSON-RPC is unusable for humans: every value is hex, contract calls are ABI-encoded blobs, and reverts are opaque bytes. Existing API clients stop at "send JSON, show JSON". Developers fall back to a mix of `cast`, block explorers and scripts. Rustman already has the pieces (a JSON-RPC client, GraphQL, assertions, reports, an agent interface), so an ABI-aware, read-only-by-default EVM console with real safety rails is a differentiator no mainstream API client offers.

## What Changes

- An EVM layer on top of `jsonrpc-requests`: chain pinning and detection, a known-chain registry, a typed `eth_*` method catalog, and unit-aware value display (hex, decimal, wei, gwei, ether).
- New request kind `evm-call`: read-only contract calls by address or ENS name, with ABI loading from local JSON, Foundry/Hardhat artifacts, or an opt-in Etherscan-compatible source fetch. Decoded outputs, revert reasons and custom errors.
- Decoding of transactions, receipts and logs, with built-in ERC-20/721/1155 ABIs and token-unit formatting.
- Live streams of new heads and logs (`eth_subscribe`, with polling fallback over HTTP).
- Simulation: `eth_call` and `eth_estimateGas` at any block with state overrides, and call trees via `debug_traceCall`/`trace_call` when the node supports them.
- ENS forward and reverse resolution, optional Anvil fork management, EVM assertions and captures.
- New request kind `evm-tx`, gated by the **signing gate**: off by default. Sending needs workspace opt-in plus a per-invocation `--sign`, an approved signer (keystore, keychain or hardware wallet; never a raw key), a decoded confirmation, a chain-id match and a successful simulation. Agents can never sign.
- `rustman evm probe`: chain exploration of one or more RPC providers (method support, archive detection, rate limits, cross-provider consistency, latency), producing standard reports and viz dashboards.
- Blockchain GraphQL presets for geth's EIP-1767 endpoint and The Graph subgraphs: scalar formatting, pagination helpers, subgraph freshness checks and links into the EVM decoder.
- CLI `rustman evm call|decode|logs|tx|block|ens|fork|send|probe`, a TUI pane for both kinds, and read-only agent tools.

Out of scope: non-EVM chains (Solana, Bitcoin, Starknet stay plain `jsonrpc`), contract compilation or deployment tooling (use Foundry/Hardhat), wallet UX beyond signing a single transaction (no portfolio, no token swaps), account abstraction (ERC-4337) bundler flows, MEV/private relays, and Etherscan features beyond verified-ABI fetch.

## Capabilities

### New Capabilities
- `evm-toolkit`: read-only EVM access. Chain identity, method catalog, `evm-call` requests, ABI handling, decoding, streams, simulation, ENS, Anvil forks, EVM assertions and captures, CLI, TUI and agent tools.
- `evm-signing`: the signing gate and the `evm-tx` request kind. Signers, confirmation, simulation-before-sign, nonce handling, CI restrictions and audit. The only capability allowed to send a signed transaction.
- `chain-exploration`: automated probing and comparison of EVM RPC providers, reported through `test-reports` and `visual-dashboards`.
- `blockchain-graphql`: presets and helpers for blockchain GraphQL endpoints (geth EIP-1767, The Graph subgraphs) on top of `graphql-requests`.

### Modified Capabilities
None. `workspace` gains kinds `evm-call` and `evm-tx` through its kind registry. `agent-mcp-interface` gains read-only EVM tools through its conditional-tool mechanism. `test-reports` gains an additive report subject. None of these change those capabilities' requirements.

## Impact

- New crate `crates/rustman-evm` (depends on `rustman-core`, `rustman-jsonrpc`, `rustman-report`), built on the `alloy` crate family.
- New optional runtime tool: `anvil` on `PATH` (only for `rustman evm fork`).
- Hardware wallet support adds USB HID dependencies behind a cargo feature `hw-wallet`, off for plain `cargo build` (USB HID needs `libudev` on Linux, which would break the "no system libraries" rule) and enabled in official release artifacts.
- `rustman.toml` gains an `[evm]` section; environments gain `chain_id`.
- No change to the default build requirements: everything stays pure Rust plus `ring`.
