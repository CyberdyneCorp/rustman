# Plugins

Plugins extend Rustman with WebAssembly components: in-house auth schemes, domain-specific assertions, value generators, body transformers, importers, request kinds and report formats. Workspaces travel through Git, so a plugin is treated as untrusted code. It runs in a sandbox, gets nothing the workspace does not grant it in `rustman.toml`, and is pinned by the sha256 of its bytes.

The behavior is specified in [`openspec/specs/plugins`](../openspec/specs/plugins/spec.md). This page covers the plugin package and manifest, the `rustman plugin` commands, declaring and pinning plugins, URL sources, the grant vocabulary, the sandbox and its limits, trust, the extension points wired so far, the plugin API, and writing and testing plugins with the SDK, its templates and the test harness.

Plugin support is the default cargo feature `plugins` of `rustman-cli`; `cargo build --no-default-features` builds Rustman without wasmtime.

## The plugin package

A plugin is a WebAssembly **component** (`.wasm`) built against the `rustman:plugin@1.0.0` WIT package, plus a manifest, `rustman-plugin.toml`:

```toml
# plugins/hmac/rustman-plugin.toml
name = "hmac"
version = "0.2.0"
api = 1
extension_points = ["auth"]
capabilities = ["clock", "secrets:read", "net:auth.example.com"]
description = "HMAC request signing for the Acme API"
component = "hmac.wasm"
```

| Field | Required | Meaning |
|-------|----------|---------|
| `name` | yes | kebab-case (`aws-sigv4`, `faker`) |
| `version` | yes | semver (`0.3.1`) |
| `api` | yes | the plugin API major the component was built against |
| `extension_points` | yes | one or more of `auth`, `assertion`, `generator`, `transformer`, `importer`, `kind`, `report` |
| `capabilities` | yes (may be empty) | what the plugin may be granted: a class (`clock`, `random`, `env`, `fs:read`, `net`, `secrets:read`) or one exact grant (`env:HOME`, `net:auth.example.com`) |
| `description` | no | shown with the plugin |
| `component` | no | for a folder source: the component, relative to the manifest (default `plugin.wasm`) |

A missing field fails with an error naming it, for example `plugin hmac: rustman-plugin.toml: missing field 'version'`. An unknown extension point fails with `plugin hmac: unknown extension point 'telemetry'`.

The manifest can also be embedded in the component, as a custom section named `rustman-plugin.toml`. A URL source must embed it, since only the `.wasm` file is downloaded; embedded, the manifest is covered by the sha256 pin. For a `.wasm` file source, an embedded manifest is used over a `rustman-plugin.toml` next to the file; a folder source always uses the folder's manifest.

Plugin files are read before the set is trusted (by `plugin list`, the trust prompt and the pin check of every headless command), so they are read defensively: a component, a `rustman-plugin.toml`, a cached download or `rustman.lock` must be a regular file, and a symbolic link is followed only when it resolves to a regular file inside the workspace. A FIFO, a device (such as a committed `plugins/evil.wasm -> /dev/zero`), a socket or a link out of the workspace is refused at once with exit code 2, for example `plugin evil: <path>: resolves outside the workspace` or `plugin evil: <path>: not a regular file`. A component or cached download may be at most 20 MiB and a `rustman-plugin.toml` at most 256 KiB (`plugin <name>: <path>: larger than 262144 bytes`); `rustman.lock` at most 1 MiB.

Each extension point is a world of the WIT package (`auth`, `assertion`, `generator`, `transformer`, `importer`, `kind`, `report`), and the component must export the interface of every extension point its manifest declares (`rustman:plugin/auth-scheme` for `auth`, and so on). The WIT source ships with the `rustman-plugin-sdk` crate in [`crates/rustman-plugin-sdk/wit/plugin.wit`](../crates/rustman-plugin-sdk/wit/plugin.wit).

## Declaring plugins in `rustman.toml`

Only plugins declared in `rustman.toml` are loaded. A plugin's files sitting in the workspace (say `plugins/hmac/`) do nothing until a `[plugins.hmac]` table names them.

```toml
# rustman.toml
[workspace]
name = "shop"
version = 1

[plugins.hmac]
source = "plugins/hmac"
sha256 = "4f9c1b0e7a3d5c2e8b6a9f0d1c3e5a7b9d2f4a6c8e0b1d3f5a7c9e1b3d5f7a90"
grants = ["clock", "secrets:read:hmac_key"]
limits = { cpu_ms = 200 }

[plugins.faker]
source = "https://plugins.example.com/faker-1.4.0.wasm"
sha256 = "0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f9"
grants = ["random"]
```

