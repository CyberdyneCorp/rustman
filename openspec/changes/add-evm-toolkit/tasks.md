# Tasks

## 1. Crate scaffold and chain identity

- [x] 1.1 Create `crates/rustman-evm` depending on `rustman-core`, `rustman-http`, `rustman-jsonrpc` and pinned `alloy` crates (primitives, json-abi, dyn-abi, without default features); the signing stream (section 5) adds `consensus` and `signer-local`, and RPC types stay `serde_json` values decoded by the toolkit, so `rpc-types` is not needed; verify `cargo build` with no system libraries and `cargo deny check` passes.
- [x] 1.2 Register request kinds `evm-call` and `evm-tx` in the workspace kind registry with schema validation; verify loader tests for valid files, overloaded-function errors, missing `to` in `evm-tx`, and that loading performs no RPC or signer access.
- [x] 1.3 Implement chain pinning, `eth_chainId` check, client detection and the known-chain registry with `[evm.chains]` overrides; verify unit tests against recorded RPC fixtures (geth, reth, anvil, unknown) and the exact mismatch message with exit code 4.
- [x] 1.4 Document `[evm]` config, `chain_id` in environments and the chain registry in `docs/evm.md`; verify docs build and examples parse.

## 2. Values, ABI and decoding

- [x] 2.1 Implement `Quantity` parsing and formatting (hex, decimal, wei/gwei/ether, token units) with integer-only arithmetic; verify property tests (parse ∘ format round-trips for random U256) and the `0.000000000000000001 ether → 0x1` case.
- [x] 2.2 Implement ABI sources (built-in ERC-20/721/1155, JSON, Foundry, Hardhat) with path-escape protection and content-hash cache; verify fixture tests per source format and a path-escape rejection test.
- [x] 2.3 Implement opt-in explorer ABI fetch with `.rustman/abi-cache/` and EIP-1967 proxy resolution; verify the disabled-by-default error makes no network call (mock server asserts zero hits) and a proxy fixture resolves to the implementation.
- [x] 2.4 Implement encode/decode for calls, outputs, logs, `Error(string)`, `Panic(uint256)` and custom errors; verify tests with known calldata/revert vectors including `Panic(0x12)` text.
- [x] 2.5 Implement transaction/receipt decoding and token metadata caching with the `decimals()` failure warning; verify against recorded mainnet fixtures (USDC transfer) via insta snapshots.

## 3. Calls, simulation, streams, ENS, forks

- [x] 3.1 Implement `evm-call` execution via `rustman-jsonrpc` (`eth_call`, block tags, state overrides, `--estimate`); verify against a local Anvil in CI (deploy a fixture ERC-20 with `forge` in test setup) and the unsupported-overrides error with a stub node.
- [x] 3.2 Implement call traces (`debug_traceCall` → `trace_call` → degrade) normalized to one tree; verify with Anvil (supports debug) and a stub returning `-32601` for both.
- [x] 3.3 Implement head/log streams over `eth_subscribe` with HTTP polling fallback and reorg marking; verify with Anvil WebSocket, an HTTP-only stub, and a `removed: true` fixture.
- [x] 3.4 Implement ENS forward/reverse resolution with forward verification and the `ens_rpc` requirement; verify with recorded mainnet fixtures and the no-mainnet error.
- [x] 3.5 Implement `rustman evm fork` (spawn, lifetime binding, temporary environment) and the `anvil not found` path; verify an integration test that starts and stops the fork and a test with `PATH` cleared.
- [x] 3.6 Register the send-method blocklist pre-send hook in `rustman-jsonrpc`; verify a `jsonrpc` file calling `eth_sendRawTransaction` is refused with exit 4 and the mock node receives nothing.
  - Note: the Anvil-backed checks of 3.1, 3.2, 3.3 and 3.5 (`crates/rustman-evm/tests/anvil.rs`) are opt-in (`RUSTMAN_ANVIL=1`) and run in the `jsonrpc-anvil` CI job with `RUSTMAN_REQUIRE_ANVIL=1`; they pass locally against Anvil, and their CI run is pending the first push. A SIGKILLed `rustman evm fork` can leave Anvil running (no unsafe `PR_SET_PDEATHSIG`, no Windows job object); its stale environment is removed by the next fork on Unix.

## 4. Assertions, captures, CLI, TUI and agent tools

