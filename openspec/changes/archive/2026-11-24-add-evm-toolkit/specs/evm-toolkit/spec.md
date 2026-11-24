## ADDED Requirements

### Requirement: Chain pinning per environment
An environment MAY set `chain_id` (integer). Before the first EVM request to an endpoint in a session, the system SHALL call `eth_chainId` and compare. A mismatch SHALL abort the request with `chain mismatch: environment '<env>' expects <expected> (<name>), endpoint reports <actual> (<name>)` and exit code 4. Without `chain_id`, the detected chain SHALL be shown and used, with no error.

#### Scenario: Mismatch blocks the request
- **GIVEN** environment `sepolia` with `chain_id: 11155111`
- **AND** an RPC URL that answers `eth_chainId` with `0x1`
- **WHEN** the user runs `rustman evm call ...` with `--env sepolia`
- **THEN** no call SHALL be sent beyond `eth_chainId`
- **AND** the error SHALL read `chain mismatch: environment 'sepolia' expects 11155111 (Sepolia), endpoint reports 1 (Ethereum Mainnet)` with exit code 4

#### Scenario: Unpinned environment
- **GIVEN** an environment without `chain_id`
- **WHEN** an EVM request is sent to an endpoint reporting chain id 8453
- **THEN** the request SHALL proceed and the status SHALL show `Base (8453)`

### Requirement: Node client detection
The system SHALL identify the node from `web3_clientVersion` (falling back to `net_version` for the network) and classify it as `geth`, `reth`, `erigon`, `nethermind`, `besu`, `anvil`, `hardhat` or `unknown`. The client name and version SHALL be shown in the TUI status bar and included in `evm` command JSON output. Detection results SHALL be cached per endpoint for the session.

#### Scenario: Geth detected
- **GIVEN** `web3_clientVersion` returns `Geth/v1.14.11-stable/linux-amd64/go1.23.2`
- **WHEN** the endpoint is first used
- **THEN** the client SHALL be reported as `geth 1.14.11`

#### Scenario: Unsupported method
- **GIVEN** `web3_clientVersion` returns JSON-RPC error `-32601`
- **THEN** the client SHALL be reported as `unknown` and no error SHALL be shown to the user

### Requirement: Known-chain registry
The system SHALL ship a registry with name, native currency symbol and decimals, and block explorer base URL for: Ethereum Mainnet (1), Sepolia (11155111), Holesky (17000), Base (8453), OP Mainnet (10), Arbitrum One (42161), Polygon PoS (137) and Anvil/Hardhat local (31337). Users SHALL be able to add or override entries in `[evm.chains.<id>]` in `rustman.toml`. Unknown ids SHALL display as `chain <id>` with symbol `ETH`.

#### Scenario: Custom chain
- **GIVEN** `rustman.toml` defines `[evm.chains.999] name = "My L2"`, `symbol = "MYL"`
- **WHEN** an endpoint reports chain id 999
- **THEN** the status SHALL show `My L2 (999)` and native values SHALL be formatted in `MYL`

### Requirement: Method catalog with typed parameters
The system SHALL provide a catalog of standard `eth_*`, `net_*` and `web3_*` methods with parameter names and types (address, quantity, data, block tag, hash, filter object). The TUI SHALL render a typed form per method and the CLI SHALL accept `--param name=value`. Invalid values (bad address checksum, non-hex data, unknown block tag) SHALL be rejected before sending with a message naming the parameter.

#### Scenario: Typed balance query
- **WHEN** the user picks `eth_getBalance` and fills `address = 0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045`, `block = latest`
- **THEN** the request SHALL be `{"method":"eth_getBalance","params":["0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045","latest"]}`

#### Scenario: Bad checksum rejected
- **GIVEN** a mixed-case address whose EIP-55 checksum is wrong
- **WHEN** the user sends the request
- **THEN** it SHALL be rejected with `param 'address': invalid EIP-55 checksum`

