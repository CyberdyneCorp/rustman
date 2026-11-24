# Rustman — Delivery Roadmap

This file is the **delivery plan**: what ships in which release, in what order, and why.
The **capability map, glossary and cross-cutting contracts** stay in [`openspec/ROADMAP.md`](openspec/ROADMAP.md),
and the requirements are the living specs in `openspec/specs/<capability>/` (each change, archived, is in `openspec/changes/archive/`). Change ids below are the OpenSpec change ids.

## Where we are

- **Specified:** 26 OpenSpec changes, 643 requirements, ~990 scenarios, ~645 implementation tasks.
- **Done:** Phase 0 and **v0.1 — Headless core**: Cargo workspace and CI (#1); workspace, environments, secrets and redaction (#2); the HTTP engine (#3); assertions, `send`/`run`, reporters and history (#47); the safety policy and governed sender (#48). `bootstrap-core-engine` is archived, and its living specs are in `openspec/specs/`.
- **Done:** **v0.2 — Usable**: the TUI workbench (#49, #54, #57), importers for Postman, Insomnia, Bruno, HAR and `.http` (#55), and runner parity: `run --data`, Digest/SigV4/JWT auth, code snippets (#56). All three exit criteria pass. The TUI "Import…" and "Copy as…" actions followed in #62. Still open: SigV4 `unsigned_payload` and the manual terminal matrix (snippet `--placeholders` closed with the docs generator).
- **Done:** **v0.3 — Autonomous**: the OpenAPI explorer (#58) and the agent MCP server (#63, `rustman mcp serve`, read-only by default). Both exit criteria pass in CI on Linux, macOS and Windows.
- **Done:** **v0.4 — Multi-protocol**: the webhook inspector (#59), the GraphQL client (#61), snapshot testing (#65), the OpenAPI mock server (#66) and the MCP client (#71, with HMAC-sealed workspace trust). Follow-ups: snapshot assertions for `kind: jsonrpc` (#72), GraphQL introspection size limit (#70), Windows job objects for MCP server cleanup.
- **Done:** **v0.5 — Protocols**: the JSON-RPC client (#68), the gRPC client (#76) and the realtime client for WebSocket and SSE (#80). Follow-ups: a bounded live feed for realtime sessions (#81) and shared SSE event ids (#82).
- **Done:** **v0.6 — Pro testing**: contract testing (#86, OpenAPI breaking-change diff, Pact generate/verify, passive conformance), trace propagation (#88, W3C/B3 context, trace links, OTLP export), load testing (#91, `rustman bench`), the docs generator (#92, `rustman docs build/serve`) and stress testing (#94, `rustman stress` breakpoint/spike/soak/fuzz with wire faults, kill switch, recovery and minimization). On the side track, the viz producer (#85, `--viz`, `rustman viz`) and the `rustman-viz` companion on Raven (#89) landed, plus the README demo GIF (#90). Follow-ups: trace ids for WebSocket, MCP-session and native gRPC exchanges, client-span export for explorations, and the Jaeger end-to-end CI job (trace-propagation 3.7, 4.5, 5.1); a bench dashboard in the companion and the bench end-to-end job (load-testing 6.3, 7.1); the companion has no stress view either, so `--viz` on `bench` and `stress` only warns.
- **Done:** **v0.7 — Operate & extend**: scheduled monitors (#97, `rustman monitor` with alert rules, desktop/webhook/email notifiers, a local result store and a TUI view), secret providers (#98, `{{secret:...}}` from the keychain, env, SOPS, 1Password, Vault, a command, and AWS/GCP/Azure behind cargo features), the plugin system (#101, WASM component plugins on wasmtime with a frozen WIT API, a capability sandbox, sha256 pins, workspace trust and seven extension points), the traffic recorder (#105, `rustman record` forward/reverse proxy with a per-workspace local CA, scoped HTTPS interception, HAR export and saved request files) and the messaging client (#106, Kafka, MQTT and AMQP request files, AsyncAPI import, CLI, TUI pane and agent tools). Flaky stress tests were fixed in #100 and #103. Follow-ups: plugin sandbox hardening (#102: Windows reparse points, a listing race, `plugin verify` and private literal IPs, URL userinfo in history); recorder secret detection in URL path segments and nested JSON fields (#104); the viz companion has no `monitor_trend` view yet, so `monitor status --viz` falls back to text.
- **Done:** the **EVM toolkit** side track (`add-evm-toolkit`), in three PRs. #107, read-only: `evm-call` and `evm-tx` request files with `chain_id` pinning, ABI decoding (built-in ERC-20/721/1155, Foundry and Hardhat artifacts, opt-in explorer fetch), `eth_call` simulation with state overrides and call traces, head and log streams, ENS, Anvil forks, `rustman evm call|decode|logs|tx|block|ens|fork`, a TUI pane and read-only agent tools. #110: chain exploration (`rustman evm probe`: method matrix, archive detection, latency, subscriptions, provider consistency and rate-limit discovery, as a `rustman.report/v1` exploration report) and blockchain GraphQL presets for geth EIP-1767 and The Graph subgraphs (scalar display, `--paginate`, subgraph freshness and a `max_lag_blocks` assertion). #114, the signing gate: `rustman evm send --sign` and `rustman run --sign` with keystore and OS-keychain signers, a workspace opt-in, raw keys refused, simulation, typed confirmation, receipt wait and an audit record; agents can't sign (`rustman-mcp` doesn't link the signing crate). Flaky tests were fixed in #109 and #113. Follow-ups: hardware-wallet signers, Ledger and Trezor (#111); signing from the TUI (#112). The OpenSpec change is archived with v1.0.
- **In progress:** **v1.0 — Stabilize**. Done: every OpenSpec change is archived and `openspec/specs/` holds 35 living specs (open tasks moved to #35 and #116–#123); the public contracts are frozen and documented in [`docs/stability.md`](docs/stability.md), each with a guard test in CI; the docs site (mdBook on GitHub Pages, <https://cyberdynecorp.github.io/rustman/>) and the migration guide from Postman (#125). Next: the release pipeline (signed artifacts per OS, #35) and the manual terminal matrix (#116). Open work is tracked in the [roadmap tracking issue](https://github.com/CyberdyneCorp/rustman/issues/46).
- **CI:** fmt, clippy, tests on Linux/macOS/Windows, `cargo deny`, and `openspec validate --all --strict`. v0.7 added jobs against real services: `messaging-kafka` (Redpanda) and `messaging-mqtt-amqp` (Mosquitto, RabbitMQ), `secrets-vault` (Vault dev server), `secrets-localstack` (AWS), `release-secrets` (every provider type in the release binary), and `plugin-templates` (scaffolds, builds and tests every plugin template). Default debug and release builds are checked for the absence of the recorder's test hooks. The EVM toolkit runs in the `jsonrpc-anvil` job with `RUSTMAN_REQUIRE_ANVIL=1`, so a missing Anvil fails instead of skipping: the `rustman-evm` Anvil tests (with solc 0.8.26 for the fixture token), and the signing tests of `rustman-evm-signing` and `rustman evm send`, plus the "CI on Anvil" example of `docs/evm-signing.md`.

The order in `openspec/ROADMAP.md` follows the order the specs were written in. This roadmap reorders the work by
dependency, user value and risk. The main changes:

1. **Extract shared enablers first.** Nine changes depend on parts of other changes that the dependency table
   doesn't list (see [Dependency corrections](#dependency-corrections)). The biggest is the safety policy:
   `api-exploration` owns it, but load testing, monitors, plugins, the agent interface, EVM, messaging and stress
   testing all need it.
2. **Ship adoption features early.** Importers and the data-run/auth parts of `add-runner-parity` depend only on the
   core. They are what gets a Postman user to switch, so they move from M6 to the second release.
3. **Keep high-risk, optional work off the critical path.** The Raven companion (pre-1.0 C library, Vulkan) and the
   EVM toolkit (49 requirements, a niche audience) run as side tracks and never block a release.

## Phase 0 — Before writing code (≈1 week)

Settle these first. Each one is cheap now and expensive after code exists.

| # | Decision / fix | Source | Recommendation | Status |
|---|----------------|--------|----------------|--------|
| 0.1 | Does `ring`'s build-time C compiler satisfy the "no native toolchain" rule? | bootstrap design, Open Questions | Accept it. `rustls-rustcrypto` is still experimental. Write the decision into `openspec/config.yaml`. | ✅ done |
| 0.2 | License | bootstrap design | `MIT OR Apache-2.0`. Raven (Apache-2.0) is compatible. | ✅ done |
| 0.3 | Pin the MSRV and the `rmcp` minor version | bootstrap 1.1, mcp-client D1, agent Open Questions | MSRV 1.97 (needed by `wasmtime`); `rmcp ~3.10`, shared by changes 4 and 6. | ✅ done |
| 0.4 | Spike `serde-saphyr`: round-trip, error positions, deterministic emitter | bootstrap D4 | Done: go, with strict scalar options and two-pass loading (no `flatten`/tagged enums). Results in bootstrap D4. | ✅ done |
| 0.5 | Fix: `add-graphql-client` design D4/D5 build SSE and multipart uploads on "reqwest" | graphql design | `rustman-http` is hyper-based, and reqwest was rejected in bootstrap D2. Rewrite both on `rustman-http` byte streams. | ✅ done |
| 0.6 | Fix: report versioning contradiction | bootstrap 6.3 vs explorer design and task 4.2 | Bootstrap already publishes `rustman.report/v1 (kind: run)`. Change the explorer to say it *extends* v1 instead of introducing it. | ✅ done |
| 0.7 | Fix: `openspec/ROADMAP.md` dependency table | see below | Update the "Depends on" column. | ✅ done |
| 0.8 | Pin the OpenSpec CLI in CI | `.github/workflows/openspec-validate.yml` uses `@latest` | Pin an exact version. | ✅ done |

## Shared enablers

Build each of these once, in the release listed, even when its "owner" change comes later.

| Enabler | Spec owner | Needed by | Land in |
|---------|-----------|-----------|---------|
| `SafetyPolicy` + `GovernedSender` in `rustman-core` | `api-exploration` (5, tasks 2.1–2.3) | 6, 10, 12, 16, 18, 22, 23, 26 | **v0.1** as a standalone slice. Agent mode and benches can't exist without it. |
| Request-kind registry, assertion registry, `RequestPane` trait | `workspace`, `assertions`, `tui` | every protocol change | v0.1 / v0.2 |
| `rustman.report/v1` schema (`schemars`, checked-in JSON Schema) | `test-reports` | every reporter | v0.1 (`kind: run`), extended later |
| Shared `TlsSettings` → `rustls::ClientConfig` builder | `http-requests` | 3, 9, 14, 15 | v0.1 |
| One WebSocket stack (`tokio-tungstenite` + rustls + proxy CONNECT) | first protocol to need it (3) | 3, 9, 15 | v0.4 |
| Data-row reader (CSV/JSON) | `cli-runner` (25) | 18 | v0.2 |
| Snippet engine | `http-requests` (25) | `tui` "Copy as…", 21 | v0.2 |
| `rustman-viz-protocol` crate (serde types only) | `visual-dashboards` (8) | TUI `v` key (2), 18, 22, 26 | v0.2: pull the protocol crate forward. The companion itself comes later. |
| Workspace trust (`.rustman/trust.yaml`) | `mcp-client` (4) | 23 | v0.4 |
| Exploration runner (scheduling, budgets, findings, journal) | `api-exploration` (5) | 10 probe, 12 stateful, 26 | v0.3 |
| Replay header hygiene `sanitize_for_replay` | `webhook-inspector` (7) | 12 proxy, 17 | v0.4 |
| Neutral import IR + secret extraction | `collection-import` (11) | 12 `--record`, 25 script migration | v0.2 |

## Releases

Size is relative, based on task and requirement counts: **S** < 20 tasks, **M** 20–30, **L** 30+, **XL** = bootstrap.
There are no dates yet; they depend on team capacity.

### v0.1 — "Headless core" (critical path)

A CLI tool that is already useful in CI: a workspace in Git, environments and secrets, an HTTP engine with exact
timings, assertions, reporters and history.

| Change / slice | Size | Notes |
|----------------|------|-------|
| `bootstrap-core-engine` | XL (48 tasks, 59 req) | Split into four PRs: (a) scaffold + CI + `cargo deny`; (b) workspace + environments + secrets + redaction; (c) HTTP engine; (d) assertions + runner + history. |
| Safety-policy slice of `add-openapi-explorer` (tasks 2.1–2.3) | S | Pulled forward. It has no OpenAPI dependency. |

**Exit criteria:** bootstrap 8.1 (offline e2e on Linux, macOS and Windows) and 8.2 (canary-secret audit finds zero
matches) pass; `cargo build` works on a clean machine with no system libraries; the request-file `version: 1` and
the report `v1 (kind: run)` schemas are published. Archive `bootstrap-core-engine`.

### v0.2 — "Usable" (M1, plus what makes people switch)

| Change | Size | Notes |
|--------|------|-------|
| `add-terminal-ui` | L (37) | Ship task 8.1 (`v` viz hand-off) against the stub companion only. |
| `add-importers` | S (17) | Moved up from M6. This is how Postman, Insomnia and Bruno users get their collections in. |
| `add-runner-parity`: data-driven runs, Digest/SigV4/JWT auth, snippets, "Copy as…" | M | Only depends on 1 and 2. |
| `add-runner-parity`: script migration | S | Lands right after `add-importers`. |

**Exit criteria:** the TUI pty smoke test passes on all three OSes (TUI 9.1); a real Postman collection imports with
no secret written to disk; `rustman run --data` works. **This is the first public release.**

### v0.3 — "Autonomous" (M3) — the differentiator

| Change | Size | Notes |
|--------|------|-------|
| `add-openapi-explorer` (rest) | L (34, 44 req) | Import + sync, explorer, the full report renderers (HTML, Markdown, SARIF), `report diff`. |
| `add-agent-mcp-server` | M (23) | Agent tools for later changes plug in as those changes land. |

**Exit criteria:** explorer 5.1 (`bad-api` end to end in under 5 minutes) and agent 5.1 (agent scenario on all
OSes) pass. A demo shows `claude mcp add rustman -- rustman mcp serve` working read-only by default.

### v0.4 — "Multi-protocol + test confidence" (M2, plus part of M6)

| Change | Size | Notes |
|--------|------|-------|
| `add-graphql-client` | M (26) | Introduces the shared WebSocket stack. |
| `add-mcp-client` | M (25) | Shares `rustman-mcp` and the `rmcp` pin with the agent server. |
| `add-webhook-inspector` | S (15) | |
| `add-snapshot-testing` | S (17) | Its e2e task 7.1 needs kinds 3, 4 and 9. Ship it with http and graphql, and add mcp/jsonrpc coverage as those land. |
| `add-openapi-mock-server` | M (25) | Needs 5 (model, synthesizer, graph), 7 (header hygiene) and 11 (record IR). |

These five changes are independent of each other and can run in parallel.

### v0.5 — "Protocols" (M7, without messaging)

| Change | Size |
|--------|------|
| `add-jsonrpc-client` | M (20) |
| `add-grpc-client` | M (23) |
| `add-realtime-client` (WebSocket, SSE) | S (18) |

`add-messaging-client` waits until v0.7. Kafka, MQTT and AMQP need broker containers in CI and serve a smaller audience.

### v0.6 — "Pro testing" (M8, first half)

| Change | Size | Notes |
|--------|------|-------|
| `add-contract-testing` | M (22) | High value in PR pipelines. |
| `add-trace-propagation` | S (17) | |
| `add-load-testing` | M (24) | Live progress is a terminal dashboard; the companion has no bench view yet (task 6.3). |
| `add-docs-generator` | S (18) | Uses the snippet engine from v0.2. |
| `add-stress-testing` | L (33) | Comes last in this release, because it builds on load testing. Its wire-fault writer is the one place raw HTTP is allowed, so review it carefully. |

### v0.7 — "Operate & extend" (M8, second half)

| Change | Size | Notes |
|--------|------|-------|
| `add-secret-providers` | M (21) | Cloud providers sit behind cargo features. |
| `add-scheduled-monitors` | S (19) | |
| `add-plugin-system` | M (25) | Freezes a WIT plugin API, so it should come after the core APIs have had real use. |
| `add-traffic-recorder` | M (25) | Local CA and keychain. Security-sensitive, so it needs a dedicated review. |
| `add-messaging-client` | M (21) | |

### v1.0 — Stabilize

- ✅ Freeze public contracts: request-file `version: 1`, `rustman.report/v1`, `rustman-viz/1`, the plugin WIT API and
  the CLI exit codes ([`docs/stability.md`](docs/stability.md), with frozen-surface tests in CI).
- ✅ Make sure every change is archived and `openspec/specs/` holds the living specs.
- Release pipeline: signed artifacts per OS, with `hw-wallet` and cloud secret providers enabled in official builds.
- Full manual terminal matrix (TUI 9.2, #116).
- ✅ A docs site (mdBook from `docs/`, deployed to GitHub Pages) and a migration guide from Postman
  (`docs/migrate-from-postman.md`, #125).

### Side tracks (never on the critical path)

| Change | Can start after | Why it's separate |
|--------|-----------------|-------------------|
| `add-raven-visualizer` | v0.3 (exploration reports exist) | Raven is pre-1.0, a C library, and needs Vulkan. It is an optional binary built in its own CI job. The protocol crate already shipped in v0.2. |
| `add-evm-toolkit` | v0.5 (`add-jsonrpc-client`) and v0.3 (explorer runner, agent) | XL (49 req, 37 tasks) with a niche audience. The signing gate needs its own security review. Split it into: read-only `evm-toolkit` → `chain-exploration` + `blockchain-graphql` → `evm-signing`. ✅ Done in #107, #110 and #114. |

## Critical path

```
Phase 0 ─▶ bootstrap (+ safety slice) ─▶ terminal-ui ─▶ openapi-explorer ─▶ agent-mcp-server ─▶ v0.3
                     │                        │
                     ├─▶ importers ─▶ runner-parity (script migration)
                     └─▶ runner-parity (data, auth, snippets)   [parallel with the TUI]
```

After v0.3 the work spreads out: v0.4, v0.5 and the side tracks are mostly independent and can be staffed in parallel.

## Dependency corrections

These are missing from `openspec/ROADMAP.md`'s "Depends on" column, found by reading the designs and tasks.
Fold them in during Phase 0 (item 0.7).

| Change | Also depends on | Evidence |
|--------|-----------------|----------|
| 2 `add-terminal-ui` | 8 (protocol types only) | TUI design D10 and task 8.1 use `rustman-viz-protocol` |
| 14, 15, 16 | 6 for their agent tools | Each registers conditional `agent-mcp-interface` tools. (The GraphQL and webhook tools are defined in 6 itself.) |
| 9 `add-jsonrpc-client` | 6 | Task 7.2 adds the `rpc_call` agent tool |
| 10 `add-evm-toolkit` | 5, 6, 8 (hard) | Design D10 reuses the explorer runner; tasks 4.4 and 6.4 |
| 12 `add-openapi-mock-server` | 7, 11 | Design D6: replay header hygiene, and the HAR IR for `--record` |
| 13 `add-snapshot-testing` | 3, 4, 9 (for e2e coverage) | Task 7.1 |
| 18 `add-load-testing` | 5 (safety policy, reports), 25 (data-row reader) | Proposal "Safety"; cross-cutting contracts table |
| 19 `add-trace-propagation` | 5 (report `trace` field) | Cross-cutting contracts table |
| 22 `add-scheduled-monitors` | 5 (safety policy) | Proposal: "they go through the safety policy" |
| 23 `add-plugin-system` | 4 (workspace trust), 5 (safety policy for network grants) | Proposal and cross-cutting contracts |

## Risks to watch

| Risk | Affects | Mitigation already in the specs | Roadmap action |
|------|---------|---------------------------------|----------------|
| Request-file format frozen at `version: 1` too early | everything | Additive fields only | Don't call it frozen until v0.5. By then, seven request kinds will have tested it. |
| `hyper`-based transport means owning redirects, cookies and proxies | v0.1 | In-process test server | Budget extra time for bootstrap slice (c). |
| MCP spec revisions move fast | 4, 6 | Shared `rmcp` pin, reference-server tests | Re-pin `rmcp` at each release. |
| False positives in the explorer erode trust | 5 | Conservative severities, per-rule disable | Dogfood on 2–3 real APIs before v0.3. |
| Raven API churn | 8 | Exact pin, a single adapter module | Kept as a side track. |
| Security-sensitive surfaces (signing, local CA, plugins, wire faults, agent mode) | 6, 10, 17, 23, 26 | Gates exist in the specs | Run a `/security-review` pass before each of these is archived. |
| Scope: ~645 tasks | all | — | Every release above is shippable on its own. Cut from the bottom of each release, never from v0.1–v0.3. |

## How to use this file

- Pick the next change from the current release, then run `openspec-apply-change <id>`.
- When a change is implemented, archive it (`openspec archive <id>`) and tick it here.
- Any change that adds or reorders a release updates this file in the same PR.

| Release | Changes | Status |
|---------|---------|--------|
| Phase 0 | decisions 0.1–0.8 | ✅ |
| v0.1 | bootstrap-core-engine, safety slice | ✅ |
| v0.2 | add-terminal-ui, add-importers, add-runner-parity | ✅ |
| v0.3 | add-openapi-explorer, add-agent-mcp-server | ✅ |
| v0.4 | add-graphql-client, add-mcp-client, add-webhook-inspector, add-snapshot-testing, add-openapi-mock-server | ✅ |
| v0.5 | add-jsonrpc-client, add-grpc-client, add-realtime-client | ✅ |
| v0.6 | add-contract-testing, add-trace-propagation, add-load-testing, add-docs-generator, add-stress-testing | ✅ |
| v0.7 | add-secret-providers, add-scheduled-monitors, add-plugin-system, add-traffic-recorder, add-messaging-client | ✅ |
| Side | add-raven-visualizer, add-evm-toolkit | ✅ |
| v1.0 | stabilize | ☐ |
