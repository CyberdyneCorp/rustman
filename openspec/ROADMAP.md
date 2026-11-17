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

Milestones:
- **M1 "usable"** = 1 + 2. A REST client with workspace, environments, TUI and CLI runner.
- **M2 "multi-protocol"** = 3 + 4 + 7. GraphQL, MCP client, webhooks.
- **M3 "autonomous"** = 5 + 6. OpenAPI exploration, plus an agent interface.
- **M4 "visual"** = 8.

## Glossary (use these exact terms in specs)

| Term | Meaning |
|------|---------|
| **Workspace** | A directory containing `rustman.toml`. It's the root of everything the user versions in Git. |
| **Collection** | A folder inside the workspace that groups requests. Folders can nest. |
| **Request file** | One saved request, one file (`*.rman.yaml`). Its `kind` is `http`, `graphql` or `mcp`. |
| **Environment** | A named set of variables (`environments/<name>.yaml`). Exactly one is active at a time. |
| **Variable** | A `{{name}}` placeholder resolved at send time. Precedence, highest first: runtime override > captured > environment > workspace defaults. |
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

## Crate layout (target)

```
crates/
  rustman-core/      workspace, environments, variables, secrets, request model, safety policy
  rustman-http/      HTTP execution (reqwest + hyper), timings, TLS, auth helpers
  rustman-graphql/   GraphQL execution, introspection, schema cache, graphql-ws subscriptions
  rustman-mcp/       MCP client and server (official `rmcp` SDK)
  rustman-explore/   OpenAPI parsing, probe generation, exploration engine, findings
  rustman-report/    run/exploration reports (JSON, JUnit XML, Markdown, HTML)
  rustman-webhook/   webhook inspector server (hyper)
  rustman-tui/       ratatui front-end
  rustman-cli/       `rustman` binary (clap); wires TUI, CLI subcommands, MCP server
  rustman-viz/       optional Raven GUI companion binary (feature `raven`, links the C lib)
```

Lesson carried over from Ghostman: never hand-roll HTTP/1.1 parsing. The webhook inspector and every server component use `hyper`.

## Cross-cutting contracts

These are defined once, in the capability named, and every other change reuses them. Don't redefine them.

| Contract | Owner capability | Notes |
|----------|------------------|-------|
| Redaction of secrets | `environments` | Covers every output path: TUI, history, reports, viz, MCP tool results. |
| Safety policy (host allowlist, methods, rate, destructive opt-in, production guard) | `api-exploration` | `agent-mcp-interface` reuses it. Implemented once in `rustman-core`, and every automated sender goes through it. |
| Report format `rustman.report/v1` (`kind: run \| exploration`) | `test-reports` | `cli-runner` emits `kind: run` from day one. `test-reports` extends the schema additively. |
| Viz protocol `rustman-viz/1` | `visual-dashboards` | JSON lines over the companion's stdin. Read-only consumer. |
| `rmcp` SDK version | `mcp-client` | `agent-mcp-interface` shares the same crate (`rustman-mcp`) and pin. |

### CLI exit codes (all subcommands)

| Code | Meaning | Used by |
|------|---------|---------|
| 0 | Success: all checks passed | all |
| 1 | Checks failed: an assertion, a finding at or above `--fail-on`, a tool error, or a new finding in `report diff` | run, send, explore, graphql, mcp-client, report diff |
| 2 | Usage or configuration error: bad flag, invalid file, unsupported spec version, or unknown report version | all |
| 3 | A request errored: network, TLS, timeout, unresolved variable. It wins over 1. | run, send |
| 4 | Refused by the safety policy | explore, agent-triggered CLI paths |
| 130 | Interrupted (SIGINT) | all |