### Requirement: Unit-aware value display
Quantities SHALL be displayed with a toggle between hex, decimal and, for native-value fields (`value`, `balance`, `gasPrice`, fees), unit forms `wei`, `gwei` and `ether` (or the chain's native symbol). Inputs SHALL accept the same forms, e.g. `1.5 ether`, `30 gwei`, `0x0de0b6b3a7640000`, `1000000000000000000`. Conversion SHALL be exact (no floating point).

#### Scenario: Balance shown in ether
- **GIVEN** `eth_getBalance` returns `0x0de0b6b3a7640000`
- **THEN** the default display SHALL be `1 ETH` with the toggle offering `1000000000000000000 wei` and `0x0de0b6b3a7640000`

#### Scenario: Exact input parsing
- **WHEN** the user enters `0.000000000000000001 ether`
- **THEN** it SHALL encode as `0x1`

### Requirement: evm-call request files
A `kind: evm-call` request file SHALL contain `rpc` (URL or `{{variable}}`), `to` (address or ENS name), `abi` (source), `function` (name or full signature), `args` (list), and optional `from`, `value`, `block` (default `latest`), `state_overrides`, `assertions` and `captures`. It SHALL execute as a read-only `eth_call`; it SHALL never produce a signed transaction.

#### Scenario: ERC-20 balance call
- **GIVEN** a file with `to: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"`, `abi: erc20`, `function: balanceOf`, `args: ["vitalik.eth"]`
- **WHEN** it is sent
- **THEN** the system SHALL resolve the ENS name, encode `balanceOf(address)`, call `eth_call` and show the decoded `uint256` formatted with the token's `decimals()` and `symbol()`

#### Scenario: Overloaded function needs signature
- **GIVEN** an ABI with `transfer(address,uint256)` and `transfer(address,uint256,bytes)`
- **AND** `function: transfer`
- **WHEN** the file loads
- **THEN** loading SHALL fail with `function 'transfer' is overloaded; use a full signature such as 'transfer(address,uint256)'`

### Requirement: ABI sources
`abi` SHALL accept a built-in name (`erc20`, `erc721`, `erc1155`), a path to a JSON ABI, a path to a Foundry (`out/*.json`) or Hardhat (`artifacts/**/*.json`) artifact (the `abi` field is extracted), or `{explorer: true}`. Paths SHALL resolve relative to the request file and SHALL NOT escape the workspace. Parsed ABIs SHALL be cached by content hash.

#### Scenario: Foundry artifact
- **GIVEN** `abi: ../out/Vault.sol/Vault.json`
- **WHEN** the request loads
- **THEN** the `abi` array of the artifact SHALL be used

### Requirement: Verified ABI fetch is opt-in
`abi: {explorer: true}` SHALL fetch the verified ABI from an Etherscan-compatible API configured in `[evm.explorers.<chain_id>]` (`api_url`, API key as a secret) only when `[evm] allow_explorer_fetch = true`. Fetched ABIs SHALL be cached in `.rustman/abi-cache/<chain>/<address>.json`. Proxy contracts (EIP-1967 implementation slot) SHALL resolve to the implementation ABI and say so.

#### Scenario: Fetch disabled by default
- **GIVEN** `allow_explorer_fetch` is not set
- **WHEN** a request uses `abi: {explorer: true}`
- **THEN** it SHALL fail with `explorer ABI fetch is disabled; set [evm] allow_explorer_fetch = true` and no network call SHALL be made to the explorer

#### Scenario: Proxy resolved
- **GIVEN** fetch is enabled and the address is an EIP-1967 proxy
- **WHEN** the ABI is fetched
- **THEN** the implementation's ABI SHALL be used and the UI SHALL show `proxy → implementation 0x…`

### Requirement: Revert and custom error decoding
When a call reverts, the system SHALL decode `Error(string)`, `Panic(uint256)` (with the Solidity panic meaning, e.g. `0x11 arithmetic overflow`) and any custom error present in the loaded ABI. Undecodable revert data SHALL be shown as hex with its 4-byte selector.

#### Scenario: Custom error
- **GIVEN** an ABI declaring `error InsufficientBalance(uint256 available, uint256 required)`
- **WHEN** the call reverts with that error
- **THEN** the result SHALL show `reverted: InsufficientBalance(available=5, required=10)`

#### Scenario: Panic code
- **WHEN** a call reverts with `Panic(0x12)`
- **THEN** the result SHALL show `reverted: Panic(0x12) division or modulo by zero`

### Requirement: Transaction, receipt and log decoding
`rustman evm tx <hash>` and the TUI SHALL fetch a transaction and its receipt and show from, to, value, nonce, gas used, effective gas price, fee paid, status and block. Input data and logs SHALL be decoded with any ABI known for the target addresses (built-in token ABIs, loaded or cached ABIs). Undecodable logs SHALL show topics and data as hex.

#### Scenario: Token transfer decoded
- **GIVEN** a receipt with a USDC `Transfer` log
- **WHEN** the user runs `rustman evm tx 0x…`
- **THEN** the log SHALL display `Transfer(from=0x…, to=0x…, value=250.00 USDC)`

#### Scenario: Pending or unknown transaction
- **WHEN** the hash is unknown to the node
- **THEN** the command SHALL print `transaction not found` and exit with code 1

### Requirement: Built-in token standards
The system SHALL include ERC-20, ERC-721 and ERC-1155 ABIs. ERC-20 amounts SHALL be formatted using the token's `decimals()` and `symbol()` (fetched once per token per session and cached). If `decimals()` fails, the raw integer SHALL be shown with a warning.

#### Scenario: Token without decimals
- **GIVEN** a token contract whose `decimals()` reverts
- **WHEN** a `Transfer` log is decoded
- **THEN** the raw value SHALL be shown with the warning `decimals() unavailable; showing raw amount`

### Requirement: Head and log streams
`rustman evm logs` and the TUI SHALL stream new heads or logs filtered by `address` (one or many) and `topics` (including event-signature shorthand like `Transfer(address,address,uint256)`). Over WebSocket the system SHALL use `eth_subscribe`. Over HTTP it SHALL fall back to `eth_newFilter` + `eth_getFilterChanges` polling every 2 s (configurable). Decoded events SHALL appear in a live timeline.

#### Scenario: WebSocket subscription
- **GIVEN** a `wss://` endpoint
- **WHEN** the user runs `rustman evm logs --address 0xA0b8… --event "Transfer(address,address,uint256)"`
- **THEN** the system SHALL use `eth_subscribe` with that filter and print one decoded event per line

#### Scenario: HTTP polling fallback
- **GIVEN** an `https://` endpoint
- **WHEN** the same command runs
- **THEN** the system SHALL poll `eth_getFilterChanges` every 2 seconds and print a note `polling every 2s (no WebSocket)`

#### Scenario: Reorg marked
- **WHEN** a streamed log arrives with `removed: true`
- **THEN** it SHALL be shown as `REORGED` and not counted as a new event

### Requirement: Simulation with state overrides
`evm-call` requests and `rustman evm call` SHALL support simulating at any block (`block` tag, number or hash) with `state_overrides` (balance, nonce, code, storage per address) and SHALL report `eth_estimateGas` for the same call when `--estimate` is given. If the node does not support state overrides, the request SHALL fail with `node does not support state overrides`.

#### Scenario: Historical call
- **GIVEN** `block: 19000000`
- **WHEN** the call runs against an archive node
- **THEN** the result SHALL reflect state at block 19000000

#### Scenario: Balance override
- **GIVEN** `state_overrides: {"0xabc…": {balance: "100 ether"}}`
- **WHEN** the call runs
- **THEN** `eth_call` SHALL include the override object as its third parameter

### Requirement: Call traces when supported
With `--trace`, the system SHALL request a call tree via `debug_traceCall` with the `callTracer`, falling back to `trace_call` (Erigon/Nethermind style). The tree SHALL show each frame's type, from, to, decoded function, value, gas and revert reason. When neither method is available, the system SHALL report `tracing not supported by <client>` and still return the plain call result.

#### Scenario: Degrade without tracing
- **GIVEN** a node returning `-32601` for both trace methods
- **WHEN** the user runs `rustman evm call ... --trace`
- **THEN** the call result SHALL be printed followed by `tracing not supported by geth` (exit code unaffected)

### Requirement: ENS resolution
Any address input SHALL accept an ENS name. Resolution SHALL use Ethereum Mainnet's ENS registry through the endpoint configured as `[evm] ens_rpc` (default: the current endpoint if it is chain 1). `rustman evm ens <name|address>` SHALL do forward or reverse resolution, and reverse names SHALL be verified by forward lookup before display.

#### Scenario: Forward resolution
- **WHEN** the user runs `rustman evm ens vitalik.eth`
- **THEN** the output SHALL be the resolved checksummed address

#### Scenario: No ENS endpoint
- **GIVEN** the current endpoint is Sepolia and `ens_rpc` is unset
- **WHEN** an argument is `vitalik.eth`
- **THEN** the request SHALL fail with `ENS needs a mainnet RPC; set [evm] ens_rpc`

### Requirement: Anvil fork management
`rustman evm fork --chain <name|id> [--block N] [--port P]` SHALL start `anvil --fork-url <rpc> --fork-block-number N` using the environment's RPC for that chain, print the local URL, and register a temporary environment `fork-<chain>` with `chain_id` set to the fork's id. If `anvil` is not on `PATH`, it SHALL fail with `anvil not found; install Foundry (https://getfoundry.sh)`. The fork SHALL stop when the command exits.

#### Scenario: Fork mainnet
- **GIVEN** `anvil` is installed and the `mainnet` environment has an RPC URL
- **WHEN** the user runs `rustman evm fork --chain mainnet --block 20000000`
- **THEN** anvil SHALL start on `127.0.0.1:8545` forking block 20000000
- **AND** an environment `fork-mainnet` SHALL be available until the command exits

#### Scenario: Anvil missing
- **WHEN** `anvil` is not on `PATH`
- **THEN** the command SHALL exit with code 2 and the install hint

### Requirement: EVM assertions
Request files of EVM kinds SHALL support `evm` assertions: `{output: <name|index>, <op>: value}` with ops from `assertions` (`equals`, `gt`, `gte`, `lt`, `lte`, `contains`) where values accept unit forms (`1 ether`, `30 gwei`); `{emits: "<Event(sig)>", where: {field: value}}`; `{reverts: true}` or `{reverts_with: "<reason or ErrorName>"}`; and `{gas_lt: <n>}`.

#### Scenario: Balance threshold
- **GIVEN** `- evm: {output: balance, gte: "1 ether"}` and a decoded `balance` of `0.5 ether`
- **THEN** the assertion SHALL fail with `evm output 'balance': expected >= 1 ETH, got 0.5 ETH`

#### Scenario: Expected revert
- **GIVEN** `- evm: {reverts_with: "InsufficientBalance"}`
- **WHEN** the call reverts with `InsufficientBalance(5, 10)`
- **THEN** the assertion SHALL pass

### Requirement: EVM captures
`captures` SHALL support `{var, from: evm_output, output: <name|index>}`, `{var, from: evm_event, event: "<sig>", field: <name>}` and `{var, from: evm_receipt, field: <contractAddress|blockNumber|status|gasUsed>}`. Captured quantities SHALL be stored as decimal strings and addresses as checksummed strings.

#### Scenario: Capture decoded output
- **GIVEN** `captures: [{var: owner, from: evm_output, output: 0}]` on an `owner()` call
- **WHEN** the call returns an address
- **THEN** `{{owner}}` SHALL hold the EIP-55 checksummed address for later requests

### Requirement: EVM CLI commands
The CLI SHALL provide `rustman evm call`, `decode` (calldata, revert data or a log by ABI), `logs`, `tx`, `block <number|tag|hash>`, `ens`, `fork` and `probe`, each with `--json` for machine output and `--env`/`--rpc` to choose the endpoint. Exit codes SHALL follow the ROADMAP table: 1 for assertion failure or a reverted call checked by an assertion, 3 for RPC transport errors, 4 for a chain mismatch.

#### Scenario: Decode calldata offline
- **WHEN** the user runs `rustman evm decode --abi erc20 0xa9059cbb000…`
- **THEN** the output SHALL be `transfer(to=0x…, amount=…)` with no network access

### Requirement: TUI pane for EVM kinds
The TUI SHALL register editor panes for `evm-call` and `evm-tx`: a function picker from the ABI, typed argument fields, block tag selector, unit toggles on quantities, a decoded result view, revert and trace views, and a live event timeline for streams. The status bar SHALL show chain name, chain id and node client.

#### Scenario: Function picker
- **GIVEN** an `evm-call` tab with an ABI loaded
- **WHEN** the user opens the function picker
- **THEN** only `view` and `pure` functions SHALL be listed by default, with a toggle to show all functions as read-only `eth_call` simulations

### Requirement: EVM read-method preset
This capability SHALL register the `@evm-read` preset for `[jsonrpc] agent_read_only_methods`. It SHALL expand to read-only methods only (`eth_chainId`, `eth_blockNumber`, `eth_call`, `eth_estimateGas`, `eth_getBalance`, `eth_getCode`, `eth_getStorageAt`, `eth_getTransactionCount`, `eth_getBlockBy*`, `eth_getTransaction*`, `eth_getLogs`, `eth_feeHistory`, `eth_gasPrice`, `net_version`, `web3_clientVersion`). It SHALL also register its signing and sending methods as JSON-RPC method guards.

#### Scenario: Preset enables reads only
- **GIVEN** `[jsonrpc] agent_read_only_methods = ["@evm-read"]`
- **WHEN** an agent calls `eth_getBalance` and then `eth_sendRawTransaction` via `rpc_call`
- **THEN** the first SHALL be allowed and the second SHALL be refused by the method guard

### Requirement: Read-only agent tools
When the build includes this capability, `agent-mcp-interface` SHALL expose `evm_call`, `evm_decode`, `evm_tx`, `evm_block`, `evm_logs_query` (bounded range, max 10,000 blocks) and `evm_ens`. All SHALL be read-only, pass the safety policy's host allowlist and rate limit, and be redacted. No agent tool SHALL sign, send raw transactions, or call `eth_sendRawTransaction` or `eth_sendTransaction`.

#### Scenario: Agent cannot send
- **WHEN** an agent lists tools
- **THEN** no tool SHALL accept a signed transaction or invoke `eth_sendRawTransaction`

#### Scenario: Log range bounded
- **WHEN** an agent calls `evm_logs_query` over 50,000 blocks
- **THEN** the tool SHALL return an error `range too large (max 10000 blocks)`

### Requirement: Send methods blocked outside the signing gate
Any `jsonrpc` or `evm-call` request whose method is `eth_sendRawTransaction`, `eth_sendTransaction`, `eth_sign`, `personal_sign`, `eth_signTransaction` or `eth_signTypedData*` SHALL be refused with exit code 4 and the message `signing and sending go through 'evm-tx' (signing gate)`, unless the target chain is local (31337) and `[evm] allow_raw_send_local = true`.

#### Scenario: Raw send blocked
- **GIVEN** a `kind: jsonrpc` file calling `eth_sendRawTransaction` on mainnet
- **WHEN** it is sent
- **THEN** it SHALL be refused with exit code 4 and no request SHALL leave the machine

#### Scenario: Send method in an http body
- **GIVEN** a `kind: http` request whose JSON body (single call or batch) calls `eth_sendTransaction`
- **WHEN** it is sent by `rustman send`, `rustman run` or the agent's `send_request`
- **THEN** it SHALL be refused with the same message (exit code 4, or a `blocked` tool error) and nothing SHALL be sent
