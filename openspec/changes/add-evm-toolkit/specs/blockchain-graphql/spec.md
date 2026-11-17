## ADDED Requirements

### Requirement: Endpoint presets
A `kind: graphql` request file MAY set `preset: geth` (EIP-1767 endpoint, usually `<rpc>/graphql`) or `preset: subgraph` (The Graph or compatible indexers). The system SHALL also auto-detect presets after introspection: `geth` when the schema has `Query.block` and `Query.logs` with `Long`/`Bytes32` scalars, `subgraph` when it has `Query._meta`. The detected preset SHALL be shown in the TUI.

#### Scenario: Subgraph detected
- **GIVEN** an introspected schema containing `_meta`
- **WHEN** the request is opened
- **THEN** the preset SHALL be shown as `subgraph`

#### Scenario: Explicit preset wins
- **GIVEN** `preset: geth` on a schema without the geth markers
- **THEN** the geth preset SHALL apply and a warning `schema does not look like geth EIP-1767` SHALL be shown

### Requirement: Blockchain scalar display
With a preset active, `BigInt`, `BigDecimal` and `Long` values SHALL display as exact decimals; `Bytes` and `Bytes32` as `0x` hex; and fields typed or named as addresses as EIP-55 checksummed addresses. Values SHALL be shown unchanged in raw mode, and assertions SHALL compare the raw JSON values.

#### Scenario: BigInt formatting
- **GIVEN** a subgraph field `totalSupply: "1000000000000000000000"` of type `BigInt`
- **THEN** it SHALL display as `1000000000000000000000`, with a unit toggle offering `1000 (18 decimals)` when the token's `decimals` is in the same result

#### Scenario: Address checksum
- **GIVEN** a lowercase address in a result
- **THEN** it SHALL display in EIP-55 checksummed form

### Requirement: Pagination helpers
For subgraph queries, the system SHALL offer `--paginate` (CLI) and a TUI action that follows `id_gt` pagination (default), or `first`/`skip` when `--paginate skip` is given, until a page returns fewer than `first` items or `--max-pages` (default 20) is reached. Results SHALL be concatenated into one array per paginated field.

#### Scenario: id_gt pagination
- **GIVEN** a query on `transfers(first: 1000, where: {id_gt: $last})`
- **WHEN** it runs with `--paginate`
- **THEN** the system SHALL repeat it with `$last` set to the last id until a page has fewer than 1000 items

#### Scenario: Page cap
- **GIVEN** more than 20 full pages exist
- **THEN** pagination SHALL stop at 20 pages with the warning `stopped at --max-pages 20`

### Requirement: Subgraph freshness check
For the `subgraph` preset, the system SHALL query `_meta { block { number } hasIndexingErrors }` and compare with the chain head from the environment's EVM RPC (if configured). The TUI SHALL show `lag N blocks`. An assertion `subgraph: {max_lag_blocks: N}` SHALL fail when the lag exceeds N or `hasIndexingErrors` is true.

#### Scenario: Stale subgraph
- **GIVEN** `- subgraph: {max_lag_blocks: 50}` and a lag of 120 blocks
- **THEN** the assertion SHALL fail with `subgraph lag 120 blocks exceeds 50`

#### Scenario: No RPC configured
- **GIVEN** no EVM RPC in the environment
- **THEN** the lag SHALL be shown as `unknown (no rpc)` and `max_lag_blocks` SHALL fail with `cannot compute lag: no rpc configured`

### Requirement: Links into the EVM decoder
In GraphQL results with a preset active, values recognized as addresses (20 bytes) or transaction hashes (32 bytes in a field named like `hash`/`transactionHash`/`txHash`) SHALL be actionable in the TUI: open in `evm-toolkit` (decode transaction, show balance, call contract) or open the explorer URL from the known-chain registry.

#### Scenario: Open transaction from result
- **GIVEN** a subgraph result with `transactionHash: 0xabc…`
- **WHEN** the user selects it and chooses "Decode transaction"
- **THEN** the EVM transaction view SHALL open for that hash on the environment's chain
