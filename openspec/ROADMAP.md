# Rustman — Product Roadmap & Shared Vocabulary

Rustman is a terminal-first, local-first alternative to Postman, written in Rust. One engine serves three faces:

```
                ┌──────────── rustman (single binary) ────────────┐
 human  ──────▶ │  TUI (ratatui)                                  │
 script/CI ───▶ │  CLI  (rustman run / explore / mcp ...)         │
 AI agent ────▶ │  MCP server (stdio | streamable HTTP)           │
                │        │                                        │
                │        ▼                                        │
                │  core engine: workspace · environments · HTTP   │
                │  GraphQL · MCP client · assertions · explorer   │
                │  history · reports · webhook inspector          │
                └──────────────┬──────────────────────────────────┘
                               │ viz protocol (JSON lines over a local pipe)
                               ▼
                 rustman-viz (optional, cargo feature `raven`)
                 Raven GUI: charts, plots, tables, API graph
```

## Changes, in implementation order

Each change lives in `openspec/changes/<id>/`. Archive it once it's implemented. Its delta specs then become the living specs in `openspec/specs/`.

| # | Change | Introduces capabilities | Depends on |
|---|--------|-------------------------|------------|
| 1 | `bootstrap-core-engine` | `workspace`, `environments`, `http-requests`, `assertions`, `cli-runner`, `request-history` | — |
| 2 | `add-terminal-ui` | `tui` | 1 |
| 3 | `add-graphql-client` | `graphql-requests` | 1 (TUI parts: 2) |
| 4 | `add-mcp-client` | `mcp-client` | 1 (TUI parts: 2) |
| 5 | `add-openapi-explorer` | `openapi-import`, `api-exploration`, `test-reports` | 1 |
| 6 | `add-agent-mcp-server` | `agent-mcp-interface` | 1, 5 |
| 7 | `add-webhook-inspector` | `webhook-inspector` | 1 (TUI parts: 2) |
| 8 | `add-raven-visualizer` | `visual-dashboards` | 1, 5 (TUI launch: 2) |
| 9 | `add-jsonrpc-client` | `jsonrpc-requests` | 1 (TUI parts: 2) |
| 10 | `add-evm-toolkit` | `evm-toolkit`, `evm-signing`, `chain-exploration`, `blockchain-graphql` | 1, 9 (GraphQL presets: 3; reports: 5) |
| 11 | `add-importers` | `collection-import` | 1 |
| 12 | `add-openapi-mock-server` | `mock-server` | 1, 5 |
| 13 | `add-snapshot-testing` | `snapshot-testing` | 1 (TUI approve flow: 2) |
| 14 | `add-grpc-client` | `grpc-requests` | 1 (TUI parts: 2) |
| 15 | `add-realtime-client` | `realtime-sessions` (WebSocket, SSE) | 1 (TUI parts: 2) |
| 16 | `add-messaging-client` | `messaging-requests` (Kafka, MQTT, AMQP via AsyncAPI) | 1 |
| 17 | `add-traffic-recorder` | `traffic-recorder` | 1 |
| 18 | `add-load-testing` | `load-testing` | 1 (live charts: 8) |
| 19 | `add-trace-propagation` | `trace-propagation` (OpenTelemetry) | 1 |
| 20 | `add-contract-testing` | `contract-testing` | 1, 5 |
| 21 | `add-docs-generator` | `api-docs-generation` | 1 (examples: `request-history`; code samples: snippet engine from 25) |
| 22 | `add-scheduled-monitors` | `monitors` | 1 |
| 23 | `add-plugin-system` | `plugins` (WASM) | 1 |
| 24 | `add-secret-providers` | `secret-providers` | 1 |
| 25 | `add-runner-parity` | extends `cli-runner` (data-driven runs), `http-requests` (Digest, AWS SigV4, JWT auth; snippet export), `tui` (Copy as…), `collection-import` (script migration) | 1 (TUI parts: 2; script migration: 11) |

