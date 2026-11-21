# CLI reference

`rustman` is the headless face of Rustman, for scripts and CI. Reports and response bodies go to stdout; warnings and errors go to stderr, prefixed with `warning:` and `error:`.

This page covers sending and running requests, reporters, terminal behavior, logging, completions and the exit codes. The other commands are described with the features they belong to.

## Commands

| Command | What it does | Described in |
|---------|--------------|--------------|
| `rustman` / `rustman tui [path]` | Open the terminal UI (`rustman` alone does so when stdin and stdout are a terminal, and prints help otherwise); `--theme`, `--ascii`, `--vim` | [Terminal UI](tui.md) |
| `rustman init [dir] [--bare]` | Create a workspace: `rustman.toml`, a `local` environment and a sample request | [Request files](request-files.md#workspace-layout) |
| `rustman env list` | List the environments and mark the active one | [Environments](environments.md#the-active-environment) |
| `rustman secret set <name> --env <env>` | Store a secret value (read from stdin or a no-echo prompt) | [Secrets](environments.md#declaring-and-setting-a-secret) |
| `rustman secret list --env <env>` | List the declared secrets and whether each is set | [Secrets](environments.md#declaring-and-setting-a-secret) |
| `rustman secret rm <name> --env <env>` | Delete a stored secret value | [Secrets](environments.md#declaring-and-setting-a-secret) |
| `rustman secret get <ref>` | Fetch a secret reference (`<provider>/<path>[#field]`) from its provider and print `•••••••• (<n> chars, <provider>)`; `--reveal` prints the value, on a terminal only (exit 2 otherwise); exits 3 when the secret cannot be resolved, whatever the cause (an unknown provider or a provider type not in this build included), as `send` does; `--workspace` | [Secret providers](secrets.md#the-secret-commands) |
| `rustman secret test [<provider>]` | Check that secret providers (one, or every configured one) can authenticate and be reached; exits 0 when every check passes and 3 otherwise (an unknown provider name included) | [Secret providers](secrets.md#the-secret-commands) |
| `rustman secret list-providers` | List the configured secret providers: name, type, auth, origin (workspace or user) and whether the type is in this build, never values; `--types` lists the provider types this build supports | [Secret providers](secrets.md#the-secret-commands) |
| `rustman cookies clear` | Empty the cookie jar of an environment | [Cookies](http.md#cookies) |
| `rustman auth login <request>` | Run the OAuth2 authorization-code login for a request and cache its token | [OAuth2](http.md#oauth2) |
| `rustman import curl` | Create a request file from a curl command | [Curl import](http.md#import) |
| `rustman import <src>` / `rustman import auto <src>` | Import a collection export, detecting its format from the content | [Importing collections](import.md) |
| `rustman import postman <src>` | Import a Postman Collection v2.0 or v2.1 export | [Postman](import.md#postman-collections) |
| `rustman import postman-env <src>` | Import a Postman environment export into `environments/` | [Postman environments](import.md#postman-environments) |
| `rustman import insomnia <src>` | Import an Insomnia v4 (JSON) or v5 (YAML) export | [Insomnia](import.md#insomnia-exports) |
| `rustman import bruno <src>` | Import a Bruno collection folder or a single `.bru` file | [Bruno](import.md#bruno-collections) |
| `rustman import har <src>` | Import the requests of a HAR 1.2 capture, with `--host`, `--method`, `--content-type` and `--include-static` | [HAR](import.md#har-captures) |
| `rustman import http <src>` | Import `.http` / `.rest` files (a file or a folder) and their environment files | [HTTP files](import.md#http-files) |
| `rustman import <format> <src>` | Import a source with an [importer plugin](plugins.md#importers) that registers `<format>`: the host checks the request files and environments it returns and writes them (nothing when one is invalid); `--into`, `--dry-run`, `--on-conflict`, `--trust-plugins` | [Importers](plugins.md#importers) |
| `rustman import openapi <file\|url>` | Create a collection, environments and auth config from an OpenAPI 3.0 or 3.1 document; `--into`, `--allow-remote-refs`; `--sync` (with `--dry-run`, `--json`) updates an imported collection | [OpenAPI import](import-openapi.md) |
| `rustman import asyncapi <file\|url>` | Create Kafka, MQTT and AMQP request files (a folder per channel) and an environment per server from an AsyncAPI 2.6 or 3.0 document; `--into`, `--allow-remote-refs`, `--workspace`; other versions exit 2 | [Messaging](messaging.md#asyncapi-import) |
| `rustman explore plan --spec <file\|url> --target <url>` | Discover the resource graph and plan exploration probes without sending anything; `--probes`, `--operations`, `--seed`, `--fuzz-cases`, the safety flags, `--graph`, `--json` | [Exploration](exploration.md#planning-rustman-explore-plan) |
| `rustman explore run --spec <file\|url> --target <url>` | Discover, plan, send the probes under the safety policy and write the report; the plan flags plus `--rate`, `--concurrency`, `--max-probes`, `--max-duration`, `--fail-on`, `--report <format>[=<path>]` | [Exploration](exploration.md#running-rustman-explore-run) |
| `rustman explore resume <id>` | Continue an interrupted exploration with the probes not yet run | [Exploration](exploration.md#interrupting-and-resuming) |
| `rustman explore status [<id>]` | Show the phase, progress and findings by severity of an exploration (the latest by default); `--json` | [Exploration](exploration.md#status) |
| `rustman explore export <id> --findings <ids\|all> --into <dir>` | Write findings as regression request files for `rustman run` | [Exploration](exploration.md#reproducing-findings) |
| `rustman report schema` | Print the JSON Schema of `rustman.report/v1` | [Reports](reports.md#the-canonical-report) |
| `rustman report render <report.json> --format <format>` | Render a run or exploration report as `json`, `junit`, `markdown`, `html` or `sarif`, or with a [report plugin](plugins.md#report-renderers) (`<plugin>/<format>`, with `-o`); `-o <file>`, `--fail-on`, `--trust-plugins` | [Reports](reports.md#rendering) |
| `rustman report diff <old.json> <new.json>` | List new, fixed and unchanged findings, coverage deltas and changed assertions; `--fail-on`, `--format markdown\|json` | [Reports](reports.md#comparing-reports) |
| `rustman export curl <request>` | Print a request as a curl command (redacted; secret provider references print as `«redacted:<provider>/<path>»` without being fetched) | [Curl export](http.md#export) |
| `rustman export postman <path>` | Write a collection folder or request file as a Postman Collection v2.1 (`-o <file>`) | [Exporting](import.md#exporting) |
| `rustman export http <path>` | Write a collection folder or request file as a `.http` file (`-o <file>`) | [Exporting](import.md#exporting) |
| `rustman export snippet --lang <lang> <request>` | Print a request as code for one of 11 languages and tools (redacted; secret provider references are never fetched); `--list` lists them | [Code snippets](snippets.md) |
| `rustman send` | Send one request and print the response | [below](#rustman-send) |
| `rustman run` | Run the requests under files and folders and report the results | [below](#rustman-run) |
| `rustman bench [path]... --target <url>` | Load test request files and collections as virtual-user iterations under the safety policy pinned to `--target`; `--executor` (`constant-vus`, `ramping-vus`, `constant-arrival-rate`), `--vus`, `--duration`, `--iterations`, `--think-time`, `--stage` (or `rustman.toml [[bench.stages]]`), `--rate`, `--max-vus`, `--data`, `--data-mode`, `--data-exhaust`, `--secret-columns`, `--allow-mutations`, `--allow-destructive`, `--production`, `--timeout`, `--insecure`/`-k`, `--env`, `--var`, `--threshold`, `--report json=<file>`/`html=<file>`, `--viz`, `--trust-plugins`; shows a live dashboard on a terminal (a progress line every 10 s otherwise); capped by `rustman.toml [bench]`; exits 1 when a threshold fails, 2 on a usage error or a cap, 4 when the policy refuses, 130 on Ctrl+C | [Load testing](bench.md) |
| `rustman stress <mode> [path]... --target <url>` | Find where a target listed in `rustman.toml [stress] targets` breaks, how, and whether it recovers: `breakpoint` (`--start-rps`, `--step-factor`, `--step-duration`, `--refine`, `--require-rps`), `spike` (`--baseline-rps`, `--baseline`, `--spike-factor`, `--spike-duration`), `soak` (`--rps`, `--duration`, `--drift-threshold`) or `fuzz`; `--slo` (or `rustman.toml [stress.slo]`), `--treat-throttling-as-failure`, `--max-vus`, `--fail-on`, `--allow-mutations`, `--allow-destructive`, `--production`, `--timeout`, `--insecure`/`-k`, `--env`, `--var`; fuzz mixing with `--fuzz-ratio`, `--malformed`, `--spec`, `--seed` and the fuzz phase's `--baseline`, and wire faults with `--wire` and `--allow-wire-faults`, all checked by the stress gate first; the health request (`--health`, default the first GET) feeds the kill switch and the recovery check (`--recovery-timeout`, default 2m); a dashboard on a terminal, else one line per step; `--report <format>[=<path>]` and `--viz`; the report is kept as `.rustman/stress/<id>/report.json`; capped by `rustman.toml [stress]`; exits 1 on a finding at or above `--fail-on` or a `max_sustainable_rps` below `--require-rps`, 2 on a usage error or a cap, 4 when the safety policy or the stress gate refuses, 130 on Ctrl+C | [Stress testing](stress.md) |
| `rustman stress export <id> --findings <ids\|all> --into <dir>` | Write the findings of a stress run as regression request files (the minimized input and a `status: "4xx"` assertion), wire faults as `.wire.txt` comment files | [Stress testing](stress.md#regression-export) |
| `rustman monitor <command>` | Scheduled monitors: monitor files in `monitors/*.rman-monitor.yaml`, results in `.rustman/monitors/` | [Monitors](monitors.md) |
| `rustman monitor run <name>` / `rustman monitor run --all` | Run monitors once on the run engine under the read-only monitor policy, store each result, compact old results when due, and evaluate and send alerts; exits 0 (pass), 1 (an assertion failed), 2 (an invalid monitor file, a missing target or environment), 3 (a request errored or the run passed its `timeout`), 4 (a request was blocked), 130 on Ctrl+C | [Monitors](monitors.md#one-shot-runs) |
| `rustman monitor list` | List the monitors with their schedule, environment and targets; `--json` | [Monitors](monitors.md#monitor-files) |
| `rustman monitor daemon` | Run every monitor of the workspace on its schedule until Ctrl+C or SIGTERM (10 s drain): overlapping runs skipped, monitor files reloaded, one daemon per workspace (exit 2 for a second one), no listening port, one log line per run on stderr | [Monitors](monitors.md#the-daemon) |
| `rustman monitor status` | Each monitor's last result, uptime over 24 h, 7 d and 30 d, p95 latency with a sparkline, coverage and firing rules, from the store only; `--json`; `--viz` also opens the trend view in the viz companion, or notes why not | [Monitors](monitors.md#status-and-history) |
| `rustman monitor history <name>` | The stored results of a monitor, newest first, compacted days as hourly aggregates; `--since`, `--outcome`, `--json` | [Monitors](monitors.md#status-and-history) |
| `rustman monitor test-alert <name>` | Send `[TEST] <name>` through every channel of a monitor and print `ok` or the error per channel; exits 0 when all succeed, 3 otherwise | [Monitors](monitors.md#testing-the-channels) |
| `rustman monitor schedule-recipe <name> --for <scheduler>` | Print a cron, systemd, launchd or GitHub Actions configuration that runs the monitor on its schedule; writes only with `--write <path>` | [Monitors](monitors.md#external-schedulers) |
| `rustman graphql introspect <endpoint\|request>` | Fetch a GraphQL schema by introspection and cache it in `.rustman/graphql/schemas/`; `--out <file>` also writes it as SDL | [GraphQL](graphql.md#rustman-graphql-introspect) |
| `rustman graphql query <request>` / `--endpoint <url> --query <text>` | Validate, send and print a GraphQL operation or subscription; `--no-validate`, `--json`, `--max-events`, `--duration` | [GraphQL](graphql.md#rustman-graphql-query) |
| `rustman rpc call <endpoint\|request> [method] [params-json]` | Send one JSON-RPC call (or a `kind: jsonrpc` request) and print its result or error object; `--header`, `--id`, `--notify`, `--subscribe` / `--unsubscribe`, `--raw`, `--json`, `--no-validate` | [JSON-RPC](jsonrpc.md#rustman-rpc-call) |
| `rustman rpc batch <endpoint> <file\|->` | Send calls from a JSON array, a YAML list or JSON lines as batches of 100 and print the results in input order | [JSON-RPC](jsonrpc.md#rustman-rpc-batch) |
| `rustman rpc discover <endpoint>` | Fetch an OpenRPC document with `rpc.discover` and cache it in `.rustman/cache/openrpc/` | [JSON-RPC](jsonrpc.md#openrpc-catalogs) |
| `rustman grpc list <target\|request>` | List the services and methods of a gRPC target, by reflection through `.rustman/grpc/descriptors/` or from the request's `schema`; `--refresh`, `--json` | [gRPC](grpc.md#rustman-grpc-list) |
| `rustman grpc describe <target\|request> <symbol>` | Describe a service, a method (call type, message types and a request template) or a message; `--json` | [gRPC](grpc.md#rustman-grpc-describe) |
| `rustman grpc call <request>` / `--target <t> --method <m> --data <json>` | Send one gRPC call and print the response messages and status; `--metadata`, `--deadline-ms`, `--json` (one line per message, then a status line) | [gRPC](grpc.md#rustman-grpc-call) |
| `rustman grpc health <target>` | Call `grpc.health.v1.Health/Check` and print the serving status; `--service`, `--timeout`, `--json` | [gRPC](grpc.md#rustman-grpc-health) |
| `rustman ws connect <url\|request>` | Open a WebSocket session (a `ws://` / `wss://` URL or a `kind: ws` request), send the lines of a piped stdin as text frames and print the received messages; `--json`, `--max-messages`, `--duration`, `--header`, `--subprotocol`, `--trace` | [WebSocket and SSE](realtime.md#rustman-ws-connect-and-rustman-sse-listen) |
| `rustman sse listen <url\|request>` | Listen to a Server-Sent Events stream (an `http(s)://` URL or a `kind: sse` request) and print its events; `--json`, `--max-messages`, `--duration`, `--header`, `--trace` | [WebSocket and SSE](realtime.md#rustman-ws-connect-and-rustman-sse-listen) |
| `rustman kafka produce [request]` | Produce messages (a `kind: kafka` request, or `--brokers`, `--topic`, `--partition`, `--key`, `--header` and `--value`, or one message per line of a piped stdin) and print their partitions and offsets; `--json` | [Messaging](messaging.md#rustman-kafka-mqtt-amqp-produce-and-consume) |
| `rustman kafka consume [request]` | Read a topic from `--partition`s and an `--offset` (`earliest`, `latest`, a number or an RFC 3339 timestamp) without a consumer group, printing messages as they arrive; `--max-messages`, `--duration`, `--json` (one line per message) | [Messaging](messaging.md#rustman-kafka-mqtt-amqp-produce-and-consume) |
| `rustman mqtt produce [request]` | Publish messages (a `kind: mqtt` request, `--value` flags or stdin lines) with `--broker`, `--topic`, `--qos`, `--retain` | [Messaging](messaging.md#rustman-kafka-mqtt-amqp-produce-and-consume) |
| `rustman mqtt consume [request]` | Subscribe to a topic filter (`+`, `#`) with a clean session and print messages as they arrive; `--max-messages`, `--duration`, `--json` | [Messaging](messaging.md#rustman-kafka-mqtt-amqp-produce-and-consume) |
| `rustman amqp produce [request]` | Publish messages with publisher confirms to `--exchange` with `--routing-key` (`--mandatory` fails unroutable messages) | [Messaging](messaging.md#rustman-kafka-mqtt-amqp-produce-and-consume) |
| `rustman amqp consume [request]` | Consume through a temporary queue bound to `--exchange` with `--binding-key`, or from an existing `--queue` with `--destructive` or `--requeue`; `--max-messages`, `--duration`, `--json` | [Messaging](messaging.md#rustman-kafka-mqtt-amqp-produce-and-consume) |
| `rustman history list` | List past exchanges, newest first, with `--status`, `--method`, `--grep`, `--since`, `--limit` and `--json` | [History](history.md#listing-and-searching) |
| `rustman history show <id>` | Print one exchange in full (redacted) | [History](history.md#listing-and-searching) |
| `rustman history replay <id>` | Send an exchange's request again, with secrets read from the store now (a plugin auth scheme signs it again: `--trust-plugins` as for `send`) | [History](history.md#replay) |
| `rustman history clear` | Delete every recorded exchange | [History](history.md#retention) |
| `rustman trace open <id>` | Open the trace link of a history exchange (an id or a unique prefix) in the browser and print it; `--no-browser` only prints it; exits 2 without a link template | [Trace propagation](tracing.md#opening-a-trace) |
| `rustman mcp serve` | Serve the workspace to AI agents over MCP: stdio by default, or Streamable HTTP with `--http [<addr>]`, `--allow-remote` and `--token-file` | [Agents (MCP)](agent.md) |
| `rustman mcp-client tools <server>` | List an MCP server's tools; `<server>` is a workspace server, `stdio:<command line>` or an `http(s)://` URL; `--json`, `--trust`, `--timeout`, `--env` | [MCP client](mcp-client.md#rustman-mcp-client) |
| `rustman mcp-client call <server> <tool>` | Call a tool with `--args <json>` or `--arg k=v` / `--arg k:=<json>`; `--no-validate`, `--sampling-response <file>`; exits 1 on a tool or protocol error | [MCP client](mcp-client.md#rustman-mcp-client) |
| `rustman mcp-client resources <server>` | List an MCP server's resources, or read one with `--read <uri>` | [MCP client](mcp-client.md#rustman-mcp-client) |
| `rustman mcp-client prompts <server>` | List an MCP server's prompts, or get one with `--get <name> --arg k=v` | [MCP client](mcp-client.md#rustman-mcp-client) |
| `rustman mcp-client logout <server>` | Delete the OAuth tokens stored for an MCP server | [MCP client](mcp-client.md#oauth) |
| `rustman completions <shell>` | Print a shell completion script | [below](#completions) |
| `rustman snapshot list [path]` | List snapshot files with their request, name and state (`ok`, `orphaned`, `unreferenced name`); `--json` | [Snapshots](snapshots.md#listing-and-pruning) |
| `rustman snapshot prune [path]` | Delete snapshot files no request uses anymore; `--yes`, `--dry-run` | [Snapshots](snapshots.md#listing-and-pruning) |
| `rustman webhook listen` | Capture inbound webhooks on a local port, answer with mocks, check signatures and print the event log; `--host`, `--port`, `--allow-remote`, `--max-body`, `--max-captures`, `--jsonl`, `--env`, `--workspace` | [Webhook inspector](webhooks.md) |
| `rustman record` | Run the local recording proxy (default `127.0.0.1:8888`) and print one redacted line per capture until Ctrl+C; HTTPS is decrypted only for `--intercept` hosts; `--host`, `--port`, `--allow-remote`, `--workspace`, filters (`--include-host`, `--exclude-host`, `--method`, `--content-type`, `--keep-static`), `--json`, `--upstream` (reverse proxy, `--insecure`), `--save` (`--into`, `--env`, `--store-secrets`), `--har` (`--show-secrets`) | [Traffic recorder](record.md) |
| `rustman record ca init` | Create the workspace's local CA: the key in the OS keychain, the certificate in the user config directory; exits 2 without a keychain | [Traffic recorder](record.md#the-local-ca) |
| `rustman record ca export` | Print the CA certificate in PEM (never the key) and its SHA-256 fingerprint; `--out` | [Traffic recorder](record.md#the-local-ca) |
| `rustman record ca install` | Trust the CA in this machine's trust store after a typed `yes` (or `--yes`); declining exits 1 | [Traffic recorder](record.md#trusting-the-ca) |
| `rustman record ca uninstall` | Remove the CA from every trust store Rustman installed it in | [Traffic recorder](record.md#trusting-the-ca) |
| `rustman record ca rotate` | Uninstall and delete the CA, create a new one and print both fingerprints; `--yes` | [Traffic recorder](record.md#the-local-ca) |
| `rustman record ca remove` | Uninstall the CA, then delete its key from the keychain and its certificate, after a typed `yes` (or `--yes`); declining exits 1 | [Traffic recorder](record.md#the-local-ca) |
| `rustman viz <report.json>` | Open a run or exploration report in the graphical viz companion `rustman-viz`; `--view graph` opens the API graph of an exploration | [below](#viz-companion) |
| `rustman mock serve <openapi>` | Serve an OpenAPI document as a local mock server until Ctrl+C; `--host`, `--port`, `--allow-remote`, `--allow-remote-refs`, `--seed`, `--no-prefer`, `--no-validate`, `--enforce-security`, `--stateful`, `--env`, `--workspace`, `--cors`, `--no-cors`, `--jsonl`, `--history`, `--proxy-unmatched`, `--record` | [Mock server](mock.md) |
| `rustman contract diff <old> <new>` | Compare two OpenAPI 3.0/3.1 documents (files or URLs) and classify every difference as `breaking`, `non-breaking` or `unknown` with a rule id and JSON pointers into both documents; exits 1 on an unallowlisted breaking difference, 2 on an invalid document or allowlist; `--base <ref>` (old document from Git), `--fail-on-unknown`, `--fail-on`, `--allowlist`, `--report`, `--env`, `--workspace` | [Contract testing](contract.md), [Contract rules](contract-rules.md) |
| `rustman contract rules` | List the rule catalog with its version; `--json` prints a JSON array (`id`, `classification`, `description`, `example`, `catalog_version`) | [Contract rules](contract-rules.md) |
| `rustman contract pact generate [<collection>...]` | Write a Pact v4 (or `--pact-version 3`) file from a collection's http requests and assertions, to `contracts/pacts/<consumer>-<provider>.json` (or `--output`); secrets and dynamic values become `regex` matchers with `«redacted:...»` examples; assertions without a Pact equivalent warn; `--consumer`, `--provider`, `--env`, `--workspace` | [Contract testing](contract.md#generating-a-pact-file-consumer-side) |
| `rustman contract pact verify <pact-file>...` | Replay Pact interactions against `--provider-url` through the safety policy, with provider states POSTed to `--provider-states-url`; exits 1 on a mismatch, 2 for stateful interactions without a state URL, 4 when an interaction is blocked (`--allow-mutations`, `--allow-destructive`) or the provider is an unconfirmed production host (`--production <host>`); `--report`, `--env`, `--workspace` | [Contract testing](contract.md#verifying-a-provider) |
| `rustman contract verify --spec <openapi>` | Check recorded exchanges (history with `--since`, `--grep`, `--limit`, or `--from <run-report>`) against a local OpenAPI document (URLs are refused) without sending anything: violations, undocumented exchanges and the operations observed (`5/12 operations observed`); exits 1 on a violation (or an undocumented exchange with `--fail-on-undocumented`); `--report`, `--env`, `--workspace` | [Contract testing](contract.md#passive-conformance) |
| `rustman docs build` | Generate a static, self-contained HTML site (`--format html\|markdown\|both`, default `html`) from the workspace (`--workspace`) into `--out` (default `site/`): one section per collection, one page per request, client-side search, dark and light themes, deterministic output; redacted examples for opted-in requests (`--examples history\|snapshot`), published variables with `--env`; refuses a non-empty folder it did not generate unless `--force`; exits 1 when the leak scan finds a secret in an output file | [API docs](api-docs.md) |
| `rustman docs serve` | Preview the docs on `127.0.0.1` only (`--port`, default 4000; `--env`, `--examples`): builds into a temporary folder, rebuilds 200 ms after a workspace change and reloads open pages; build errors appear on stderr and in a page overlay without stopping it; exits 2 when the port is taken | [API docs](api-docs.md#preview) |
| `rustman plugin add <path\|url>` | Fetch a plugin from a folder or `.wasm` file inside the workspace or an `https` URL (TLS verified, 20 MiB, the workspace `ca_bundle` trusted), print its manifest and requested capabilities, pin its sha256 and declare it in `rustman.toml` with `grants = []`, update `rustman.lock`; `--name`, `--workspace`; exits 2 on `http://`, a manifest or component error, or a name already declared | [Plugins](plugins.md#managing-plugins) |
| `rustman plugin list` | List the declared plugins (name, version, API, source, grants) and whether this machine trusts their set; `--workspace` | [Plugins](plugins.md#managing-plugins) |
| `rustman plugin remove <name>` | Remove a plugin's table from `rustman.toml` and its `rustman.lock` entry; `--workspace` | [Plugins](plugins.md#managing-plugins) |
| `rustman plugin verify` | Re-hash every plugin against its pin (downloading URL sources not cached yet), check manifests and grants and bring `rustman.lock` in line; exits 0, or 2 on a mismatch; `--workspace` | [Plugins](plugins.md#managing-plugins) |
| `rustman plugin api` | Print the plugin API package version and the API majors this build supports | [Plugins](plugins.md#plugin-api-versions) |
| `rustman plugin test [path]` | Run a plugin's test cases (`rustman-plugin-tests.toml`, or `--cases <file>`) in the sandbox with a fake clock, randomness, environment, network and secrets; prints each case's output (redacted), exits 0 when every case passes, 1 on a failure, 2 when the plugin or cases cannot be read | [Plugins](plugins.md#testing-plugins) |
| `rustman plugin new <name> --kind <extension point>` | Scaffold a `no_std` Rust plugin project for `auth`, `assertion`, `generator`, `transformer`, `importer`, `kind` or `report`: manifest, sample, test cases and a `cargo build --target wasm32-wasip2` recipe; `--dir`, `--sdk <path>` (build against a local `rustman-plugin-sdk`) | [Plugins](plugins.md#writing-plugins) |

The command groups `rustman env`, `rustman secret`, `rustman cookies`, `rustman auth`, `rustman import`, `rustman export`, `rustman explore`, `rustman report`, `rustman graphql`, `rustman rpc`, `rustman grpc`, `rustman ws`, `rustman sse`, `rustman kafka`, `rustman mqtt`, `rustman amqp`, `rustman history`, `rustman mcp`, `rustman mcp-client`, `rustman mock`, `rustman snapshot`, `rustman trace`, `rustman webhook`, `rustman contract`, `rustman contract pact`, `rustman docs`, `rustman plugin` and `rustman record ca` only hold the commands above: on their own they print their subcommands and exit with 2. `rustman import` also takes a source file or folder directly (`rustman import export.json`), which is the same as `rustman import auto export.json`.

`rustman --help` and `rustman <command> --help` document every flag; `rustman --version` prints `rustman <version>`, such as `rustman 0.1.0`.

### Global flags

These work before or after the command name (`rustman -v run .` or `rustman run . -v`):

| Flag | Effect |
|------|--------|
| `--color <auto\|always\|never>` | `auto` (the default) colors stdout only when it is a terminal, `NO_COLOR` is unset or empty and `TERM` is not `dumb`. `always` forces color, even when piped; `never` turns it off. |
| `-v`, `-vv`, `-vvv` | Logs on stderr: `-v` one line per request, `-vv` also the redacted request and response headers, `-vvv` everything, including the HTTP and TLS libraries. |

## rustman send

```text
rustman send <REQUEST> [--env <name>] [--var k=v]... [--timeout <duration>] [--insecure] [--body-only | -o <file>] [--show-secrets]
             [-u | --update-snapshots] [--ci] [--trust-plugins]
rustman send <METHOD> <URL> [-H 'K: V']... [-d <body> | --json <body>] [same flags]
rustman send <URL>                                    # an ad-hoc GET
```

`<REQUEST>` is a request file (relative to the current directory), its id (the path from the workspace root without `.rman.yaml`, such as `users/get-user`) or its `name`. A name shared by several requests fails with exit code 2:

```text
error: request name 'List' is ambiguous: a/list.rman.yaml, b/list.rman.yaml
```

By default the status line, the headers and the body are printed:

```text
HTTP/1.1 200
content-type: application/json
x-request-id: req-1

{"id":42,"name":"Ana","email":"ana@example.com","roles":["admin","dev"],"address":{"city":"Lisbon"}}
```

Headers and bodies are shown [redacted](environments.md#redaction): `Authorization`, `Proxy-Authorization`, `Cookie` and `Set-Cookie`, every secret the request references and every captured value that became a secret. A large body is streamed from its spill file, and a binary body (one that is not valid UTF-8) is redacted byte by byte. `--show-secrets` prints the headers and body as received instead, on the terminal only: history still stores them redacted. `--body-only` prints only the response body, redacted the same way (as received with `--show-secrets`). `-o <file>` writes the decoded body to a file, always byte for byte; use it to save an image or other binary body exactly:

```sh
rustman send get-user --env staging --body-only | jq .id
rustman send logo --env staging -o logo.png
```

The request's assertions and captures run as in `rustman run`. Failed assertions are listed on stderr (`fail: status: expected 200, got 404`) and the exit code is 1; a request that gets no response prints `error: <reason>` and exits with 3. When the request carried [trace context](tracing.md), stderr also shows `trace: <trace id>`, `trace: backend <id>` when the server's `traceresponse` names another trace, and `trace: <link>` when a link template is configured. [Snapshot assertions](snapshots.md#recording-updating-and-ci) are recorded, compared and updated as in `rustman run`, with the same `--update-snapshots` (`-u`) and `--ci` flags and the same `CI` variable; a recorded or updated snapshot is noted on stderr (`snapshot default recorded: users/__snapshots__/get-user.default.snap.yaml`).

### Ad-hoc requests

`rustman send <METHOD> <URL>` sends a request that has no file. `-H` adds a header (repeatable), `-d` / `--data` sends a body as is and `--json` sends a JSON body with `Content-Type` and `Accept` set to `application/json`. The flags mean what they mean for curl, and the request is built the same way [`rustman import curl`](http.md) builds one.

No workspace is needed. Outside a workspace, the request is sent with the default settings, its cookies last for the command only and nothing is written to the current folder; `--env`, secrets and other environment features need a workspace:

```sh
rustman send GET https://httpbin.org/get
rustman send POST https://api.example.com/users -H 'X-Team: core' --json '{"name":"Ana"}'
```

Inside a workspace, an ad-hoc request uses the workspace settings and the active environment, so `{{base_url}}` and secrets resolve. A URL that starts with a variable is an ad-hoc URL too; `http://` is not added to it, the variable brings the scheme:

```sh
rustman send '{{base_url}}/users/42' --env dev
rustman send DELETE '{{base_url}}/users/42?key={{api_token}}' --env dev
```

## rustman run

```text
rustman run [<PATH>...] [--env <name>] [--var k=v]... [--timeout <duration>] [--insecure]
            [--bail[=iteration]] [--filter <tag:name | glob>]... [--iterations <n>] [--parallel <n>]
            [--data <file> | --no-data] [--data-label <column>] [--secret-columns <a,b>]
            [--reporter <format>[:<path>]]... [--report <format>[=<path>]]...
            [--max-events <n>] [--duration <duration>] [-u | --update-snapshots] [--ci]
            [--trust] [--trust-plugins] [--viz]
```

Runs every request file (`kind: http`, [`kind: graphql`](graphql.md#in-rustman-run) and [`kind: mcp`](mcp-client.md#request-files)) under the given files and folders (default: the current folder), in collection order: file-name order, or the folder's `_collection.yaml` `order:` (see [collections](request-files.md)). Paths must exist and lie inside the workspace. Values captured by one request are available to every later request of the run.

| Flag | Effect |
|------|--------|
| `--env <name>` | The active environment (default: `RUSTMAN_ENV`, then `default_environment`). |
| `--var k=v` | A variable above every other source (repeatable). |
| `--timeout <duration>` | The total timeout of every request, replacing `timeout_ms`: `500ms`, `5s`, `2m`, or a number of milliseconds. Also on `send`. |
| `--insecure`, `-k` | No TLS certificate verification; exchanges are flagged `insecure: true`. Also on `send`. |
| `--no-history` | Do not record the exchanges in [history](history.md). Also on `send`. |
| `--trace` | Send [trace context](tracing.md) headers (`traceparent`) even when no `trace:` block enables them. Also on `send`, `ws connect` and `sse listen`. |
| `--bail` | After the first failed or errored request, the requests not started yet are reported as `skipped (bail)`. `--bail=iteration` skips only the rest of the failing iteration and goes on with the next one. |
| `--filter <value>` | Keep only matching requests (repeatable; a request matching any filter runs). `tag:<name>` matches the request's `tags`; anything else is a glob over the request id or file path, where `*` and `?` stay within one folder and `**` crosses folders: `users/*`, `**/login`. |
| `--iterations <n>` | Run the selected requests `n` times (default 1). Captures carry over between iterations. Cannot be combined with `--data`. |
| `--data <file>` | Run the selected requests once per row of a CSV or JSON file, with the row's values as variables; see [data-driven runs](data-runs.md). `--no-data` ignores the data file a `_collection.yaml` names. |
| `--data-label <column>` | Label each iteration of a data-driven run with this column's value (default `row <n>`). |
| `--secret-columns <a,b>` | Data columns whose values are secrets, redacted in every output. |
| `--parallel <n>` | Run up to `n` folders at once (default 1). |
| `--reporter <format>[:<path>]` | `pretty`, `json`, `junit` or `tap`, written to stdout or to `<path>` (repeatable; default `pretty` on stdout). At most one reporter may write to stdout. A file is replaced atomically, a symlink is followed, and a device such as `/dev/stdout` is written in place. A [report plugin](plugins.md#report-renderers)'s `<plugin>/<format>:<path>` writes the bytes the plugin returns to `<path>`. |
| `--report <format>[=<path>]` | Also render the canonical report as `json`, `junit`, `markdown`, `html` or `sarif` (repeatable), to `<path>` or stdout; see [Reports](reports.md). With a `--report` on stdout and no `--reporter`, the `pretty` report is not printed. At most one reporter or report may write to stdout. |
| `--max-events <n>` | GraphQL and JSON-RPC subscriptions stop after `n` events, replacing each request's `max_events` (default 10). |
| `--duration <duration>` | GraphQL and JSON-RPC subscriptions stop after this long, replacing each request's `duration` or `max_duration_ms` (default `30s`). |
| `--update-snapshots`, `-u` | Overwrite [snapshots](snapshots.md) that differ from the response and report them `updated`; matching files are left untouched. |
| `--ci` | CI mode: never write snapshots, and fail a request whose snapshot is missing. Also on when the `CI` environment variable is `true` or `1`. Combined with `--update-snapshots` it is a usage error (exit code 2): `--update-snapshots cannot be used in CI mode`. |
| `--trust` | Spawn the workspace's [stdio MCP servers](mcp-client.md#workspace-trust) that are not trusted yet, and record the approvals; without it, `kind: mcp` requests to an untrusted server error. |
| `--trust-plugins` | Load the workspace [plugins](plugins.md#trust) even when this machine has not trusted their set yet, and record the approval. Without it, an untrusted set stops the run with exit code 2 before any request is sent. Also on `send`, `bench`, `history replay`, `import <format>` and `report render`. |
| `--viz` | Show the run live in the [viz companion](#viz-companion). Without one, the run continues without visuals and its exit code is unchanged. |

A filter or path that selects no request fails with `error: no requests to run (check the paths and --filter)` (exit code 2).

```sh
rustman run api/users --env staging
rustman run . --env ci --filter tag:smoke --bail
rustman run . --env ci --parallel 4 --reporter json:out/report.json --reporter pretty
```

### Parallel folders

With `--parallel <n>`, each folder is a unit of sequential work and up to `n` units run at once, so the requests of one folder still run in order and their captures chain. A folder whose `_collection.yaml` sets `parallel: true` runs its requests concurrently instead, still up to `n` at once; with the default `--parallel 1` it runs them in order like any other folder. Reports always list requests in collection order, whatever order they finished in.

```yaml
# load/_collection.yaml
parallel: true
```

### Reporters

`pretty` prints one line per request, the reasons below each request that did not pass, and a summary:

```text
PASS  users/list  GET 200  12 ms
FAIL  users/create  POST 404  8 ms
      status: expected 201, got 404
ERROR users/slow
      timeout after 500 ms (response)
SKIP  users/delete  skipped (bail)

4 requests: 1 passed, 1 failed, 1 errored, 1 skipped (35 ms)
```

`json` writes a `rustman.report/v1` document with `"kind": "run"`, described by the JSON Schema in [`schemas/report.schema.json`](schemas/report.schema.json). It holds every request (with its redacted exchange, timings, assertion results and capture results) and `summary.total`, `summary.passed`, `summary.failed`, `summary.errored`, `summary.skipped`, `summary.duration_ms` and `summary.interrupted`. A request is `passed` when every assertion passed and every required capture matched, `failed` when an assertion failed or could not be evaluated or a required capture matched nothing, `errored` when no response arrived (unresolved variables, network, TLS, timeout, auth) and `skipped` with a `skip_reason`: `bail`, `interrupted`, or `unsupported_kind` for request kinds this build cannot send. Report files are replaced atomically.

A data-driven or repeated run (`--data`, or `--iterations` above 1) groups the pretty report under one `iteration <i>/<n>` line per iteration, adds ` [<label>]` to JUnit test case names and TAP descriptions. Every JSON request has the 1-based integer `iteration`; with `--data` only, it also gets a `data_row` object (`index`, `total`, `label`); see [iteration labels](data-runs.md#iteration-labels). A `kind: jsonrpc` request also gets a `jsonrpc` object with its envelopes, attempts and subscription events; see [JSON-RPC in `rustman run`](jsonrpc.md#in-rustman-run). A `kind: grpc` request gets a `grpc` object with its exchange (`protocol: grpc`, status, metadata, messages, timeline, timings); see [gRPC in `rustman run`](grpc.md#in-rustman-run).

`junit` writes JUnit XML for CI systems: one `<testsuite name="rustman">` with one `<testcase>` per request (named by its id, with `classname` set to its file). A failed request gets a `<failure>`, an errored one an `<error>` and a skipped one `<skipped>`; their `message` is the first reason, and the element text lists them all:

```sh
rustman run . --env ci --reporter junit:out/junit.xml --reporter pretty
```

```xml
<testcase name="users/create" classname="users/create.rman.yaml" assertions="1" time="0.008">
    <failure message="status: expected 201, got 404">status: expected 201, got 404</failure>
</testcase>
```

`tap` writes a [TAP version 14](https://testanything.org/tap-version-14-specification.html) stream: one test point per request, `# SKIP <reason>` for skipped requests, and a YAML block with the reasons under each request that failed or errored. An interrupted run ends with `Bail out!`.

```text
TAP version 14
1..2
ok 1 - users/list
not ok 2 - users/create
  ---
  status: failed
  file: "users/create.rman.yaml"
  reasons:
    - "status: expected 201, got 404"
  ...
```

Every reporter writes from the same redacted records: secret values and sensitive headers appear as `«redacted:<name>»` in all of them.

Runs with [`snapshot` assertions](snapshots.md#in-reports) add a summary line such as `1 snapshot recorded, 1 snapshot failed` to `pretty`, a `snapshot` object to each snapshot assertion and `summary.snapshots` to `json`, the diff to the JUnit failure message, and `#` comments to `tap`.

## Viz companion

`rustman-viz` is an optional graphical companion that draws runs, explorations and large responses as charts, tables and graphs. It is a separate program: `rustman` builds, installs and runs without it, and never needs a GPU. `rustman` talks to it over the [viz protocol](viz-protocol.md). Its views and how to install it are in [`viz.md`](viz.md).

```text
rustman viz <REPORT> [--view dashboard|graph]
rustman run [<PATH>...] --viz
rustman explore run --spec <file|url> --target <url> --viz
rustman explore resume <id> --viz
```

- `rustman viz <REPORT>` opens a saved JSON report (from `rustman run --reporter json:<file>` or `rustman explore run`) in the run or exploration dashboard, chosen from the report's `kind`. `--view graph` opens the API graph of an exploration report instead. A file that is not a Rustman report exits with 2: `error: notes.txt is not a Rustman report`.
- `--viz` opens the matching dashboard and streams progress to it as requests or probes finish. The data is redacted before it is sent: secrets appear as `«redacted:name»`, and the companion has no way to reveal them.

`rustman` finds the companion in this order:

1. `RUSTMAN_VIZ`, the path of the executable. When it is set but does not name an executable file, `rustman` reports that path and looks nowhere else.
2. `rustman-viz` (`rustman-viz.exe` on Windows) on `PATH`.
3. `rustman-viz` next to the running `rustman` executable.

The first match is started once with `--protocol-check` to confirm that it speaks `rustman-viz/1`. Its stderr is appended to `.rustman/viz.log` in the workspace.

When the companion cannot be used, `--viz` prints the reason as a warning and the command carries on. Its exit code depends only on the run or exploration. `rustman viz` prints the same reasons as an error and exits with 2. Either way, the message suggests an HTML report instead.

| Condition | Message |
|-----------|---------|
| Not found | `Viz companion not found; continuing without visuals (...)` |
| Another protocol version | `Viz companion speaks rustman-viz/2, rustman needs rustman-viz/1; upgrade rustman-viz` |
| No graphics (no Vulkan driver on Linux or Windows; a VM or remote session) | `Graphics unavailable (Vulkan); continuing without visuals` |
| Exits or crashes during a run | `Viz companion exited; continuing without visuals` |
| Stops reading for 5 seconds | `Viz companion stopped reading for 5 s; detached from it, continuing without visuals` |

Sending to the companion never slows a run down. When it falls behind, progress updates are dropped, but findings and the final summary are always kept. After 5 seconds of back-pressure, `rustman` stops sending to it.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Every executed request passed. |
| 1 | At least one request failed (an assertion, or a required capture) and none errored; for `rustman bench`, a threshold failed; for `rustman stress`, a finding at or above `--fail-on`, or `--require-rps` not reached. |
| 2 | Usage or configuration error: a bad flag, no workspace, an invalid file, a missing environment, an ambiguous request name, no request to run. |
| 3 | At least one request errored: network, TLS, timeout, auth or unresolved variable. It wins over 1. |
| 4 | Refused by the [safety policy](safety.md#production-guard) (`rustman explore`, `rustman bench`, `rustman stress`), the [stress gate](stress.md#safety) or the [monitor policy](monitors.md#read-only-by-default). |
| 130 | Interrupted by Ctrl+C (SIGINT), or on Unix by SIGTERM or SIGHUP, handled the same way. |

`rustman import` has its own: 0 when the import completed (even with unsupported features, unless `--fail-on-unsupported`), 1 with `--fail-on-unsupported` when a feature was unsupported, dropped or unmapped, and 2 for an unreadable, invalid, oversized or undetectable source (see [Importing collections](import.md#exit-codes)). `rustman import openapi` and `rustman import asyncapi` exit 0 on success, 3 when fetching the document failed, and 2 otherwise.

`rustman graphql query` exits 0 when the result has no errors, 1 when `errors` is not empty (or an assertion of the request failed), and 3 when no response came back or the document failed validation; `rustman graphql introspect` exits 3 when the server could not be reached or refused introspection (see [GraphQL](graphql.md#cli-commands)).

`rustman rpc call` and `rpc batch` exit 0 when every call returned a result, 1 when one returned a JSON-RPC error (printed on stdout) or an assertion failed, 2 when the OpenRPC catalog refuses a call, 3 when no usable answer came back, and 4 when a method guard refuses a call; `rustman rpc discover` exits 1 when the endpoint does not support `rpc.discover` (see [JSON-RPC](jsonrpc.md#exit-codes)).

`rustman grpc call` exits 0 when the call passed (status `OK` without assertions), 1 for another status or a failed assertion, 2 when the call cannot be sent as written (no schema, unknown method or field, invalid metadata), and 3 when no status came back or the deadline expired. `rustman grpc health` exits 0 for `SERVING`, 1 for `NOT_SERVING`, `SERVICE_UNKNOWN` or `UNKNOWN`, and 3 when the check fails (`UNIMPLEMENTED` included); see [gRPC](grpc.md#exit-codes).

`rustman docs build` exits 0 when the site was written, 1 when the post-build leak scan found a secret value in an output file (the file is removed), and 2 for a usage or configuration error: a non-empty `--out` folder without `.rustman-docs-manifest` (without `--force`), a secret in `[docs].publish_variables`, an unknown `--env`, an invalid template (see [API docs](api-docs.md#exit-codes)). `rustman docs serve` exits 0 when stopped with Ctrl+C (or on Unix SIGTERM or SIGHUP), and 2 when it cannot start: the port is already taken or cannot be bound, or no workspace is found. It never exits 1: a failed build (including the leak scan, an unknown `--env` or an invalid `[docs]` setting) is printed on stderr and shown in a page overlay, and the server keeps serving the last good build (see [API docs](api-docs.md#preview)).

`rustman ws connect` and `rustman sse listen` exit 0 when the session ended normally (its steps done, a bound reached, or closed by the server), 1 when an assertion of the request failed, 3 when the session failed (a refused handshake such as `ws: handshake failed: 401 Unauthorized`, a transport error, a `wait_for` timeout, an oversized message) and 130 on Ctrl+C (see [WebSocket and SSE](realtime.md#rustman-ws-connect-and-rustman-sse-listen)).

`rustman kafka`, `rustman mqtt` and `rustman amqp` `produce` and `consume` exit 0 when the operation passed (a consume that reached its bounds, even with no message), 1 when an assertion of the request failed, 3 when it failed (an unreachable broker, `kafka: unknown topic '<name>'`, `amqp: unroutable`, a failed login), 4 when the safety policy refused it, and 130 on Ctrl+C (see [Messaging](messaging.md#rustman-kafka-mqtt-amqp-produce-and-consume)).

`rustman explore run` and `explore resume` exit 1 when a finding is at or above `--fail-on` (default `high`) instead of on failed requests, and 0 otherwise; see [Exploration](exploration.md#exit-codes). `rustman report diff` exits 1 when a new finding is at or above `--fail-on` (default `high`), and `report render` and `report diff` exit 2 on a report with an unknown major schema version; see [Reports](reports.md).

On Ctrl+C (and on Unix SIGTERM or SIGHUP), in-flight requests are cancelled, stdio MCP servers are stopped with the processes they started, the requests not finished are reported as `skipped` with `skip_reason: interrupted`, every reporter writes its partial report (with `summary.interrupted: true`), and the exit code is 130. A second Ctrl+C quits at once, also with exit code 130, without waiting for reports to be written (stdio MCP servers still running are killed first).

## Terminal output and logs

Reports and response bodies go to stdout; logs, warnings, errors and Ctrl+C notices go to stderr, so `rustman run . --reporter json -vv > report.json` leaves a valid JSON document in `report.json` and the logs on the terminal.

The `pretty` report is colored only when stdout is a terminal and color is not turned off (`--color never`, a non-empty `NO_COLOR`, or `TERM=dumb`); `--color always` forces it. Piped or redirected output has no ANSI escapes and no carriage-return progress lines. Reports written to a file (`--reporter pretty:out/run.txt`) are never colored.

Logs are off unless `-v` or `RUST_LOG` is given. `RUST_LOG` takes [`tracing` filter directives](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html) and replaces the `-v` levels: `RUST_LOG=rustman_http=debug` shows the requests and headers, `RUST_LOG=trace` everything. Log lines carry only redacted text, and every line is scanned once more for the secret values the run resolved before it reaches stderr.

```text
$ rustman run users -vv --reporter json > report.json
2026-11-17T09:30:00.101Z DEBUG rustman_http::runner::execute: sending request=users/list iteration=1
2026-11-17T09:30:00.113Z  INFO rustman_http::runner::execute: GET http://localhost:8080/users -> 200 (12 ms) request=users/list status=Passed
2026-11-17T09:30:00.113Z DEBUG rustman_http::runner::execute: > Authorization: «redacted:Authorization» request=users/list
2026-11-17T09:30:00.113Z DEBUG rustman_http::runner::execute: < content-type: application/json request=users/list
```

## Completions

`rustman completions <shell>` prints a completion script for `bash`, `zsh`, `fish`, `powershell` or `elvish` on stdout:

```sh
rustman completions bash > ~/.local/share/bash-completion/completions/rustman
rustman completions zsh > "${fpath[1]}/_rustman"
rustman completions fish > ~/.config/fish/completions/rustman.fish
rustman completions powershell >> $PROFILE
```

## OAuth2 without a terminal

When stdin is not a terminal (as in CI), `send` and `run` never open a browser: a request whose `authorization_code` token is not cached fails with `oauth2: interactive login required (run 'rustman auth login <request>')`. See [OAuth2](http.md#oauth2).
