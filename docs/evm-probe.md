# Probing EVM providers

`rustman evm probe` checks one or more EVM JSON-RPC providers and compares them: which methods each one supports, whether it keeps historical state (archive), how fast each method answers, whether WebSocket subscriptions work, where its rate limit is, and whether the providers agree with each other. The result is a standard [exploration report](exploration.md) (`rustman.report/v1`, `kind: exploration`, `subject: evm-rpc`), so every [report format](reports.md), `rustman report diff` and `--fail-on` work as for `rustman explore run`.

A probe only reads. It sends methods from a fixed read-only catalog; `eth_sendRawTransaction`, `eth_sendTransaction` and every signing method are refused before anything is sent (exit code 4), and the [send-method guard](evm.md#send-methods) applies on top.

```sh
rustman evm probe https://rpc-a.example https://rpc-b.example
```

## Providers

Each argument is a provider:

- a URL: `https://`, `http://`, `wss://` or `ws://`;
- a `{{variable}}`, resolved with the active environment (`--env`), such as `{{rpc}}`;
- the name of an environment of the workspace: its `{{rpc}}` variable is probed, with that environment's secrets and `chain_id` pin.

```sh
rustman evm probe mainnet-a mainnet-b --methods all --report html=probe.html
```

API keys in URLs are secrets: keep them in environment secrets (`https://eth-mainnet.example/v2/{{alchemy_key}}`). Output, reports and viz events show `«redacted:alchemy_key»`. Credentials typed into a literal URL are hidden too, as `«redacted:url-key»`, in errors, output and the report's endpoints: the userinfo (`user:password@`), the values of key-like query parameters (`apikey`, `api_key`, `key`, `token`, `access_token`, `auth`, `secret`, or any name containing one of those words), and path segments of 20 or more letters, digits, `-` and `_` that mix letters and digits (not `0x` hex values such as block hashes and addresses). The probe then warns once that the URL holds what looks like an API key and suggests storing it as a secret, written `{{NAME}}` or `{{secret:<provider>/<path>}}`.

All providers must be on one chain. When they are not, the probe stops before any other request, with exit code 2:

```text
error: providers report different chains (1, 11155111)
```

An environment that pins `chain_id` must match its provider, as for every EVM command (exit code 4 otherwise).

## Checks

| Check | What it does |
|---|---|
| Method matrix | Calls each `--methods` method with valid minimal parameters (built from the provider's head block) and classifies it `supported`, `unsupported` (`-32601` or "method not found"), `restricted` (an auth or plan error, HTTP 401/403) or `error`. |
| Archive | `eth_getBalance` of `--address` at block 1,000,000 (or `--block`), never closer than 10,000 blocks to the head: `archive` when it answers, `full` on a pruned-state error (`missing trie node`), `unknown` otherwise. |
| Latency | p50, p95 and max per method per provider, over every request of the probe. HTTP latency is the time on the wire (request sent to last response byte), so the wait for `--rate` pacing is never counted; WebSocket latency is timed from the paced send. |
| Blocks | The last 8 blocks: block times and gas used, for the viz plots. |
| Subscriptions | For `ws://` and `wss://` providers: `eth_subscribe` to `newHeads` (`supported` once a head arrives within 30 s, `timeout` when none does) and to `logs` (`supported` once the subscription is accepted, since matching logs can be rare: when none arrives within the timeout the detail says `subscribed; no log arrived before the timeout` instead of reporting `timeout`); `unsupported` when the subscription is refused. |
| Trace | With `--call <address>:<calldata>`, the call is traced (`debug_traceCall`, then `trace_call`) for the call-trace graph. |
| Consistency | With two or more providers, see below. |
| Rate limit | Only with `--rate-discovery`, see below. |

`--methods` takes `standard` (the default), `all`, or a list:

```sh
rustman evm probe https://rpc-a.example --methods eth_call,eth_getBalance,debug_traceCall
```

`standard` holds `eth_chainId`, `web3_clientVersion`, `net_version`, `eth_blockNumber`, `eth_getBlockByNumber`, `eth_getBlockByHash`, `eth_syncing`, `eth_gasPrice`, `eth_maxPriorityFeePerGas`, `eth_feeHistory`, `eth_getBalance`, `eth_getCode`, `eth_getTransactionCount`, `eth_getStorageAt`, `eth_call`, `eth_estimateGas`, `eth_getLogs`, `eth_getTransactionByHash`, `eth_getTransactionReceipt`, `debug_traceCall` and `trace_call`. `all` adds `eth_getBlockReceipts`, `eth_getBlockTransactionCountByNumber`, `eth_getUncleCountByBlockNumber`, `eth_createAccessList`, `eth_getProof`, `eth_simulateV1`, `eth_blobBaseFee`, `debug_traceTransaction`, `trace_transaction`, `trace_block`, `txpool_status`, `net_listening` and `net_peerCount`.

## Consistency between providers

With two or more providers, the probe compares:

- the head: `eth_blockNumber` on every provider, 5 samples over 10 s (`--drift-interval` sets the time between samples). A provider behind the highest head by more than `--max-drift` blocks (default 3) on every sample is a `medium` finding, `head drift 8 blocks`;
- at one fixed block, 10 below the lowest head (so reorgs near the head cannot cause false alarms): the block hash (`eth_getBlockByNumber`), the balance of `--address` (`eth_getBalance`) and, with `--call`, the `eth_call` result. A provider that disagrees with the first one is a `high` finding whose evidence holds both answers.

```sh
rustman evm probe https://rpc-a.example https://rpc-b.example --call 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48:0x18160ddd
```

## Rate-limit discovery

`--rate-discovery` sends `eth_blockNumber` at 5, 10, 20 and 40 req/s, one second per step, never above `--max-rate` (default 40). It stops at the first HTTP 429 or JSON-RPC rate-limit error (`-32005`), waits out its `Retry-After`, and reports the last rate that passed: `rate limit ≈ 20 req/s` (an `info` finding). Without the flag no rate discovery runs.

```sh
rustman evm probe https://rpc-a.example --rate-discovery --max-rate 20
```

## Safety and budgets

Every request passes the [safety policy](safety.md): each provider's host only, 10 req/s and 4 requests in flight per provider (`--rate`, `--concurrency`, or `[safety]` in `rustman.toml`), and a budget of 500 requests per provider (`--budget`, at least 1). A provider that reaches its budget is not probed further: its remaining checks are `skipped (budget)`, and the report says so with an `info` finding. A host listed in `[safety].production_hosts`, or an environment marked `production: true`, needs `--production <host>`.

## Reports and exit codes

The probe prints a summary per provider (archive state, subscriptions, rate limit, and each method with its support and latency). `--report <format>=<path>` (repeatable) writes the report as `json`, `junit`, `markdown`, `html` or `sarif`; the method matrix is the report's coverage section (methods × providers; each cell's `class` is `supported`, `unsupported`, `restricted`, `error` or `skipped (budget)`), and the EVM details are in its `evm` section. Provider error messages are cut to 2 KiB in the report, and finding evidence to 16 KiB; the call-trace graph keeps at most 1000 frames (a final `… N more frames omitted` node counts the rest), and a frame's error text is cut to 256 bytes.

```sh
rustman evm probe https://rpc-a.example https://rpc-b.example --report json=probe.json --report sarif=probe.sarif
```

| Code | Meaning |
|---|---|
| 0 | No finding at or above `--fail-on` (default `high`). |
| 1 | A finding at or above `--fail-on`, such as providers disagreeing on state. |
| 2 | Bad flags, or providers on different chains. |
| 3 | A provider could not be reached. |
| 4 | A refusal: a write method in `--methods`, a chain pin mismatch, an unconfirmed production host. |
| 130 | Interrupted (Ctrl+C). |

## Dashboards

`--viz` sends the results as [viz protocol](viz-protocol.md) `chain_probe` events when the probe finishes (they are not streamed while it runs): the data of a provider × method latency heatmap, block-time and gas plots, and a call-trace graph for `--call`. The bundled [viz companion](viz.md) has no `chain_probe` view yet, so today it does not draw them: the probe completes normally, warns that the companion has no such view and suggests `--report html=…` instead. A companion that advertises the `chain_probe` view receives the events.
