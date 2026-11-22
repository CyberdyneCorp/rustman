# EVM toolkit

Rustman's EVM layer sits on top of the [JSON-RPC client](jsonrpc.md): every node call goes through it, so its transport settings, method guards and redaction apply. The `rustman-evm` crate knows chains, units and ABIs; it never signs or sends a transaction.

This page covers what is available today: the `[evm]` table, chain pinning with `chain_id` in environments, the known-chain registry, node client detection, quantities and units, the `evm-call` and `evm-tx` request files and their load-time checks, ABI sources (built-in token standards, JSON ABIs, Foundry and Hardhat artifacts, opt-in explorer fetch), decoding of calls, outputs, reverts, transactions, receipts and logs, running calls (with state overrides, gas estimates and traces), `evm` assertions and captures, head and log streams, ENS names, local Anvil forks, the send-method guard, the `rustman evm` commands, the TUI pane and the read-only agent tools. Signing and sending `evm-tx` requests with `rustman evm send` is covered in [EVM signing](evm-signing.md).

## `[evm]` in `rustman.toml`

Every key is optional. Workspaces without `[evm]` keep working.

```toml
[workspace]
name = "defi"
version = 1

[evm]
# Verified ABIs from a block explorer: off unless set.
allow_explorer_fetch = true
# A mainnet endpoint for ENS names (a URL or a {{variable}}).
ens_rpc = "{{mainnet_rpc}}"

# Add a chain, or change a built-in one.
[evm.chains.999]
name = "My L2"
symbol = "MYL"
decimals = 18
explorer = "https://explorer.myl2.example"

# An Etherscan-compatible API per chain id. The key is a secret.
[evm.explorers.1]
api_url = "https://api.etherscan.io/v2/api"
api_key = "{{etherscan_key}}"
```