- [x] 4.1 Implement `evm` assertions (output ops with units, `emits`, `reverts`, `reverts_with`, `gas_lt`) and EVM captures; verify unit tests per op and failure-message snapshots.
- [x] 4.2 Implement CLI `rustman evm call|decode|logs|tx|block|ens|fork` with `--json`, `--env`/`--rpc` and ROADMAP exit codes; verify `assert_cmd` tests per subcommand including offline `decode`.
- [x] 4.3 Implement TUI panes for `evm-call` (function picker, typed args, unit toggles, decoded/revert/trace views, event timeline) and the status bar chain/client display; verify ratatui `TestBackend` snapshot tests.
- [x] 4.4 Register read-only agent tools (`evm_call`, `evm_decode`, `evm_tx`, `evm_block`, `evm_logs_query` with the 10,000-block cap, `evm_ens`); verify MCP contract tests and that `tools/list` contains no sending tool.
- [x] 4.5 Extend `docs/evm.md` with request-file reference, assertions, captures and CLI examples; verify every example file in docs loads in a doc test.
- [x] 4.6 Implement the typed method catalog (`eth_*`, `net_*`, `web3_*` params with types), `rustman rpc call --param name=value`, the pre-send param check (EIP-55 checksum, hex data, block tags) for every face, the native-amount display of `eth_getBalance`, and the TUI's typed-params label; verify the "Typed balance query", "Bad checksum rejected" and "Balance shown in ether" scenarios in unit and `assert_cmd` tests.

## 5. Signing gate

- [ ] 5.1 Implement signer configuration (`keystore`, `keychain` with `rustman evm signer import`, `ledger`/`trezor` behind `hw-wallet`) with `Secret<T>` + `zeroize`, and raw-key rejection everywhere (files, env, vars, flags); verify tests for each signer type with test keystores and rejection tests for a 64-hex value in each location.
- [ ] 5.2 Implement the gate preconditions (workspace `allow_signing`, `--sign`, pinned and matching chain id, simulation success or `--force-revert`) with exit code 4 and exact messages; verify one test per refusal path.
- [ ] 5.3 Implement fee suggestion (`eth_feeHistory`, legacy `eth_gasPrice`), gas × 1.2 and nonce management (pending count + in-run counter); verify against Anvil with two sequential transactions and fee-history fixtures.
- [ ] 5.4 Implement the confirmation (CLI prompt and TUI dialog) with all required fields, typed chain name for chain 1 and `protected_chains`, and `LocalHeadless` for 31337/`headless_signing_chains` with `--yes-local`; verify expectrl-driven CLI tests, TUI snapshots, and non-TTY refusal on a non-local chain.
- [ ] 5.5 Implement send, receipt wait (`--confirmations`, 120 s timeout → exit 3), receipt assertions/captures and the history audit record without raw bytes or key material; verify an Anvil end-to-end test and a history-file scan asserting no key material.
- [ ] 5.6 Prove agents can't sign: a dependency test asserting `rustman-mcp` does not depend on `rustman-evm::signing`, and an MCP test where `send_request` on an `evm-tx` file fails with the specified error.
- [ ] 5.7 Document signing setup, the gate rules and CI-on-Anvil usage in `docs/evm-signing.md`; verify docs examples run against Anvil in CI.

## 6. Chain exploration

- [x] 6.1 Implement `rustman evm probe` on the exploration runner with the EVM probe catalog, safety policy budgets and read-only method allowlist; verify a test that the mock node never sees a write method.
- [x] 6.2 Implement method support matrix, archive detection, subscription check and per-method latency; verify against stub providers (archive, pruned, no-trace, silent WebSocket).
- [x] 6.3 Implement opt-in rate-limit discovery and multi-provider consistency (head drift, block hash, state at fixed block, chain-id mismatch → exit 2); verify with stubs that return 429 above a threshold and that disagree on state.
- [x] 6.4 Emit `rustman.report/v1` `kind: exploration`, `subject: evm-rpc`, apply `--fail-on`, and stream `--viz` events (latency heatmap, block-time/gas plots, call-trace graph); verify the report validates against the published schema and a viz stub receives the events.
  - Note: the `--viz` events are sent when the probe finishes, not streamed while it runs (`docs/evm-probe.md`).
- [x] 6.5 Document `rustman evm probe` in `docs/evm-probe.md`; verify example invocations in docs run against stubs.

## 7. Blockchain GraphQL presets

- [ ] 7.1 Implement preset detection (`geth`, `subgraph`) and explicit `preset:` handling with the mismatch warning; verify with introspection fixtures from geth and a subgraph.
- [ ] 7.2 Implement scalar formatting (BigInt/BigDecimal/Long decimal, Bytes hex, EIP-55 addresses) at display time only; verify snapshot tests and that assertions compare raw values.
- [ ] 7.3 Implement `--paginate` (`id_gt` default, `skip` option, `--max-pages` 20) for CLI and TUI; verify against a mock subgraph returning paged data.
- [ ] 7.4 Implement subgraph freshness (`_meta`, lag display, `max_lag_blocks` assertion, no-RPC behavior) and decoder links in the TUI; verify with a mock subgraph + Anvil head and TUI snapshot tests.
- [ ] 7.5 Document presets in `docs/evm.md`; verify example queries in docs parse against the fixture schemas.

## Workflow follow-up

- Archive with `openspec archive add-evm-toolkit` after implementation, once `add-jsonrpc-client` is archived.