Milestones:
- **M1 "usable"** = 1 + 2. A REST client with workspace, environments, TUI and CLI runner.
- **M2 "multi-protocol"** = 3 + 4 + 7. GraphQL, MCP client, webhooks.
- **M3 "autonomous"** = 5 + 6. OpenAPI exploration, plus an agent interface.
- **M4 "visual"** = 8.
- **M5 "chain"** = 9 + 10. JSON-RPC, plus an ABI-aware, read-only-by-default Ethereum console.
- **M6 "adoption"** = 11 + 12 + 13 + 25. Importers, mock server, snapshot tests, and Postman/Newman parity (data-driven runs, Digest/SigV4/JWT auth, code snippets, script migration). The data-run and auth parts of 25 depend only on 1, so they can land right after M1.
- **M7 "protocols"** = 14 + 15 + 16. gRPC, WebSocket/SSE, message brokers.
- **M8 "pro"** = 17 to 24. Recorder, load tests, tracing, contracts, docs, monitors, plugins, secret providers.

## Request kinds

`workspace` keeps a registry of request kinds, and each change registers its own. `tui` uses the same registry to pick the editor pane.

| Kind | Registered by |
|------|---------------|
| `http`, `graphql`, `mcp` | `bootstrap-core-engine` (`graphql` and `mcp` run only once changes 3 and 4 land; until then they're reported as skipped) |
| `jsonrpc` | `add-jsonrpc-client` |
| `evm-call`, `evm-tx` | `add-evm-toolkit` |
| `grpc` | `add-grpc-client` |
| `ws`, `sse` | `add-realtime-client` |
| `kafka`, `mqtt`, `amqp` | `add-messaging-client` |

## Glossary (use these exact terms in specs)

| Term | Meaning |
|------|---------|
| **Workspace** | A directory containing `rustman.toml`. It's the root of everything the user versions in Git. |
| **Collection** | A folder inside the workspace that groups requests. Folders can nest. |
| **Request file** | One saved request, one file (`*.rman.yaml`). Its `kind` comes from the request-kind registry (see Request kinds). |
| **Environment** | A named set of variables (`environments/<name>.yaml`). Exactly one is active at a time. |
| **Variable** | A `{{name}}` placeholder resolved at send time. Precedence, highest first: runtime override > in-run captures > data row > request `variables` > collection `variables` > persisted captures > environment > `rustman.toml` (owned by `environments`). |
| **Data row** | One row of a `--data` CSV/JSON file. A data-driven run executes the selected requests once per row. |
| **Secret** | A variable whose value is stored outside the workspace (OS keychain or the git-ignored `.rustman/secrets.yaml`). It's redacted as `«redacted:name»` everywhere it could leak. |
| **Assertion** | A declarative check on a response (status, header, JSONPath, JSON Schema, latency). It doesn't involve a scripting language. |
| **Capture** | Extracting a value from a response into a variable for later requests. |
| **Run** | Executing one or more request files headlessly. It produces a run report. |
| **Exchange** | One executed request plus its response, timings and assertion results. History stores these. |
| **Exploration** | An automated campaign that derives test cases from an OpenAPI document and runs them against a target. |
| **Probe** | One generated test case inside an exploration. |
| **Finding** | A probe outcome that violates the spec or a security expectation. Each has a severity. |
| **Safety policy** | The rules that decide whether a request may be sent: host allowlist, allowed methods, rate limit, and destructive opt-in. Explorations and agents always go through it. |
| **Target** | A base URL plus the auth an exploration or agent runs against. |
| **MCP server (remote)** | An external server that Rustman *connects to* as a client (capability `mcp-client`). |
| **Agent interface** | Rustman *acting as* an MCP server for AI agents (capability `agent-mcp-interface`). |
| **Viz companion** | The optional `rustman-viz` GUI binary built on Raven. |
| **Chain** | An EVM network identified by its chain id. An environment pins the expected chain id. |
| **Signer** | A key source for EVM transactions: an encrypted keystore, the OS keychain, or a hardware wallet. Never a raw key inside workspace files. |
| **Snapshot** | A stored, normalized response that later runs are compared against. |
| **Mock** | A route served by the mock server, generated from OpenAPI or written by hand. |
| **Plugin** | A sandboxed WASM module that adds auth schemes, assertions, request kinds or importers. |
| **Secret provider** | An external store for secrets (1Password, Vault, a cloud secret manager, SOPS). Referenced as `{{secret:provider/path}}`. |
| **Monitor** | A collection run on a schedule, with alert rules. |

## Crate layout (target)

```
crates/
  rustman-core/      workspace, environments, variables, secrets, request model, safety policy
  rustman-http/      HTTP execution (hyper + custom timed connector), timings, TLS, auth helpers, snippets
  rustman-graphql/   GraphQL execution, introspection, schema cache, graphql-ws subscriptions
  rustman-mcp/       MCP client and server (official `rmcp` SDK)
  rustman-explore/   OpenAPI parsing, probe generation, exploration engine, findings
  rustman-report/    run/exploration reports (JSON, JUnit XML, Markdown, HTML)
  rustman-webhook/   webhook inspector server (hyper)
  rustman-tui/       ratatui front-end
  rustman-cli/       `rustman` binary (clap); wires TUI, CLI subcommands, MCP server
  rustman-viz/       optional Raven GUI companion binary (feature `raven`, links the C lib)
  rustman-jsonrpc/   generic JSON-RPC 2.0 client (HTTP, WebSocket)
  rustman-evm/       EVM toolkit on `alloy`: ABI, decoding, simulation, ENS, signing gate
  rustman-import/    Postman, Insomnia, Bruno, HAR and .http importers
  rustman-mock/      OpenAPI mock server (hyper)
  rustman-grpc/      gRPC client (tonic, reflection)
  rustman-realtime/  WebSocket and SSE sessions
  rustman-messaging/ Kafka, MQTT, AMQP clients
  rustman-proxy/     traffic recorder (hyper, local CA)
  rustman-bench/     load testing engine
  rustman-plugin/    WASM plugin host
```

Lesson carried over from Ghostman: never hand-roll HTTP/1.1 parsing. The webhook inspector and every server component use `hyper`.

## Cross-cutting contracts

These are defined once, in the capability named, and every other change reuses them. Don't redefine them.

| Contract | Owner capability | Notes |
|----------|------------------|-------|
| Redaction of secrets | `environments` | Covers every output path: TUI, history, reports, viz, MCP tool results. |
| Safety policy (host allowlist, methods, rate, destructive opt-in, production guard) | `api-exploration` | `agent-mcp-interface` reuses it. Implemented once in `rustman-core`, and every automated sender goes through it. |
| Report format `rustman.report/v1` (`kind: run \| exploration \| bench \| contract`, optional `subject`, optional per-exchange `trace`) | `test-reports` | `cli-runner` emits `kind: run` from day one. `load-testing` adds `bench`, `contract-testing` adds `contract`, `chain-exploration` adds `subject: evm-rpc`, and `trace-propagation` adds `trace`. Every extension is additive. |
| Viz protocol `rustman-viz/1` | `visual-dashboards` | JSON lines over the companion's stdin. Read-only consumer. |
| `rmcp` SDK version | `mcp-client` | `agent-mcp-interface` shares the same crate (`rustman-mcp`) and pin. |
| Request-kind registry | `workspace` | New kinds plug in without changing the core loader. |
| Workspace trust (`.rustman/trust.yaml`) | `mcp-client` | `plugins` reuses it. Spawning a stdio MCP server or loading plugins needs one-time trust; headless runs need `--trust` / `--trust-plugins`. |
| Things agents can never do | `agent-mcp-interface` | Sign transactions, start load tests, install plugins, read secret values. There are no tools for these. |
| Signing gate (explicit opt-in, decoded confirmation, chain-id match, agents can never sign) | `evm-signing` | No other capability may send a signed transaction. |
| Snippet engine (code samples per language, redacted; placeholder mode) | `http-requests` | `tui` (Copy as…) and `api-docs-generation` reuse it. One template per language. |
| Data-row reader (CSV/JSON) | `cli-runner` | `load-testing`'s `--data` uses the same reader. |
| Agent tools for new capabilities | `agent-mcp-interface` | Each change may register read-only tools. They're conditional, pass the safety policy, and are redacted. |

### CLI exit codes (all subcommands)

| Code | Meaning | Used by |
|------|---------|---------|
| 0 | Success: all checks passed | all |
| 1 | Checks failed: an assertion, a finding at or above `--fail-on`, a tool error, or a new finding in `report diff` | run, send, explore, graphql, mcp-client, report diff |
| 2 | Usage or configuration error: bad flag, invalid file, unsupported spec version, or unknown report version | all |
| 3 | A request errored: network, TLS, timeout, unresolved variable. It wins over 1. | run, send |
| 4 | Refused by the safety policy or the signing gate | explore, evm, agent-triggered CLI paths |
| 130 | Interrupted (SIGINT) | all |