| Key | Default | Meaning |
|---|---|---|
| `allow_explorer_fetch` | `false` | Allow `abi: {explorer: true}` to fetch verified ABIs. |
| `ens_rpc` | unset | The mainnet endpoint used for ENS names. |
| `[evm.chains.<id>]` | none | `name`, `symbol`, `decimals` and `explorer` for a chain id; absent fields keep the built-in values. |
| `[evm.explorers.<id>]` | none | `api_url` and `api_key` of an Etherscan-compatible API (the V2 multichain shape, with a `chainid` parameter). |
| `allow_raw_send_local` | `false` | Reserved: would let `jsonrpc` files send raw transactions to chain 31337. Not honored in this build; see [Send methods](#send-methods). |
| `allow_signing`, `signers` | unset | Read by the signing gate: see [EVM signing](evm-signing.md). |
| `protected_chains`, `headless_signing_chains` | unset | Reserved for the signing confirmation (not available yet). |

A key Rustman does not know under `[evm]` is a warning when the workspace loads (`rustman.toml: unknown key 'evm.<key>' ignored`); `rustman-evm` rejects misspelled keys inside `[evm.chains.*]` and `[evm.explorers.*]`, and table keys that are not integer chain ids.

## Chain pinning

An environment may pin the chain its endpoints must be on:

```yaml
chain_id: 11155111
variables:
  rpc: https://sepolia.example/rpc
secrets: [etherscan_key]
```

Before the first EVM request to an endpoint in a session, Rustman calls `eth_chainId`. When the environment sets `chain_id` and the endpoint reports another chain, the request stops there, with exit code 4 and nothing else sent:

```text
chain mismatch: environment 'sepolia' expects 11155111 (Sepolia), endpoint reports 1 (Ethereum Mainnet)
```

Without `chain_id`, the detected chain is shown and used. The chain and the node client are asked once per endpoint per session.

## Node client detection

After the chain check, Rustman reads `web3_clientVersion` and classifies the node as `geth`, `reth`, `erigon`, `nethermind`, `besu`, `anvil`, `hardhat` or `unknown`, with its version: `Geth/v1.14.11-stable/linux-amd64/go1.23.2` is shown as `geth 1.14.11`. A node without `web3_clientVersion` (JSON-RPC error `-32601`) is `unknown`, with no error; Rustman then asks `net_version` for the network id. EVM command JSON output carries `chain_id`, `chain`, `client`, `client_version` and `network`.

## Known chains

| Id | Name | Native currency | Explorer |
|---|---|---|---|
| 1 | Ethereum Mainnet | ETH | https://etherscan.io |
| 10 | OP Mainnet | ETH | https://optimistic.etherscan.io |
| 137 | Polygon PoS | POL | https://polygonscan.com |
| 8453 | Base | ETH | https://basescan.org |
| 17000 | Holesky | ETH | https://holesky.etherscan.io |
| 31337 | Anvil/Hardhat local | ETH | none |
| 42161 | Arbitrum One | ETH | https://arbiscan.io |
| 11155111 | Sepolia | ETH | https://sepolia.etherscan.io |

Every native currency has 18 decimals. Chains are shown as `Name (id)`, such as `Base (8453)`. An id neither built in nor configured is shown as `chain <id>` with symbol `ETH`. With `[evm.chains.999] name = "My L2"` and `symbol = "MYL"`, chain 999 is shown as `My L2 (999)` and native values in `MYL`. Chains can also be named by alias: `mainnet`, `optimism`, `arbitrum`, `polygon`, `anvil`.

## Quantities and units

Every quantity is a 256-bit integer; parsing and display use integer arithmetic only, so conversions are exact.

| Input | Meaning |
|---|---|
| `0x0de0b6b3a7640000` | Hex. |
| `1000000000000000000` | A decimal integer, in wei. |
| `1.5 ether`, `2 eth`, `2 ETH` | Ether (18 decimals). |
| `30 gwei` | Gwei (9 decimals). |
| `5 wei` | Wei. |
| `2 MYL` | The chain's native symbol, with its decimals. |

`0.000000000000000001 ether` is `0x1`. An amount with more decimals than its unit (`1.5 wei`) and an unknown unit (`1 finney`) are errors naming the value. Nodes receive the minimal hex form (`0x1`).

Native values (`value`, `balance`, gas prices and fees) are shown in the native currency first, with toggles for wei, gwei and hex: a balance of `0x0de0b6b3a7640000` is `1 ETH`, `1000000000000000000 wei`, `1000000000 gwei` or `0x0de0b6b3a7640000` (the hex the node sent). Other quantities (block numbers, nonces) toggle between decimal and hex. ERC-20 amounts are shown with the token's decimals and symbol, with at least two decimals: `250.00 USDC`.

## Request files

### `kind: evm-call`

A read-only `eth_call`. It never produces a signed transaction.

```yaml
version: 1
kind: evm-call
name: USDC balance
rpc: "{{rpc}}"
to: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
abi: erc20
function: balanceOf
args: ["0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"]
block: latest
```

| Field | Required | Meaning |
|---|---|---|
| `rpc` | yes | The node URL or a `{{variable}}`. |
| `to` | yes | A contract address or an ENS name. |
| `abi` | yes | A built-in name, a path, or `{explorer: true}` (see [ABI sources](#abi-sources)). |
| `function` | yes | A function name, or a full signature such as `transfer(address,uint256)` for overloaded functions. |
| `args` | no | The arguments, in order: strings, numbers, booleans, and lists for arrays and tuples. Integers accept unit forms (`"1.5 ether"`). Mixed-case addresses must have a valid EIP-55 checksum. |
| `from`, `value` | no | The caller and the value sent with the call. |
| `block` | no | `latest` (default), `earliest`, `pending`, `safe`, `finalized`, a number, or a block hash. |
| `state_overrides` | no | Per address: `balance`, `nonce`, `code` and `storage` (slot to value). |
| `variables`, `assertions`, `captures`, `tags`, `description`, `docs` | no | As for other kinds. |

A simulation at a historical block with an override:

```yaml
version: 1
kind: evm-call
name: Vault balance at a block
rpc: "{{rpc}}"
to: "0x5FbDB2315678afecb367f032d93F642f64180aa3"
abi: ../out/Vault.sol/Vault.json
function: balance
block: 19000000
state_overrides:
  "0x5FbDB2315678afecb367f032d93F642f64180aa3":
    balance: 100 ether
    storage:
      "0x0": "0x1"
```

### `kind: evm-tx`

A transaction to sign and send, only through the signing gate ([EVM signing](evm-signing.md)): `rustman evm send --sign` or `rustman run --sign`. A plain `rustman run` reports it as `skipped (signing not requested)` without counting it as a failure, and `rustman send` refuses it with ``evm-tx: sign and send it with `rustman evm send --sign` or `rustman run --sign`; nothing was signed or sent`` (exit code 4, like every signing refusal). Loading never calls a node or touches a signer.

`rustman evm send` checks an `evm-tx` request against the signing gate (workspace opt-in, approved signers, `--sign`, the `chain_id` pin, simulation); see [EVM signing](evm-signing.md).

```yaml
version: 1
kind: evm-tx
name: Pay Bob
rpc: "{{rpc}}"
to: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
abi: erc20
function: transfer
args: ["0x5e3D81ECeb0c7D9bA1E6cfDAcE631B165a149d3F", "10"]
gas: 100000
max_fee: 30 gwei
max_priority_fee: 1 gwei
type: eip1559
signer: deployer
```

`to` is required: a file without it fails to load with `contract deployment is out of scope; use Foundry or Hardhat`. The other fields are `value`, `gas`, `max_fee`, `max_priority_fee`, `gas_price` (legacy), `nonce`, `type` (`eip1559` or `legacy`) and `signer`.

### Load-time checks

When a workspace loads, each EVM file is checked against its schema: unknown fields (such as `gas` in an `evm-call`), a missing `to`, `abi` or `function`, a bad `abi` value, negative numbers. Its local ABI (a built-in name or a file) is read and the function is looked up too, still without calling a node; a problem fails the load (exit code 2), so nothing is sent:

```text
requests/transfer.rman.yaml:7: function 'transfer' is overloaded; use a full signature such as 'transfer(address,uint256)'
```

The argument count, literal quantities (`value`, the fee fields), `block` and `state_overrides` are checked as well. Values that come from `{{variables}}` are checked when the request runs.

## ABI sources

| `abi:` | Source |
|---|---|
| `erc20`, `erc721`, `erc1155` | The built-in token standards. |
| `../abi/Token.json` | A JSON ABI (an array). |
| `../out/Vault.sol/Vault.json` | A Foundry artifact: its `abi` array is used. |
| `../artifacts/contracts/Vault.sol/Vault.json` | A Hardhat artifact: its `abi` array is used. |
| `{explorer: true}` | The verified ABI from the chain's explorer (opt-in). |

Paths are relative to the request file and must stay inside the workspace: `abi path '../../outside.json' resolves outside the workspace`. Parsed ABIs are cached for the session by the SHA-256 of their bytes, so the same file used by many requests is parsed once.

### Explorer fetch

`abi: {explorer: true}` fetches the verified ABI from the API in `[evm.explorers.<chain id>]`, only when `[evm] allow_explorer_fetch = true`. Otherwise the request fails, before any network access, with:

```text
explorer ABI fetch is disabled; set [evm] allow_explorer_fetch = true
```

Rustman first reads the EIP-1967 implementation slot (`eth_getStorageAt` at `0x3608…2bbc`). For a proxy, the implementation's ABI is used and Rustman says so: `proxy → implementation 0x…`. Fetched ABIs are kept in `.rustman/abi-cache/<chain id>/<address>.json` (git-ignored with the rest of `.rustman/`) and read from there next time. The API key is a secret: it is redacted from every message, even when the explorer echoes it back.

## Decoding

- **Calls and outputs**: arguments are encoded with the ABI; return values are decoded with their names. Addresses are shown checksummed, integers in decimal, bytes as hex, strings quoted; JSON output keeps integers as decimal strings.
- **Reverts**: `Error(string)` is shown as `reverted: <reason>`; `Panic(uint256)` with its Solidity meaning, such as `reverted: Panic(0x12) division or modulo by zero` or `reverted: Panic(0x11) arithmetic overflow`; custom errors of the loaded ABI as `reverted: InsufficientBalance(available=5, required=10)`. Other revert data is shown as hex with its 4-byte selector.
- **Transactions and receipts**: from, to, value, nonce, block, status, gas used, effective gas price and the fee paid, with the input decoded by the ABI known for the target (else the built-in token standards).
- **Logs**: decoded with the ABI known for their address, else the built-in token standards (ERC-20 and ERC-721 `Transfer` are told apart by their indexed fields). A USDC transfer reads `Transfer(from=0x…, to=0x…, value=250.00 USDC)`. Undecodable logs show their topics and data as hex.
- **Token metadata**: `decimals()` and `symbol()` are asked once per token per session. When `decimals()` fails, the raw amount is shown with the warning `decimals() unavailable; showing raw amount`. An unknown transaction is `transaction not found` (exit code 1).

## Running calls

An `evm-call` runs as one `eth_call` through the [JSON-RPC client](jsonrpc.md), at `block` (a tag, a number sent as hex, or a block hash sent as `{"blockHash": ...}`), with `state_overrides` as the call's third parameter:

```text
{"method": "eth_call", "params": [{"to": "0x…", "data": "0x70a08231…"}, "0x121eac0",
  {"0x5FbDB2315678afecb367f032d93F642f64180aa3": {"balance": "0x56bc75e2d63100000", "stateDiff": {"0x00…00": "0x00…01"}}}]}
```

Override quantities accept unit forms (`100 ether`); storage slots and values are padded to 32-byte words. A node that rejects the third parameter fails the request with `node does not support state overrides` (exit code 3).

ENS names are accepted for `to`, `from` and every `address` argument; they are resolved first (see [ENS names](#ens-names)). The decoded outputs are shown one per line, by name (or index when unnamed). The amounts of ERC-20 `balanceOf`, `totalSupply` and `allowance` are shown in token units, with the raw integer:

```text
balance = 250.00 USDC (250000000)
```

A revert is shown decoded (see [Decoding](#decoding)) and is not an RPC error. With `--estimate`, Rustman also sends `eth_estimateGas` for the same call (same block and overrides) and adds `gas estimate: 21000`; an estimate that fails is reported on that line, without failing the call.

## Assertions

`evm-call` files take `evm` assertions besides the generic ones (`latency`, and `json` over the result document below). Each `- evm:` item has exactly one of `output`, `emits`, `reverts`, `reverts_with` or `gas_lt`:

| Assertion | Passes when |
|---|---|
| `{output: <name or index>, equals: v}` | The decoded output equals `v`. Integers compare as 256-bit values and accept every quantity form (`0x1`, `"1 ether"`, `"30 gwei"`, and the token's symbol for ERC-20 amounts: `"250 USDC"`); addresses compare case-insensitively; booleans, strings and bytes (hex) as written. |
| `{output: ..., gt \| gte \| lt \| lte: v}` | The integer output compares so with `v` (signed integers too). |
| `{output: ..., contains: v}` | A string or bytes output contains `v`, or an array output has an item equal to `v`. |
| `{emits: "<Event(sig)>", where: {field: value}}` | An event with that signature was decoded, with those fields (`where` is optional). `eth_call` returns no logs, so a file with `emits` (or an `evm_event` capture) simulates the call once more with `debug_traceCall` (`callTracer` with `withLog`) and decodes the logs it would emit; logs of reverted sub-calls are dropped. A node without `debug_traceCall` gives a warning and no events. |
| `{reverts: true}` / `{reverts: false}` | The call reverted, or did not. |
| `{reverts_with: "<reason or ErrorName>"}` | The revert's `Error(string)` reason, custom error name or `Panic` matches; `"Panic(0x12)"` matches a panic code too. |
| `{gas_lt: n}` | The gas estimate is below `n`. A `gas_lt` assertion asks `eth_estimateGas` even without `--estimate`. |

```yaml
version: 1
kind: evm-call
name: Treasury balance
rpc: "{{rpc}}"
to: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
abi: erc20
function: balanceOf
args: ["{{treasury}}"]
assertions:
  - evm: {output: balance, gte: "250 USDC"}
  - evm: {reverts: false}
  - evm: {gas_lt: 60000}
captures:
  - {var: treasury_balance, from: evm_output, output: balance}
```

Failures name the output and, when the expected value has a unit, show both sides in the output's own units: the token's for an ERC-20 amount (`expected >= 1000000000000 USDC, got 250 USDC` for `gte: "1 ether"`; the raw integers, with the unit as written, when the token's decimals are unknown), else the native currency's. A value without a unit is compared and shown as a plain integer:

```text
evm output 'balance': expected >= 1 ETH, got 0.5 ETH
evm reverts_with: expected InsufficientBalance, got reverted: Panic(0x12) division or modulo by zero
evm gas_lt: expected < 21000, got 21000
```

An output assertion on a call that reverted is an error that quotes the revert (`evm output 'balance': the call reverted: Panic(0x12) division or modulo by zero`). A revert without an assertion about it does not fail the request: the result shows it. Expected values are taken as written; `{{variables}}` are not interpolated in them yet.

## Captures

| Capture | Value |
|---|---|
| `{var, from: evm_output, output: <name or index>}` | A decoded output. |
| `{var, from: evm_event, event: "<sig>", field: <name>}` | A field of the first decoded event with that signature (reorged logs are skipped). |
| `{var, from: evm_receipt, field: contractAddress \| blockNumber \| status \| gasUsed}` | A receipt field. |

Quantities are stored as decimal strings and addresses as EIP-55 checksummed strings, so `{{owner}}` can be passed to the next call as is:

```yaml
version: 1
kind: evm-call
name: Owner
rpc: "{{rpc}}"
to: "0x5FbDB2315678afecb367f032d93F642f64180aa3"
abi: ../out/Vault.sol/Vault.json
function: owner
captures:
  - {var: owner, from: evm_output, output: 0}
```

Captures follow the usual rules: available to every later request of the run, `persist: true` to keep them in `.rustman/state/`, and a capture named in the environment's `secrets` is a secret. `evm_event` reads the events of the simulated call (see `emits` above). `evm_receipt` has nothing to read in a read-only call (there is no receipt); it reports `no receipt field ...` there, and reads the receipt of an `evm-tx` sent with `rustman evm send` (see [EVM signing](evm-signing.md#receipt-assertions-and-captures)).

## In `rustman run`

`rustman run` sends `evm-call` requests like any other: variables (including `{{rpc}}` and secrets) are resolved, the environment's `chain_id` is checked before the first call to each endpoint, `evm` and generic assertions are evaluated, and captures feed later requests. The JSON report has an `evm` object per request:

```text
{"endpoint": {"chain_id": 1, "chain": "Ethereum Mainnet", "client": "geth", "client_version": "1.14.11", "network": null},
 "status": "Ethereum Mainnet (1) · geth 1.14.11", "to": "0xA0b8…eB48", "block": "latest",
 "result": {"function": "balanceOf(address)", "outputs": {"balance": "250000000"}, "reverted": null,
            "gas_estimate": null, "token": {"decimals": 6, "symbol": "USDC"}}}
```

A request that could not run has `{"error": {"kind": "config" | "rpc" | "refused" | "not_found", "message": ...}}`; a run with a `refused` request (a chain mismatch) exits with code 4, like `rustman evm call`. An `evm-tx` request is skipped as `signing not requested` and does not fail the run. Like every report, the `evm` object is redacted: an endpoint URL kept in a secret never appears.

## Call traces

With `--trace`, Rustman asks `debug_traceCall` with the `callTracer`; when the node does not have it, `trace_call` with `["trace"]` (Erigon and Nethermind style). Both become one call tree, one frame per line, indented by depth, with each frame's type, from and to, decoded function, value, gas and revert reason:

```text
CALL 0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266 -> 0x5FbDB2315678afecb367f032d93F642f64180aa3 balanceViaSelf(who=0xf39F…2266) gas=… used=…
  STATICCALL 0x5FbDB2315678afecb367f032d93F642f64180aa3 -> 0x5FbDB2315678afecb367f032d93F642f64180aa3 balanceOf(0xf39F…2266) gas=… used=…
```

The tree keeps 256 levels of nesting; frames below that are counted, not kept, and shown as one `… N deeper frames omitted` line under the deepest frame kept, so a broken or hostile node cannot exhaust memory or the stack.

When the node has neither method, the call result is still printed, followed by `tracing not supported by geth` (with the detected client), and the exit code is unaffected. Which method works is remembered per endpoint for the session.

## Head and log streams

A stream follows new block headers or logs. Logs are filtered by one or more addresses and by topics; an event signature such as `Transfer(address,address,uint256)` stands for its topic hash, and `*` matches any value at that position.

- Over a `ws://` or `wss://` endpoint, Rustman subscribes with `eth_subscribe` (`newHeads`, or `logs` with the filter) and unsubscribes when the stream ends.
- Over HTTP it creates a filter (`eth_newFilter`, or `eth_newBlockFilter` for heads), polls `eth_getFilterChanges` every 2 seconds (configurable), and says so: `polling every 2s (no WebSocket)`. A node that forgets the filter (`filter not found`) gets a new one; the filter is removed with `eth_uninstallFilter` at the end.

Each log is decoded like a receipt log, one per line. A log the node sends again with `removed: true` (a reorg) is shown as `REORGED` and is not counted as a new event:

```text
REORGED [7] 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48 Transfer(from=0x…, to=0x…, value=250.00 USDC)
```

A stream stops at its event or time bound, when it is stopped, or when the node closes the connection. At most 256 items wait for the printer: when it falls behind, the stream stops reading the node until it catches up, so memory stays bounded.

## ENS names

Names are resolved through Ethereum Mainnet's ENS registry (`0x00000000000C2E074eC69A0dFb2997BA6C7d2e1e`): the name's resolver, then its address, shown checksummed (`vitalik.eth` is `0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045`). Reverse lookups read the `<address>.addr.reverse` record and are shown only when the name resolves back to the same address.

The registry is read from `[evm] ens_rpc` when set, else from the current endpoint when it is chain 1. The `ens_rpc` endpoint's `eth_chainId` is checked before its first read: the registry has the same address on every chain, so an `ens_rpc` on another chain is refused (exit code 4) with `[evm] ens_rpc reports chain <id>, not Ethereum Mainnet (1); ENS is read on mainnet only`. On any other chain, without `ens_rpc`, a name fails with `ENS needs a mainnet RPC; set [evm] ens_rpc`. A name without a resolver or an address is `not found` (exit code 1). Names are lowercased; full ENSIP-15 normalization of non-ASCII names is not done.

## Local forks with Anvil

Rustman runs Foundry's `anvil` (found on `PATH`) to fork a chain: `anvil --host 127.0.0.1 --port 8545 --fork-url <rpc> --fork-block-number <N>`. With `--chain`, the upstream endpoint is the `rpc` variable of the environment of that name (`--chain local`), else of the first environment pinned to the chain it names (`mainnet`, `8453`); `--env` or `--rpc` name the endpoint directly. Rustman waits for Anvil's own `Listening on` line, so another process on the port is never taken for the fork. The rest of the fork's output is discarded: its banner lists development keys, and its errors could quote the upstream URL.

Anvil takes the upstream URL on its command line only (it has no environment variable or file for it), so while the fork runs, other users of the same machine can see that URL, API key included, in the process list (`ps`). `rustman evm fork` prints a note saying so when it starts. On a shared machine, fork from an endpoint without a key in its URL, such as a local node or a local proxy that adds the key.

While the fork runs, a temporary environment `fork-<chain>` (such as `fork-mainnet`) points at it, with `chain_id` set to the id the fork reports:

```text
# Temporary: written by `rustman evm fork`, removed when it exits.
# anvil pid: 48213
chain_id: 1
variables:
  rpc: http://127.0.0.1:8545
```

`--port 0` lets Anvil pick a free port, read back from its `Listening on` line. The file is removed and Anvil stopped when the command exits, on Ctrl+C or when Anvil exits on its own (then with exit code 3 and `anvil exited (...); the fork and environment '<name>' are gone`); an existing environment of that name that Rustman did not write is never replaced. Cleanup runs on Ctrl+C, SIGTERM and SIGHUP (a second one while stopping still kills Anvil); a `rustman` killed outright (SIGKILL) can leave Anvil running, with the upstream URL in its arguments, and the file behind. Stop such a node by its pid, recorded in the file. On Unix, the next `rustman evm fork` removes temporary environments whose recorded Anvil no longer runs and says so. Without `anvil` on `PATH`, the command fails with exit code 2 and `anvil not found; install Foundry (https://getfoundry.sh)`.

The toolkit's tests against a real Anvil are opt-in, so a plain `cargo test` never starts one: run them with `RUSTMAN_ANVIL=1 cargo test -p rustman-evm --test anvil` (Foundry on `PATH`). The `jsonrpc-anvil` CI job sets `RUSTMAN_REQUIRE_ANVIL=1`, which also turns a missing `anvil` into a failure.

## Send methods

Signing and sending belong to the signing gate (`evm-tx`), which is not available in this build. Methods are refused by family, not by exact name, so new sending methods stay out. Every `jsonrpc` request, `evm-call` and agent call, and every `kind: http` request whose body is a JSON-RPC call or batch (an object with a `method`, or an array of them; `rustman send`, `rustman run` and the agent's `send_request`), is refused before anything is sent, with exit code 4, when its method (in any case):

- is in the `personal_*`, `wallet_*` (EIP-5792 and wallet RPCs), `account_*` (Clef) or `signer_*` namespace;
- or has a name that, after its namespace, starts with `send`, `sign`, `submitTransaction` or `postTransaction`: `eth_sendRawTransaction`, `eth_sendTransaction`, `eth_sendTransactionSync` and `eth_sendRawTransactionSync` (EIP-7966), `eth_sendRawTransactionConditional`, `eth_sendBundle`, `eth_sendPrivateTransaction`, `flashbots_sendBundle`, `eth_sign`, `eth_signTransaction`, `eth_signTypedData*`.

The refusal reads:

```text
signing and sending go through 'evm-tx' (signing gate)
```

The guard applies to `rustman rpc`, `rustman run`, the TUI and the MCP server. `[evm] allow_raw_send_local` is not honored in this build: a transaction can only be signed and sent through the signing gate (an `evm-tx` request with `rustman evm send --sign` or `rustman run --sign`, see [EVM signing](evm-signing.md)), so raw sends with these methods are refused on every chain, including 31337.

## Typed JSON-RPC methods

Rustman knows the standard `eth_*`, `net_*` and `web3_*` read methods and the type of each param: address, quantity, data, block tag, hash, filter object, transaction object, bool. `rustman rpc call` builds the params from `--param name=value`, in the method's order:

```text
$ rustman rpc call {{rpc}} eth_getBalance --param address=0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045 --param block=latest
1 ETH
= 1000000000000000000 wei = 1000000000 gwei = 0x0de0b6b3a7640000
```

sends `{"method":"eth_getBalance","params":["0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045","latest"]}`. Quantities take the unit forms above (`--param position=10`, `30 gwei`), a block takes a decimal number, objects and lists are JSON, and an omitted block is `latest`. A native amount (`eth_getBalance`, `eth_gasPrice`, `eth_maxPriorityFeePerGas`, `eth_blobBaseFee`) is shown in the native currency of the environment's pinned chain or, when it pins none, of the chain the node reports (`rustman rpc call` with an endpoint then sends `eth_chainId`, kept out of history), its other forms on stderr; `--raw` and `--json` keep the hex.

The params of a catalog method are checked before they are sent, however they were written (`--param`, `PARAMS_JSON`, a `jsonrpc` file, the TUI, an agent): a mixed-case address with a wrong EIP-55 checksum, non-hex data and an unknown block tag are refused, naming the param:

```text
param 'address': invalid EIP-55 checksum
```

`rustman rpc call` with an endpoint exits 2, whether the params came from `--param` or `PARAMS_JSON`; elsewhere (a `jsonrpc` file, the TUI, an agent) the check is a method guard, with exit code 4 like the send methods. All-lowercase addresses carry no checksum and pass. Params given by name (an object) and methods outside the catalog are not checked. The TUI's JSON-RPC pane shows a catalog method's typed params next to its params, and the same check under them.

The `@evm-read` preset can be used in `[jsonrpc] agent_read_only_methods` to let agents call the read-only methods: `eth_chainId`, `eth_blockNumber`, `eth_call`, `eth_estimateGas`, `eth_getBalance`, `eth_getCode`, `eth_getStorageAt`, `eth_getBlockBy*`, `eth_getTransaction*`, `eth_getLogs`, `eth_feeHistory`, `eth_gasPrice`, `net_version` and `web3_clientVersion`.

## Commands

Every command that calls a node takes `--rpc <url or {{variable}}>` (default: the environment's `{{rpc}}`), `--env`, `--var k=v`, `--timeout`, `--insecure` and `--json`. The endpoint is identified first (`eth_chainId`, then `web3_clientVersion`): a pinned environment on the wrong chain is refused with exit code 4, and the JSON output carries an `endpoint` object with the chain and the node client. Output and errors are redacted.

### `rustman evm call`

```text
rustman evm call requests/token/balance.rman.yaml --env mainnet
rustman evm call --to 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48 --abi erc20 --function balanceOf vitalik.eth
rustman evm call --to 0x5FbDB2315678afecb367f032d93F642f64180aa3 --abi out/Vault.sol/Vault.json --function balance --block 19000000 --estimate --trace
```

A request file (path, id or name) runs with its assertions and captures; an ad-hoc call takes `--to`, `--abi` (a built-in name or a path from the workspace root), `--function` and the arguments as positional values (JSON lists and booleans as JSON, anything else as text), with `--block`, `--from` and `--value`. The node status line goes to stderr; the outputs, the gas estimate and the trace (or `tracing not supported by geth`) to stdout:

```text
balance = 250.00 USDC (250000000)
gas estimate: 21000
tracing not supported by geth
```

Exit codes: 0 when every assertion passed, 1 when one failed, 2, 3 or 4 when the call could not run (see [Exit codes](#exit-codes)). `--json` prints the `evm` record with `passed`, `assertions` and `captures`.

### `rustman evm decode`

Offline: no node, no workspace needed.

```text
$ rustman evm decode --abi erc20 0xa9059cbb000000000000000000000000d8da6bf26964af9d7eed9e03e53415d37aa96045000000000000000000000000000000000000000000000000000000000000000a
transfer(to=0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045, amount=10)
$ rustman evm decode --abi erc20 0x4e487b710000000000000000000000000000000000000000000000000000000000000012
reverted: Panic(0x12) division or modulo by zero
$ rustman evm decode --abi erc20 --topic "Transfer(address,address,uint256)" --topic 0x…from --topic 0x…to 0x…amount
Transfer(from=0x…, to=0x…, value=7)
```

Call data is decoded when its selector names a function of the ABI, else as revert data; with `--topic` (topic 0 first) the data is a log. Data no function or error of the ABI decodes is an error with exit code 2.

### `rustman evm logs`

```text
rustman evm logs --address 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48 --event "Transfer(address,address,uint256)" --max-events 5
rustman evm logs --heads --duration 1m
rustman evm logs --address 0xA0b8… --event "Transfer(address,address,uint256)" --from-block 20000000 --to-block 20000100
```

Without `--from-block` the command streams (see [Head and log streams](#head-and-log-streams)) until `--max-events`, `--duration` or Ctrl+C, then prints `<n> event(s), <m> reorged` on stderr; `--poll-interval` changes the 2 s HTTP polling. With `--from-block` (and `--to-block`, default `latest`) it runs one `eth_getLogs` over that range (a range that ends before it starts is exit code 2). The range is not capped here, as you chose it; the node may still refuse a range it finds too wide. Agents are capped at 10,000 blocks (`evm_logs_query`). `--json` prints one JSON object per log.

### `rustman evm tx`

```text
rustman evm tx 0xdeed52ec1e92d0ca8834284f39ed5e37ff6106b6bb09a9dc37fb6d1c01b0a8f6 --env mainnet
```

The transaction and its receipt (see [Decoding](#decoding)); `transaction not found` with exit code 1 for an unknown or pending hash.

### `rustman evm block`

```text
rustman evm block            # latest
rustman evm block finalized --json
rustman evm block 20000000
```

Number, hash, parent, timestamp, miner, gas used and limit, base fee and transaction count. An unknown block is `block not found` (exit code 1); an unknown tag is a usage error (exit code 2).

### `rustman evm ens`

```text
$ rustman evm ens vitalik.eth --env mainnet
0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045
$ rustman evm ens 0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045 --env mainnet
vitalik.eth
```

A reverse name is printed only when it resolves back to the address; otherwise `<address> has no verified primary ENS name`.

### `rustman evm fork`

```text
rustman evm fork --chain mainnet --block 20000000
rustman evm fork --chain 8453 --port 8546
rustman evm fork --env staging --json
```

`--chain` takes an environment name, a chain name or a chain id; `--env` and `--rpc` choose the endpoint as for the other commands. Prints the local URL (with `--json`, one object with `url`, `ws_url`, `environment`, `chain_id` and `fork_block`), writes the temporary `fork-<chain>` environment (see [Local forks with Anvil](#local-forks-with-anvil)) and waits for Ctrl+C, then stops Anvil and removes the environment. The upstream endpoint is never printed.

## TUI

`evm-call` and `evm-tx` files open in an EVM pane with the tabs **Function**, **Args**, **Block**, **Result**, **Trace** and **Events**:

- **Function**: a picker of the ABI's functions (a built-in ABI, or a file ABI read from the workspace). Only `view` and `pure` functions are listed; `a` (or the mode key) shows every function, as read-only `eth_call` simulations. `Enter` chooses one: its name, or its full signature when the name is overloaded.
- **Args**: one field per input, labelled with its name and Solidity type. Values may use `{{variables}}`.
- **Block**: `←`/`→` cycle `latest`, `finalized`, `safe`, `pending` and `earliest`; a number or hash can be typed in the file.
- **Result**: the status line (`Ethereum Mainnet (1) · geth 1.14.11`), the decoded outputs, the revert, and the gas estimate. `u` cycles the unit form of quantities: native units, wei, gwei and hex for balance-like outputs, token units for ERC-20 amounts, decimal and hex otherwise.
- **Trace**: the call tree, or why there is none. **Events**: the decoded event timeline (the events each send's simulated call emits, when the file reads events; the latest 1,000 lines), reorged logs marked `REORGED`.

Edits change only the top-level `function`, `args` and `block` lines of the file; comments and the other fields are kept. An `evm-tx` opens read-only with ``evm-tx: sign and send it with `rustman evm send --sign` or `rustman run --sign`; nothing was signed or sent``. After a send, the status bar shows the chain and the node client of the endpoint, such as `Base (8453) · geth 1.14.11`.

## Blockchain GraphQL presets

GraphQL endpoints that serve chain data get presets on top of the [GraphQL client](graphql.md). A preset never changes what is sent: it formats what is shown, pages through subgraph lists, measures a subgraph's freshness and links results into the toolkit.

| Preset | For | Detected when the schema has |
|---|---|---|
| `geth` | geth's EIP-1767 endpoint, usually `<rpc>/graphql` | `Query.block` and `Query.logs`, and the `Long` and `Bytes32` scalars |
| `subgraph` | The Graph and compatible indexers | `Query._meta` |

The preset is detected from the active schema (`schema_file` or the schema cache) and shown in the TUI's schema line (`preset subgraph`) and in `rustman graphql query`'s status line. A request file can name it with `preset:`; an explicit preset wins, and on a schema without its markers a warning is shown: `schema does not look like geth EIP-1767` (or `schema does not look like a subgraph`).

```yaml
version: 1
kind: graphql
name: Latest block
endpoint: '{{rpc}}/graphql'
preset: geth
schema_file: geth.graphql
document: '{ block { number hash gasUsed baseFeePerGas miner { address balance } } }'
```

### Scalar display

With a preset active, `BigInt`, `BigDecimal` and `Long` values are shown as exact decimals (geth's hex `BigInt` and `Long` included), `Bytes` and `Bytes32` as `0x` hex, and addresses in EIP-55 checksummed form: fields typed `Address`, and any other 20-byte `0x` value, whatever its field name or type (`Bytes`, `ID` or `String`; a subgraph's usual `account { id }`), as the TUI's result links take them. A `BigInt` in an object that also has a `decimals` field can be shown in token units: `"1000000000000000000000"` with `decimals: 18` is `1000 (18 decimals)`.

This is display only. Assertions, captures, history and `--json` output read the raw JSON as the server sent it, so a `data` assertion compares the lowercase address the subgraph returned:

```yaml
assertions:
  - data: {path: $.token.owner, equals: '0xd8da6bf26964af9d7eed9e03e53415d37aa96045'}
```

`rustman graphql query` prints formatted values; `--raw` prints them as received and `--units` adds the token-unit form. In the TUI, `u` in the response cycles **formatted**, **units** and **raw**, named at the end of the summary line. A formatted value is redacted again before it is shown, so a checksummed address that is a secret never appears.

### Pagination

For subgraph lists, `rustman graphql query <request> --paginate` (and `Alt+G` in the TUI) sends the operation again and again, concatenating the pages into one array per paginated field. A paginated field is a top-level field with a `first` argument:

- `id_gt` (the default) follows `where: {id_gt: $var}`: the variable starts at `""` (unless the request sets it) and is set to the last item's `id`;
- `--paginate skip` follows `skip: $var`, starting at 0 and moved on by `first`.

```graphql
query Transfers($last: ID) {
  transfers(first: 1000, where: {id_gt: $last}) { id from to value transactionHash }
}
```

Paging stops when a page returns fewer than `first` items, after `--max-pages` pages (default 20, at most 1000) with the warning `stopped at --max-pages 20`, or once the page bodies read reach 64 MiB in all, with the warning `stopped after N pages: they reached the 64 MiB pagination limit`. A later page that fails (a transport error, a result with errors, or a body that is not a GraphQL result) ends paging: the pages read before it are kept as the result, followed by the failing page's own errors and the error `--paginate: page N failed (<why>); keeping the N-1 pages read before it`, and the exit code is non-zero. A failing first page is the result as it is. An operation without a field taking `first`, or without the cursor variable, fails before anything is sent, with an error starting `graphql: --paginate:` and exit code 3. Pagination applies to HTTP operations, not subscriptions.

### Subgraph freshness

For the `subgraph` preset, Rustman sends `{ _meta { block { number } hasIndexingErrors } }` to the subgraph and reads the chain head (`eth_blockNumber`) from the environment's EVM node, the `rpc` variable. The node gets none of the GraphQL request's headers or auth: a subgraph API key never reaches it. The lag is shown in the TUI and on `rustman graphql query`'s status line as `lag 12 blocks`; without an `rpc` variable it is `unknown (no rpc)`. `rustman run` reads freshness only for requests with a `subgraph` assertion:

```yaml
version: 1
kind: graphql
name: Indexer is fresh
endpoint: '{{subgraph}}'
schema_file: subgraph.graphql
document: '{ tokens(first: 1) { id totalSupply decimals } }'
assertions:
  - subgraph: {max_lag_blocks: 50}
```

The assertion fails when the lag exceeds the bound (`subgraph lag 120 blocks exceeds 50`), when `hasIndexingErrors` is true (`subgraph has indexing errors`), and without an `rpc` variable (`cannot compute lag: no rpc configured`). On a request without the subgraph preset it is an error.

### Links into the toolkit

In the TUI, `L` in the response lists the addresses (20-byte values) and transaction hashes (32-byte values in fields named `hash`, `transactionHash`, `txHash`, ...) of a result with a preset. Choosing one offers:

- **Decode transaction**: the transaction and its receipt decoded, as `rustman evm tx` shows them;
- **Show balance**: the address's balance in the native currency;
- **Call contract**: a new `evm-call` tab to the address (ERC-20 ABI, `totalSupply`) to edit;
- **Open in explorer**: the explorer page from the known-chain registry, when the environment pins `chain_id`.

Lookups read the environment's `rpc`, check its `chain_id` pin first, use read-only methods only and are redacted. Links whose value is a secret are left out.

## Agent tools

`rustman mcp serve` registers six read-only tools (see the [tool reference](agent.md#tool-reference)):

| Tool | Does |
|---|---|
| `evm_call` | A saved `evm-call` request (`path`) or an ad-hoc call (`rpc`, `to`, `abi`, `function`, `args`, `block`, `from`, `estimate`), with its assertions. |
| `evm_decode` | Offline decoding of call data, revert data or a log. |
| `evm_tx` | A transaction and its receipt, decoded. |
| `evm_block` | A block by number, tag or hash. |
| `evm_logs_query` | Logs over a block range of at most 10,000 blocks: wider is `range too large (max 10000 blocks)`, before anything is sent. |
| `evm_ens` | Forward and verified reverse ENS resolution. |

Every node call goes through the session's `rpc_call` sender, so the safety policy's host allowlist and rate limit apply (a host outside it is `blocked`), and through the JSON-RPC method guards. Results are redacted. No tool accepts a signed transaction, and none can call `eth_sendRawTransaction`, `eth_sendTransaction` or a signing method.

## Exit codes

| Code | When |
|---|---|
| 1 | An `evm` assertion (or another assertion) failed; not found (an unknown transaction or block, an ENS name without an address). |
| 2 | A bad file, setting, argument or ABI; explorer fetch disabled; an agent log range over 10,000 blocks; `anvil` not found. |
| 3 | The node or the explorer could not be reached, or answered with an error; a node without state overrides. |
| 4 | Refused: a chain mismatch, an `evm-tx` given to `rustman send` (only the signing gate sends one), an `ens_rpc` that is not mainnet, a call a method guard refuses (a send method, or catalog params of the wrong type), or a host the safety policy blocks. |
