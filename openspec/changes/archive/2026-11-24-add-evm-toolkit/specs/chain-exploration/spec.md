## ADDED Requirements

### Requirement: Probe command
`rustman evm probe <rpc-or-env>...` SHALL probe one or more EVM RPC providers and produce a report. It SHALL accept `--methods <list|standard|all>` (default `standard`), `--block <n>` for historical checks, `--budget <requests>` (default 500 per provider), `--report <format>=<path>` (repeatable) and `--viz`. Probes SHALL send only read-only methods.

#### Scenario: Two providers
- **WHEN** the user runs `rustman evm probe https://rpc-a.example https://rpc-b.example`
- **THEN** both providers SHALL be probed and one report SHALL compare them

#### Scenario: No write methods
- **WHEN** any probe runs
- **THEN** no `eth_sendRawTransaction`, `eth_sendTransaction` or signing method SHALL be called

### Requirement: Probes pass the safety policy
Every probe request SHALL pass the safety policy from `api-exploration`: hosts limited to the given providers, default rate 10 req/s per provider, concurrency 4, and the request budget. Exceeding the budget SHALL stop probing that provider and mark remaining checks `skipped (budget)`.

#### Scenario: Budget exhausted
- **GIVEN** `--budget 50`
- **WHEN** a provider reaches 50 requests
- **THEN** its remaining checks SHALL be `skipped (budget)` and the report SHALL say so

### Requirement: Method support matrix
For each provider the system SHALL call each catalog method with valid minimal parameters and classify it as `supported`, `unsupported` (JSON-RPC `-32601` or equivalent), `restricted` (auth or plan error) or `error`. The matrix (methods × providers) SHALL appear in the report's coverage section.

#### Scenario: Trace methods missing
- **GIVEN** a provider that returns `-32601` for `debug_traceCall`
- **THEN** the matrix SHALL mark `debug_traceCall` as `unsupported` for that provider

### Requirement: Archive node detection
The system SHALL query `eth_getBalance` of a known address at an old block (default: block 1,000,000 or `--block`, capped to head − 10,000) and classify the provider as `archive` (succeeds), `full` (fails with a missing-trie-node or pruned-state error), or `unknown`.

#### Scenario: Pruned state
- **GIVEN** the provider answers `missing trie node`
- **THEN** the provider SHALL be classified `full`

### Requirement: Rate-limit discovery
With `--rate-discovery`, the system SHALL increase request rate stepwise (5, 10, 20, 40 req/s, never above `--max-rate`, default 40) for a cheap method (`eth_blockNumber`) and record the rate at which HTTP 429 or JSON-RPC rate-limit errors start. It SHALL stop at the first limit and honor `Retry-After`. Without the flag, no rate discovery SHALL run.

#### Scenario: Limit found
- **GIVEN** a provider that returns 429 above 20 req/s
- **WHEN** the probe runs with `--rate-discovery`
- **THEN** the report SHALL record `rate limit ≈ 20 req/s` and probing SHALL back off

### Requirement: Multi-provider consistency
With two or more providers, the system SHALL compare: head block number (drift in blocks, sampled 5 times over 10 s), `eth_getBlockByNumber` hash at a common recent block, and `eth_getBalance` and an `eth_call` (if `--call` is given) at the same block. Disagreements SHALL be findings: head drift over `--max-drift` (default 3 blocks) is `medium`, mismatched block hash or state at the same block is `high`.

#### Scenario: State disagreement
- **GIVEN** providers A and B return different balances for the same address at block 20000000
- **THEN** the report SHALL contain a `high` finding with both responses as evidence

#### Scenario: Lagging provider
- **GIVEN** provider B is 8 blocks behind A on every sample
- **THEN** a `medium` finding `head drift 8 blocks` SHALL be reported for B

### Requirement: Latency per method
The system SHALL record latency (p50, p95, max) per method per provider over the probe's requests and include them in the report. `chain_id` mismatches between providers SHALL abort the comparison with exit code 2.

#### Scenario: Different chains
- **GIVEN** one provider is mainnet and another Sepolia
- **WHEN** they are probed together
- **THEN** the command SHALL fail with `providers report different chains (1, 11155111)` and exit code 2

### Requirement: Subscription support check
For `ws://` and `wss://` providers, the system SHALL test `eth_subscribe` for `newHeads` (receive at least one head within 30 s) and `logs`, reporting `supported`, `unsupported` or `timeout`.

#### Scenario: Subscription timeout
- **GIVEN** a WebSocket provider that accepts `eth_subscribe` but sends no head within 30 s
- **THEN** `newHeads` SHALL be reported as `timeout`

### Requirement: Reports and dashboards
Probe results SHALL be emitted as a `rustman.report/v1` report of `kind: exploration` with `subject: evm-rpc`, so all `test-reports` formats and `report diff` apply. Findings SHALL follow `api-exploration` severities and the exit code SHALL follow `--fail-on` (default `high`). With `--viz`, `visual-dashboards` SHALL show a provider × method latency heatmap, head/block-time and gas plots, and, for traced calls, a call-trace node graph.

#### Scenario: CI gate on providers
- **GIVEN** a `high` consistency finding
- **WHEN** the probe finishes with default `--fail-on high`
- **THEN** the exit code SHALL be 1

#### Scenario: Viz fallback
- **GIVEN** `--viz` and no companion installed
- **THEN** the probe SHALL complete normally and suggest `--report html=…`