| Key | Meaning |
|-----|---------|
| `source` | a workspace-relative path (a `.wasm` file with `rustman-plugin.toml` next to it, or a folder holding the manifest), or an `https` URL |
| `sha256` | the sha256 of the `.wasm` file, 64 hex digits |
| `grants` | what the plugin may do (see [Grants](#grants)); default `[]` |
| `limits` | `{ cpu_ms, memory_mib }`, per call (`cpu_ms` is execution time: wall time minus host calls); defaults 100 ms and 64 MiB, at most 10000 ms and 1024 MiB |

The table name is the workspace's name for the plugin and must be kebab-case. A path source must stay inside the workspace. Keys other than these are refused.

## Managing plugins

```sh
rustman plugin add ./plugins/hmac                     # a folder or .wasm file inside the workspace
rustman plugin add https://plugins.example.com/hmac-0.2.0.wasm --name hmac
rustman plugin list
rustman plugin verify                                 # in CI: exit 0, or 2 on a mismatch
rustman plugin remove hmac
```

`rustman plugin add <path|url>` reads the plugin (a [URL source](#url-sources) is downloaded and cached), checks its manifest and compiles its component (it must import only the host interfaces and export its extension points; no plugin code runs), and prints the manifest and the capabilities it requests:

```text
plugin hmac 0.2.0 (API 1)
  HMAC request signing for the Acme API
  extension points: auth
  requested capabilities: clock, secrets:read
  sha256: 4f9c1b0e7a3d5c2e8b6a9f0d1c3e5a7b9d2f4a6c8e0b1d3f5a7c9e1b3d5f7a90
added [plugins.hmac] to rustman.toml with grants = [] and updated rustman.lock
```

It then appends a `[plugins.<name>]` table with `source`, `sha256` and `grants = []` to `rustman.toml` (the rest of the file is kept as written) and records the plugin in `rustman.lock`. The name is the manifest's, or `--name`. A path source is stored relative to the workspace root, whatever the current directory; a path outside the workspace is refused. A name or source the workspace already declares is refused before anything is downloaded: plugins are never upgraded in place, so changing one is `plugin remove` then `plugin add`, and its grants are written again by hand. `plugin add` never grants anything; grants are edited in `rustman.toml`, where they show up in code review, and the changed set must be trusted again.

`rustman plugin list` shows each declared plugin with its version and API major (from its manifest on disk, else from `rustman.lock`), source and grants, and whether this machine trusts the set (`trusted`, `not trusted`, `changed since trusted`). It reads files only.

`rustman plugin verify` re-hashes every plugin against its pin, downloads the URL sources that are not cached yet (verified against the pin before they are kept), checks every manifest and grant, and brings `rustman.lock` in line with `rustman.toml`. It exits with 0 when everything matches and 2 otherwise, which makes it the CI check of a workspace's plugins.

`rustman plugin remove <name>` removes the table (and its sub-tables such as `[plugins.<name>.limits]`) from `rustman.toml` and the entry from `rustman.lock`. A plugin declared in another form, such as an inline table under `[plugins]`, is left for the user to edit.

No `rustman plugin` command trusts a plugin set, and the agent interface has none of them.

## Pinning and `rustman.lock`

Every plugin is pinned by the sha256 of its `.wasm` file. A file whose hash does not match is refused with

```text
plugin hmac: sha256 mismatch (expected 4f9c…, got 9e1d…)
```

and `rustman run` exits with code 2 before any request is sent. Plugins never update on their own: changing a plugin means changing its pin.

`rustman.lock` (tracked in Git, written by Rustman when plugins are added or verified) records the version, source, sha256 and API major each pin resolved to, so a review shows them next to a changed hash:

```toml
# rustman.lock
# Written by rustman (plugin add, plugin verify). Do not edit by hand.
version = 1

[[plugin]]
name = "faker"
version = "1.4.0"
source = "https://plugins.example.com/faker-1.4.0.wasm"
sha256 = "0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f9"
api = 1

[[plugin]]
name = "hmac"
version = "0.2.0"
source = "plugins/hmac"
sha256 = "4f9c1b0e7a3d5c2e8b6a9f0d1c3e5a7b9d2f4a6c8e0b1d3f5a7c9e1b3d5f7a90"
api = 1
```

## URL sources

A URL source is downloaded over `https` only, with TLS verification always on (a workspace `ca_bundle` can add trusted CAs, but verification cannot be turned off), at most 20 MiB, through the `http-requests` engine (without a proxy). It runs before the plugin set is trusted, so a URL committed to a repository cannot reach this machine or a private network: a host name that resolves to an address that is not public (see the network rules under [The sandbox](#the-sandbox)) is refused when the connection opens, with `<host> resolves to the non-public address <ip> (download from the address itself to reach it)`, unless the URL names that address or `localhost` itself. Redirects are followed only while they stay on `https` and on the same host and port; a redirect elsewhere fails with `redirect to another host refused`. An `http://` source fails with `plugin sources must use https` (exit code 2).

The download is verified against the pin and cached as `.rustman/plugins/<sha256>.wasm` (git-ignored with the rest of `.rustman/`). Later loads use the cache and never download again, even if the URL now serves a newer build. When the cache is missing, the pinned URL is downloaded again and verified before use; a different build is refused with the mismatch error. A cached file that no longer matches its pin is refused the same way.

## Grants

A plugin starts with no access to the filesystem, network, environment variables, clock, randomness or secrets. Each access needs a grant in `grants`, which the manifest must also request:

| Grant | Allows |
|-------|--------|
| `clock` | the wall clock |
| `random` | random bytes (at most 64 KiB per call) |
| `env:<NAME>` | one environment variable |
| `fs:read:<dir>` | reading files under a workspace-relative folder (`fs:read:.` is the whole workspace) |
| `net:<host>[:port]` | HTTP requests to one host; without a port, only ports 80 and 443 |
| `secrets:read:<name>` | the value of one secret: a secret name, or a [provider reference](secrets.md#plugins) `<provider>/<path>[#field]` |

Wildcards are refused for every grant (`plugin hmac: wildcard grants are not allowed`). An `fs:read` folder may not leave the workspace (`plugin hmac: grant 'fs:read:../' escapes the workspace`) or point into `.rustman/`. A grant the manifest does not request is refused: `plugin hmac: grant 'env:HOME' not requested by the plugin`.

## The sandbox

Plugins run in [wasmtime](https://wasmtime.dev/) with the component model and Cranelift. The host links only the `rustman:plugin` host interfaces, `http`, `clock`, `random`, `env`, `fs-read`, `secrets` and `log`; no WASI interface is linked, so a component that imports one (filesystem, sockets, environment, clocks) fails to load. Each host function checks the grant when it is called and returns `capability-denied` without doing anything otherwise. The refusal reads `capability_denied: <what>`, for example `capability_denied: fs` or `capability_denied: net evil.example.net`.

- **Files.** `fs-read` resolves the path against the workspace root, then canonically: neither `..` nor a symbolic link can reach outside the granted folder, outside the workspace, or into `.rustman/`. Listings never show `.rustman`. Only regular files are read (a FIFO or device fails with `not a regular file` instead of blocking), opened without following a symbolic link, and the opened file must still be the file at the checked path (same device and inode on Unix): a path swapped for a symbolic link between the check and the read fails with `the path changed while it was read`.
- **Network.** `http.fetch` sends through the `http-requests` engine and the session's [safety policy](safety.md) (`GovernedSender`). In `rustman run`, `send`, `import` and `report render` the policy allows the hosts of the set's `net:` grants (https or http, on the granted port or else 443 and 80) and `[safety].allowed_hosts`, under the workspace's `[safety]` settings: a host of `[safety].production_hosts` is refused, and so are destructive methods (`DELETE`). A host the plugin holds no `net:` grant for fails with `capability-denied`; a request the policy refuses fails with `policy-denied` (for example `host not allowed`). Redirects are not followed: the plugin sees the redirect response, and a new request is checked again. Every exchange is recorded in [history](history.md), redacted, as request `plugin:<name>`. Plugins have no raw sockets. A `net:` grant matches the host name the plugin asks for, and when the connection is opened every address that name resolves to is checked: a granted name that resolves to an address that is not public fails with `policy_denied: <host> resolves to the private address <ip> (grant the address itself to reach it)`. To reach such a host, grant it literally: `net:localhost` (or a `.localhost` name) or the IP address, such as `net:10.0.0.5:8080`. The check runs on the addresses actually connected to, so re-pointing DNS after the check does not get around it; behind an HTTP proxy the proxy resolves the name, and it is not checked. The trust prompt marks grants naming this machine or a private network, such as `net:localhost` or `net:169.254.169.254`. "Not public" is deny-by-default: any address that is not globally routable unicast. For IPv4 that is `0.0.0.0/8`, private `10.0.0.0/8`, `172.16.0.0/12` and `192.168.0.0/16`, shared (CGNAT) `100.64.0.0/10`, loopback `127.0.0.0/8`, link-local `169.254.0.0/16` (cloud metadata), `192.0.0.0/24`, documentation `192.0.2.0/24`, `198.51.100.0/24` and `203.0.113.0/24`, `192.88.99.0/24`, benchmarking `198.18.0.0/15`, multicast `224.0.0.0/4`, reserved `240.0.0.0/4` and broadcast. For IPv6, everything outside global unicast `2000::/3` (loopback, unspecified, unique-local `fc00::/7`, link-local `fe80::/10`, site-local `fec0::/10`, multicast, discard), plus `2001::/23` (Teredo and IETF assignments) and documentation `2001:db8::/32` and `3fff::/20`. An IPv6 address wrapping an IPv4 one is judged as that IPv4 address: IPv4-mapped `::ffff:a.b.c.d`, IPv4-compatible `::a.b.c.d`, NAT64 `64:ff9b::/96` and 6to4 `2002::/16`.
- **Host-call sizes.** One host call can copy at most about the plugin's memory limit out of the guest (wasmtime's hostcall fuel, at least 1 MiB), however its strings and lists alias each other; beyond that the call traps. `http.fetch` also takes at most 100 request headers, 64 KiB of header names and values, and a URL of 16 KiB (`at most 100 request headers`, and so on).
- **Secrets.** `secrets.lookup(name)` returns a handle; `reveal()` returns the value only with `secrets:read:<name>`. Without that grant, `lookup` returns a handle for any name, set or not, so a plugin cannot learn which secrets the session has. Values handed to a plugin without the grant carry the placeholder `«redacted:<name>»`; this holds for values a send resolves after the plugins load too (provider values, captured secrets, secret data columns), named by their variable or, for a direct `{{secret:...}}` reference, by the reference text. Everything a plugin logs, and every error it returns, is redacted of all the session's secrets, granted or not.
- **Logs.** `log.write` lines are redacted, kept (at most 1000 per instance, 8 KiB each) and logged at debug level (`-v`).

## Limits and faults

Every call has an execution time budget, `cpu_ms` (default 100 ms, enforced by wasmtime epoch interruption on a 10 ms tick). It is wall-clock time spent running the plugin, minus the time the host spends on its network or file requests; it is not CPU time measured by the OS, so a busy machine can make a call use its budget sooner and a memory limit (default 64 MiB, across all of the plugin's memories). Raise them per plugin with `limits = { cpu_ms, memory_mib }`, up to 10000 ms and 1024 MiB; a higher value fails with `plugin <name>: memory limit exceeds maximum (1024 MiB)` (or `execution time limit exceeds maximum (10000 ms)`).

Exceeding a limit aborts the call with `plugin <name>: execution time limit exceeded` or `plugin <name>: memory limit exceeded`. Time spent in host calls (network, files) does not count against the execution time limit, but a call may take at most that limit plus 60 seconds of wall time, host calls included; past that it stops with `plugin <name>: time limit exceeded` when the plugin runs again (a host call in progress is not interrupted); a trap fails it with `plugin <name> trapped: <message>`. Only that call fails: Rustman does not crash or hang, and the plugin's instance is recreated before its next call. Calls into one plugin are serialized.

## Trust

A plugin is code that came with the workspace, so a machine runs a workspace's plugin set only once it has been trusted there. The set is identified by a hash over the sorted `(name, sha256, grants, limits)` of every `[plugins.<name>]` table, plus the hash of a path source's `rustman-plugin.toml` when it sits next to the component rather than inside it (the sha256 pin covers only the `.wasm` file): adding a plugin, repinning one, changing any grant or limit, or editing such a manifest makes it a new set. The approval is kept in `.rustman/trust.yaml` (git-ignored) under the subject `plugins`, sealed with this machine's key like the [MCP client's stdio approvals](mcp-client.md#workspace-trust), so an approval copied from another machine or committed to the repository matches nothing. Checking trust reads files only: no plugin code runs, and nothing is downloaded, before the answer. The fingerprint is computed again from the files a load actually reads (the declarations and the manifest file bytes, with every component checked against its pin), and a load whose fingerprint is not the approved one is refused with `the workspace plugins changed while they were loading; review them again`, so a file changed between the trust check and the load never runs unapproved.

- **TUI.** When `rustman tui` opens a workspace whose set is not trusted, a dialog lists each plugin with its version, source and grants in plain language (`may read the secret hmac_key`, `may send HTTP requests to api.example.com`). `y` trusts the set; `n` or `Esc` leaves every plugin unloaded. The dialog says when a trusted set changed. Once trusted, a request sent from the TUI uses the plugins like a headless command does (auth schemes, generators, transformers, assertions and request kinds, with the environment's secrets and the network of their grants; a URL source must already be cached, see `rustman plugin verify`); while the set is untrusted, a request that needs a plugin fails with a message such as `plugin faker is not loaded` or `auth scheme hmac/v1: workspace plugins are not loaded`.
- **Headless commands.** `rustman run`, `send`, `bench`, `history replay`, `import <format>` and `report render` refuse an untrusted set with `workspace plugins are not trusted (run interactively or pass --trust-plugins)` and exit code 2, before any request is sent. `--trust-plugins` records the approval and loads the plugins with exactly their grants, which is what a CI job passes. A plugin whose bytes do not match their pin is refused first (exit code 2), with or without the flag.
- **Agents.** `rustman mcp serve` loads the plugins only when their set is already trusted on this machine; otherwise it starts without them and logs `plugins not loaded: workspace not trusted`. The agent interface has no tool to add, remove, trust or grant plugins; plugins reach agents only through the requests their tools send. In an agent session a plugin's network is read-only: an agent's mutations need the human's confirmation, which a plugin call cannot ask for, so a plugin's `POST`, `PUT`, `PATCH` or `DELETE` is refused with `policy-denied`. On top of its `net:` grants, every request a plugin sends in an agent session (from an auth scheme, a generator, an assertion or a request kind) must reach an origin the agent may reach (`[agent] allowed_hosts`, or the hosts of the server's environment) and is refused with `policy-denied` (`host not allowed`) otherwise; each request, sent or refused, gets an [audit](agent.md#audit-log) line with tool `plugin:<name>`, session `plugins`, its method and its redacted URL, and a request whose line cannot be written is not sent. A request of a plugin kind (`send_request`, `run_collection`) passes the agent gate like an HTTP request: it runs only when `[agent] plugin_kinds` lists its kind, it counts as a mutation (`allow_mutations` and the human's confirmation), and it is audited with the kind as its method and `<kind>:<request id>` as its URL (in a background job, as a line of its own). Otherwise it fails with `plugin kind '<kind>' not allowed for agents (list it in [agent] plugin_kinds)` before the plugin runs.

## Extension points

A plugin implements the extension points its manifest lists in `extension_points`. Each is used by its workspace name and the item's name, `<plugin>/<name>` (request kinds and import formats by their own name). A fault in a plugin (a trap, a limit, an invalid result) fails only the operation that called it: an auth scheme's, a transformer's or a request kind's fault is a request error (exit code 3) and the request is not sent (or its response not checked); an assertion function's fault is an assertion `error`; an importer's fault writes nothing (exit code 2).

Whatever a plugin returns may hold a secret it was granted, so every sink of a run (history, reports, logs, stdout) redacts every secret of the plugin session, whether or not a request references it. Each extension point has a working example: the project `rustman plugin new <name> --kind <extension point>` scaffolds (see [Writing plugins](#writing-plugins)), which CI builds, tests and adds for every extension point.

### Auth schemes

```yaml
auth: {type: plugin, scheme: hmac/v1, key_var: hmac_key}
```

Every key besides `type` and `scheme` is a parameter for the plugin, with `{{variables}}` resolved. The plugin signs the request as it is sent: method, full URL, headers and body. It returns headers to set (replacing those of the same name), query parameters to add, and optionally a new URL whose path or query differ. A result that moves the request to another host, or that sets a header choosing the host or framing the body (`Host`, `Content-Length`, `Transfer-Encoding`, `Content-Encoding`, `Connection`, `Upgrade`, `TE`, `Trailer`, `Expect`), fails with `plugin <name>: auth plugins may not change method, host or body`. With `grants = ["secrets:read:hmac_key", "clock"]` the plugin above reads the secret named by `key_var` and the clock, and sets `X-Signature` and `X-Timestamp`; like the headers of built-in auth, the headers it sets are not recorded in history or reports, and a secret value that shows up anywhere else (for example echoed in a response) is redacted, whatever the header's name.

### Assertion functions

```yaml
assertions:
  - plugin: {fn: shop/price-consistent, tolerance: 0}
```

The function receives the exchange (method, URL and headers of the request; status, headers, body and duration of the response), redacted of the secrets the plugin may not read, and the other keys as a JSON object. It returns `pass`, `fail` or `error` with a message, reported like a built-in assertion: `fail: shop/price-consistent: total 99.0 != sum 98.5`. A plugin that loops or traps reports `error: plugin shop: execution time limit exceeded` (or `trapped: <message>`), and the run continues.

### Value generators

```yaml
body: {type: text, content: "{{$faker.email}} {{$faker.name en}}"}
```

`{{$<plugin>.<name> args...}}` calls the plugin's generator with the words after the name, once for each occurrence, like the built-in dynamic variables. It returns a string. A generator that fails leaves the variable unresolved, and nothing is sent: `unresolved variable(s): $faker.email (plugin faker: random)`. A generator without the `random` or `clock` grant has no source of randomness or time: what it returns depends only on its arguments (and on what it kept from earlier calls).

### Report renderers

```sh
rustman run . --env ci --trust-plugins --reporter acme/xlsx:out/report.xlsx
rustman report render out/report.json --format acme/xlsx -o report.xlsx
```

The plugin receives the redacted `rustman.report/v1` JSON document and returns the bytes to write; the host writes them to the file as they are. A plugin format always writes a file (`-o`, or `:<path>` after `--reporter`).

### Transformers

```yaml
transform:
  request: [acme/encrypt]
  response: [acme/decrypt]
```

A transformer plugin registers request transformers and response transformers, applied in the listed order. A request transformer runs after interpolation and before auth (an auth plugin signs what it returns): it gets the URL, the headers and the body as sent, and returns the headers and body to send. It cannot pick the host or the framing: `Host` stays the request's and `Content-Length` is set from the new body. A response transformer runs before assertions and captures, which see what it returns, headers and body; it needs the whole body in memory (a spilled body fails the request). History and the JSON report keep both: the exchange holds the transformed response and `transform.original_body` the body as received.

A transformer without `secrets:read:<name>` sees `«redacted:<name>»` in place of the secret, in the URL, the headers and the body, and what it returns is sent as is: grant the secret to a request transformer that must keep it in the request. A transformer that fails, or that names a plugin that is not loaded, fails the request (exit code 3). The `transformer` template is a `stamp` request transformer (adds a header) and a `strip-xssi` response transformer (drops a `)]}',` prefix).

### Importers

```sh
rustman import soapui specs/project.xml --into soap --trust-plugins
```

An importer plugin registers formats for `rustman import <format> <src>` (a built-in subcommand such as `postman` wins over a plugin format of the same name). The source must be inside the workspace, and the plugin reads it only through its `fs:read` grants: without one covering it, the import fails with `plugin <name>: capability_denied: fs <src>`. The plugin returns request files and environments as text; it writes nothing. The host checks every one before writing any: a request file must load (a built-in kind, or a plugin kind whose schema it matches), an environment must parse, and paths stay relative (`*.rman.yaml` files under `--into`, environments under `environments/`). The first invalid file stops the import with nothing written, naming the plugin and the file: `plugin soapui: two.rman.yaml: unknown request kind 'soap' (expected one of: ...)`. `--dry-run` lists the files, and `--on-conflict skip|overwrite|rename` decides about files that exist. What the plugin returns is redacted of the session's secrets before it is written. The `importer` template converts `METHOD URL` lines.

### Request kinds

```yaml
# orders/get-order.rman.yaml
version: 1
kind: soap
name: get-order
endpoint: '{{base_url}}/soap'
action: GetOrder
```

A kind plugin registers request kinds, each with a JSON Schema for its fields and a declarative TUI pane. A kind cannot take a built-in name: `plugin <name>: kind 'http' is reserved`. In a workspace that declares plugins, request files of other kinds load as opaque requests; once the set is loaded, `rustman run` and `send` check each one before anything is sent (exit code 2): its kind must be one a plugin registers (`unknown request kind 'soapp' (expected one of: http, ..., soap)`), and its fields (without `version`, `kind`, `name`, `description`, `tags` and `variables`) must match the kind's schema (`plugin soapy: kind 'soap': /: "action" is a required property`). To run one, the host substitutes the `{{variables}}` of every field and hands the request to the plugin as JSON (redacted of the secrets it holds no grant for); the plugin executes it through its granted host functions (`http.fetch` for a network protocol) and returns what it sent and received. The JSON report and history record it as `plugin_kind` (`kind`, `summary`, `status`, `headers`, `body`); it is never replayed.

The pane lists fields, each with an editor (`text`, `code` with a language, `key-value`, or `select` with options), and response views (`raw`, `json-tree`, `table`). The TUI loads a trusted set's kinds when it opens the workspace and draws a request of a plugin kind with that pane (each field with its editor and value, then the response views); plugins never draw. The fields are edited in the request file. Agents see plugin kinds only through the requests their tools run. The `kind` template is an `echo` kind.

### Secret providers

Secret providers have no extension point in API 1. They are planned as an additive interface within major 1 once the built-in providers land; the SDK would then gain a matching world feature.

## Plugin API versions

The plugin API major is the version of the WIT package, `rustman:plugin@1.x`; this build supports API `1`. A plugin whose `api` is another major does not load (`plugin next targets plugin API 2; this rustman supports 1`), and the rest of the workspace still runs. Within a major the package only grows: new functions, types, interfaces and worlds may be added, and nothing that exists changes, so components built against an older minor keep working. A test compares the package with its frozen surface (`crates/rustman-plugin-sdk/wit/frozen/1.0.0.txt`) and fails on any non-additive edit. `rustman plugin api` prints the package version and the supported majors:

```text
rustman:plugin@1.0.0
supported plugin API majors: 1
```

## Writing plugins

`rustman-plugin-sdk` is the guest SDK: the WIT package, its version, and Rust bindings generated with `wit-bindgen` for each extension point. Each world is a cargo feature (`auth`, `assertion`, `generator`, `transformer`, `importer`, `kind`, `report`) that adds a module of the same name with the `Guest` trait to implement and an `export!` macro; the host imports are `rustman_plugin_sdk::{http, clock, random, env, fs_read, secrets, log}`, and `rustman_plugin_sdk::describe` words a host error like the host does (`capability_denied: clock`). The `runtime` feature gives a `no_std` plugin its allocator, `cabi_realloc` and panic handler, so the component imports nothing but the Rustman host interfaces: the host links no WASI at all, and a component that imports a WASI interface does not load.

> **Until `rustman-plugin-sdk` is published on crates.io, scaffold with `--sdk <path>`.** The default dependency is a git dependency on the Rustman repository, which only builds once the SDK is available from there at a matching version; point `--sdk` at the `crates/rustman-plugin-sdk` folder of a Rustman checkout instead, as below, and commit the `Cargo.lock` of the first build.

```sh
git clone https://github.com/CyberdyneCorp/rustman
rustman plugin new my-auth --kind auth --sdk rustman/crates/rustman-plugin-sdk
cd my-auth
rustup target add wasm32-wasip2
cargo build --target wasm32-wasip2 --release
rustman plugin test .
```

`rustman plugin new <name> --kind <extension point>` writes a project into `./<name>` (or `--dir`):

| File | What it holds |
|------|---------------|
| `Cargo.toml` | a `cdylib` depending on `rustman-plugin-sdk` with the world's feature and `runtime`, its own `[workspace]`, `panic = "abort"` and a size-optimized release profile. Until the SDK is published on crates.io it comes from the Rustman repository (`git = "https://github.com/CyberdyneCorp/rustman", version = "1"`, which keeps it on API major 1; the first build pins the commit in `Cargo.lock`, which the README says to commit and build `--locked`); `--sdk <path>` uses a local copy instead, such as `crates/rustman-plugin-sdk` of a checkout |
| `rustman-plugin.toml` | the manifest, with `component = "target/wasm32-wasip2/release/<crate>.wasm"` |
| `src/lib.rs` | a `no_std` sample implementation |
| `rustman-plugin-tests.toml` | test cases for `rustman plugin test` |
| `README.md` | the build, test and use recipe |

The samples are an HMAC-SHA256 auth scheme (`v1`, needs `clock` and `secrets:read:<key_var>`), a `status-2xx` assertion function, a `slug` generator, a `stamp` request transformer and a `strip-xssi` response transformer, a `lines` importer, an `echo` request kind with its pane, and a `text` report renderer. CI scaffolds, builds (`--locked`, against the committed lockfiles in `scripts/plugin-template-locks/`, regenerated with `--update-locks` after a template's dependencies change), tests and adds every one of them (`scripts/check_plugin_templates.py`), so each template is also a working example of its extension point. The built project is a folder source: from the workspace root, `rustman plugin add ./my-auth` pins it.

Test plugins in this repository's own test suite are written in WebAssembly text against the canonical ABI and wrapped into components in-process (the `test-fixtures` feature of `rustman-plugin`), so the main test suite needs no `wasm32-wasip2` toolchain.

## Testing plugins

`rustman plugin test [path]` runs a plugin's test cases with the host test harness: the real sandbox, with exactly the grants each case lists, against a fake clock, fake randomness, a fake environment, a fake network and fake secrets. Nothing real is reachable, so the same cases give byte-identical output on every run and platform. `path` is a plugin folder (default `.`) or a `.wasm` file; the cases are `rustman-plugin-tests.toml` next to the manifest, or `--cases <file>`.

```toml
# my-auth/rustman-plugin-tests.toml
[fakes]
clock = 2026-01-01T00:00:00Z
random_seed = 7
env = { REGION = "eu-west-1" }
secrets = { hmac_key = "test-secret" }

[[fakes.http]]
method = "POST"
url = "https://auth.example.com/token"
status = 200
headers = { "content-type" = "application/json" }
body = '{"token":"t"}'

[[case]]
name = "signs with the fixed clock and the fake secret"
grants = ["clock", "secrets:read:hmac_key"]
sign = { scheme = "v1", method = "POST", url = "https://api.example.com/orders", params = { key_var = "hmac_key" } }
expect = { "header.X-Timestamp" = "1767225600", "header.X-Signature" = "0278871e60295553e9588cd1ee773721a033c7ea9e7114d87148c9897f1abf71" }

[[case]]
name = "cannot read the secret without its grant"
grants = ["clock"]
sign = { scheme = "v1", url = "https://api.example.com/orders", params = { key_var = "hmac_key" } }
expect_error = "secrets hmac_key"
```

`[fakes]` holds what every case runs against, and a case's own `fakes` table adds to it (its clock and seed win, its maps add and replace entries, its responses are matched first):

| Fake | Meaning |
|------|---------|
| `clock` | the time `clock.now` returns: a UTC datetime (`2026-01-01T00:00:00Z`, as a TOML datetime or a string) or Unix seconds; 1970-01-01T00:00:00Z when unset |
| `random_seed` | the seed of the bytes `random.bytes` returns (SplitMix64); 0 when unset |
| `env` | the variables `env.get` sees; the process environment is never read |
| `secrets` | the session's secrets: revealed only with `secrets:read:<name>`, and redacted from everything shown |
| `http` | canned responses (`method`, default `GET`; `url`, matched exactly; `status`, default 200; `headers`; `body`). A granted request without one fails with `no fake response for <method> <url>` |

Each `[[case]]` has a `name`, its `grants` (default none; checked against the manifest like a workspace's), optional `limits`, and exactly one call:

| Call | Fields | Output lines |
|------|--------|--------------|
| `sign` | `scheme`, `method` (GET), `url`, `headers`, `body`, `params` | `header.<name>`, `query.<name>`, `url` |
| `check` | `function`, `method`, `url`, `request_headers`, `request_body`, `status` (200), `headers`, `body`, `duration_ms`, `args` (a JSON object) | `verdict` (`pass`, `fail`, `error`), `message` |
| `generate` | `name`, `args` | `value` |
| `transform_request` | `name`, `url`, `headers`, `body` | `header.<name>`, `body` |
| `transform_response` | `name`, `status` (200), `headers`, `body` | `header.<name>`, `body` |
| `convert` | `format`, `source_path`, `source` | `request.<path>`, `environment.<path>` |
| `execute` | `kind`, `request` (JSON) | `summary`, `status`, `header.<name>`, `body` |
| `render` | `format`, `report` (JSON) | `output` |

`expect` maps output lines to the exact values they must have; `expect_error` instead requires the call to fail with a message containing its text. A case without either passes when the call succeeds. The command prints every case with its output lines, the plugin's log lines and the host calls it was refused, then a count:

```text
plugin my-auth 0.1.0
ok   signs with the fixed clock and the fake secret
       header.X-Signature: 0278871e60295553e9588cd1ee773721a033c7ea9e7114d87148c9897f1abf71
       header.X-Timestamp: 1767225600
ok   cannot read the secret without its grant
       capability_denied: secrets hmac_key
2 passed, 0 failed
```

It exits with 0 when every case passes, 1 when one fails (with a `!` line saying what differed), and 2 when the plugin or its cases cannot be read. Everything shown is redacted of the fake secrets, like plugin output in a real session (`value: «redacted:api_token»`); expectations are compared with the real values. The fake services only replace what a granted call reaches: a call without its grant is refused by the sandbox first, exactly as in a run.
