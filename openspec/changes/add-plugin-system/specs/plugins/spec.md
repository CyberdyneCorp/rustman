# Plugins

## Purpose

Sandboxed WASM component-model plugins that extend Rustman through declared extension points: auth schemes, assertions, value generators, transformers, importers, request kinds and report renderers. A plugin can do nothing outside its sandbox unless the workspace explicitly grants it a capability.

## ADDED Requirements

### Requirement: Plugin package and manifest
A plugin SHALL be a WebAssembly component (`.wasm`) shipped next to a `rustman-plugin.toml` manifest. The manifest SHALL declare `name` (kebab-case), `version` (semver), `api` (plugin API major, e.g. `1`), `extension_points` and `capabilities`. A missing manifest, a missing field or an unknown extension point SHALL fail with an error naming the field.

#### Scenario: Valid manifest
- **GIVEN** a manifest with `name = "aws-sigv4"`, `version = "0.3.1"`, `api = 1`, `extension_points = ["auth"]`, `capabilities = ["clock"]`
- **WHEN** the plugin is loaded
- **THEN** it SHALL register one auth scheme and request only the `clock` capability

#### Scenario: Unknown extension point
- **GIVEN** a manifest with `extension_points = ["telemetry"]`
- **WHEN** the plugin is loaded
- **THEN** loading SHALL fail with `plugin aws-sigv4: unknown extension point 'telemetry'`

### Requirement: Plugin API versioning
The host SHALL support one or more plugin API majors, listed by `rustman plugin api`. A plugin whose `api` major is unsupported SHALL NOT load, and SHALL fail with `plugin <name> targets plugin API <n>; this rustman supports <list>`. Within a major, the host SHALL accept components built against any older minor.

#### Scenario: Newer API major
- **GIVEN** the host supports API `1` and a plugin declares `api = 2`
- **WHEN** the workspace loads
- **THEN** the plugin SHALL NOT load and the incompatibility error SHALL be shown
- **AND** requests that don't use the plugin SHALL still run

### Requirement: Declaring plugins in the workspace
Plugins SHALL be declared in `rustman.toml` as `[plugins.<name>]` with `source` (a workspace-relative path or an `https` URL), `sha256` and `grants`. A plugin that isn't declared SHALL NOT be loaded, even if its files are present in the workspace.

#### Scenario: Undeclared plugin files
- **GIVEN** a `plugins/hmac/` directory containing a valid plugin and no `[plugins.hmac]` entry
- **WHEN** the workspace loads
- **THEN** the plugin SHALL NOT be loaded

### Requirement: Integrity pinning and lockfile
Every plugin SHALL be pinned by the sha256 of its `.wasm` file. `rustman.lock` SHALL record name, version, source, sha256 and API major for each plugin. A file whose hash doesn't match SHALL be refused with `plugin <name>: sha256 mismatch (expected <a>, got <b>)` and exit code 2. Plugins SHALL never auto-update.

#### Scenario: Tampered plugin
- **GIVEN** `rustman.toml` pins `hmac` to sha256 `ab12…`
- **WHEN** the `.wasm` file on disk has a different hash
- **THEN** the plugin SHALL be refused with the mismatch error
- **AND** `rustman run` SHALL exit with code 2 before any request is sent

#### Scenario: No silent upgrade
- **GIVEN** the plugin URL now serves a newer build
- **WHEN** the workspace loads
- **THEN** the cached, pinned build SHALL be used and no download SHALL occur

### Requirement: Plugin CLI
The CLI SHALL provide these commands:
- `rustman plugin add <path|url> [--name n]`: fetch, hash, show the manifest and requested capabilities, then write `rustman.toml` and `rustman.lock` with no grants.
- `plugin list`: show name, version, API, grants and trust state.
- `plugin remove <name>`.
- `plugin verify`: re-hash every plugin, exit 0 or 2.
- `plugin new <name>`: scaffold a project from the template.

#### Scenario: Add without grants
- **WHEN** the user runs `rustman plugin add https://example.com/hmac-0.2.0.wasm`
- **THEN** the manifest and its requested capabilities SHALL be printed
- **AND** `rustman.toml` SHALL gain `[plugins.hmac]` with `source`, `sha256` and `grants = []`
- **AND** `rustman.lock` SHALL be updated

#### Scenario: Verify in CI
- **GIVEN** a workspace where every plugin hash matches
- **WHEN** `rustman plugin verify` runs
- **THEN** it SHALL exit with code 0

### Requirement: URL sources
`plugin add` SHALL fetch URL sources over `https` only, with TLS verification on and a 20 MiB size limit, using the `http-requests` engine. The file SHALL be cached in `.rustman/plugins/<sha256>.wasm`. When the cache is missing at load time, the pinned URL SHALL be re-fetched and verified against the pin before use.

#### Scenario: Plain HTTP refused
- **WHEN** the user runs `rustman plugin add http://example.com/p.wasm`
- **THEN** it SHALL fail with `plugin sources must use https` and exit code 2

### Requirement: Workspace plugin trust
The first time a workspace's plugin set is loaded (or after any plugin's sha256 or grants change), the TUI SHALL show each plugin's name, version, source and granted capabilities, and SHALL ask the user to trust them. Approval SHALL be stored in `.rustman/trust.yaml`, keyed by a hash of the plugin set. Declining SHALL leave every plugin unloaded.

#### Scenario: Teammate adds a plugin
- **GIVEN** a trusted workspace, and a `git pull` that adds `[plugins.hmac]`
- **WHEN** the TUI opens the workspace
- **THEN** the trust prompt SHALL list `hmac` and its grants
- **AND** no plugin code SHALL run before the user answers

#### Scenario: Grants changed
- **GIVEN** a trusted plugin whose grants gain `net:api.example.com`
- **WHEN** the workspace loads
- **THEN** the trust prompt SHALL be shown again

### Requirement: Trusted load verification
A load SHALL compute the plugin-set hash again from the files it reads, and SHALL refuse a set whose hash is not the approved one, so files changed between the trust check and the load never run unapproved.

#### Scenario: Manifest edited after the trust check
- **GIVEN** a trusted plugin set whose manifest file is edited after the trust check passed
- **WHEN** the set loads
- **THEN** loading SHALL fail with `the workspace plugins changed while they were loading; review them again`

### Requirement: Plugins in TUI sends
Once a workspace's plugin set is trusted, a request sent from the TUI SHALL use its extension points like a headless command does. While the set is untrusted, a request that needs a plugin SHALL fail with a message naming the unloaded plugin.

#### Scenario: Generator in a TUI send
- **GIVEN** a request using `{{$faker.email}}` and the `faker` plugin
- **WHEN** it is sent from the TUI after the set was trusted
- **THEN** the sent URL SHALL hold the generated value
- **AND** without trust the send SHALL fail with `plugin faker is not loaded`

### Requirement: Headless plugin trust
Headless commands (`rustman run`, `send`, `bench`, `history replay`, `explore` and others) SHALL refuse to load untrusted plugins. They SHALL fail with `workspace plugins are not trusted (run interactively or pass --trust-plugins)` and exit code 2, unless `--trust-plugins` is passed or the current plugin-set hash is already trusted.

#### Scenario: CI run
- **GIVEN** a fresh CI checkout of a workspace with plugins
- **WHEN** `rustman run .` is executed without `--trust-plugins`
- **THEN** it SHALL exit with code 2 and the trust error, before any request is sent

#### Scenario: CI run with opt-in
- **WHEN** `rustman run . --trust-plugins` is executed and every hash matches
- **THEN** the plugins SHALL load with exactly their declared grants

### Requirement: Capability sandbox
A plugin SHALL start with no access to the filesystem, network, environment variables, clock, randomness source or secrets. Each access SHALL require a grant listed in `grants`, and SHALL also be requested in the manifest. Calling a host function without its grant SHALL return a `capability_denied` error to the plugin. The host SHALL never hand out ambient WASI capabilities.

#### Scenario: Ungranted file read
- **GIVEN** a plugin with `grants = []`
- **WHEN** it tries to open a file
- **THEN** the call SHALL fail with `capability_denied: fs`
- **AND** no host file SHALL be read

#### Scenario: Granted folder symlinked out of the workspace
- **GIVEN** `grants = ["fs:read:data"]` and `data` a symbolic link to a folder outside the workspace
- **WHEN** the plugin reads `data/<file>`
- **THEN** the call SHALL fail with `capability_denied: fs`

#### Scenario: Grant beyond the manifest
- **GIVEN** `grants = ["env:HOME"]` and a manifest that doesn't request `env`
- **WHEN** the workspace loads
- **THEN** loading SHALL fail with `plugin <name>: grant 'env:HOME' not requested by the plugin`

### Requirement: Grant vocabulary
These grants SHALL be supported:
- `clock`
- `random`
- `env:<NAME>` (one variable)
- `fs:read:<workspace-relative dir>`, which SHALL NOT resolve outside the workspace root or into `.rustman/`
- `net:<host>[:port]`
- `secrets:read:<name>`

Wildcards SHALL NOT be accepted for `env`, `net` or `secrets`.

#### Scenario: Path escape
- **GIVEN** `grants = ["fs:read:../"]`
- **WHEN** the workspace loads
- **THEN** loading SHALL fail with `plugin <name>: grant 'fs:read:../' escapes the workspace`

#### Scenario: Wildcard host refused
- **GIVEN** `grants = ["net:*"]`
- **THEN** loading SHALL fail with `plugin <name>: wildcard grants are not allowed`

### Requirement: Plugin network access
A plugin with `net:<host>` SHALL send requests only through a host-provided HTTP function, which uses the `http-requests` engine and the safety policy. A request to a host that isn't granted, or that the safety policy blocks, SHALL return `capability_denied` or `policy_denied` to the plugin. Plugins SHALL have no raw sockets.

#### Scenario: Granted host
- **GIVEN** `grants = ["net:auth.example.com"]`
- **WHEN** the plugin fetches `https://auth.example.com/token`
- **THEN** the request SHALL be sent and its exchange SHALL be recorded in history like any other exchange

#### Scenario: Other host
- **WHEN** the same plugin fetches `https://evil.example.net/`
- **THEN** the call SHALL fail with `capability_denied: net evil.example.net`

### Requirement: Resolved address check
A granted host name SHALL NOT reach a loopback, private, link-local or unspecified address it resolves to, checked on the addresses connected to, unless the grant names that address or `localhost` literally.

#### Scenario: Granted name resolving to a private address
- **GIVEN** `grants = ["net:api.example.com"]` and `api.example.com` resolving to `169.254.169.254`
- **WHEN** the plugin fetches `https://api.example.com/`
- **THEN** the connection SHALL be refused with `policy_denied: api.example.com resolves to the private address 169.254.169.254 (grant the address itself to reach it)`
- **AND** a grant naming the address itself (`net:169.254.169.254`) or `localhost` SHALL still reach it

### Requirement: Secrets inside plugins
A plugin SHALL receive a secret value in clear only when it holds `secrets:read:<name>` for that secret. Otherwise, values passed to the plugin SHALL carry the placeholder `«redacted:<name>»`. Anything a plugin returns, logs or reports SHALL pass through the redaction invariant of `environments`.

#### Scenario: Auth plugin with a grant
- **GIVEN** an auth plugin with `grants = ["secrets:read:aws_secret_key", "clock"]`
- **WHEN** it signs a request
- **THEN** it SHALL receive the real `aws_secret_key` value
- **AND** history SHALL NOT contain the real value: like built-in auth, the headers an auth plugin sets are not recorded, and wherever the value shows up (for example echoed in a response) it SHALL appear as `«redacted:aws_secret_key»`

#### Scenario: Plugin without a grant
- **GIVEN** a transformer plugin without secret grants, and a body containing `{{api_token}}`
- **WHEN** the transformer runs
- **THEN** it SHALL see `«redacted:api_token»` in place of the value

#### Scenario: Plugin echoes a secret
- **GIVEN** a plugin granted `secrets:read:api_token` that writes the value into its log output
- **THEN** the log line SHALL show `«redacted:api_token»`

### Requirement: Resource limits
Each plugin call SHALL have an execution time budget, `cpu_ms` (default 100 ms of wall time spent running the plugin, excluding host calls, enforced by epoch interruption) and a memory limit (default 64 MiB). Either can be raised per plugin with `limits = { cpu_ms, memory_mib }`, up to 10 s and 1024 MiB. Exceeding a limit SHALL abort the call with `plugin <name>: execution time limit exceeded` or `memory limit exceeded`, and SHALL NOT crash Rustman.

#### Scenario: Infinite loop
- **GIVEN** an assertion plugin that loops forever
- **WHEN** a request using it runs
- **THEN** the call SHALL be aborted after about 100 ms
- **AND** the assertion SHALL report `error: plugin my-check: execution time limit exceeded`
- **AND** the run SHALL continue with the next request

#### Scenario: Limit above maximum
- **GIVEN** `limits = { memory_mib = 4096 }`
- **THEN** loading SHALL fail with `plugin <name>: memory limit exceeds maximum (1024 MiB)`

### Requirement: Plugin fault isolation
A plugin trap, panic, limit breach or invalid return value SHALL be reported as a plugin error scoped to the operation that invoked it. A plugin auth failure, transformer failure or kind failure SHALL be a request error (exit code 3). An assertion plugin failure SHALL be an assertion `error`. The host SHALL recreate the plugin instance before its next call.

#### Scenario: Trap during auth
- **GIVEN** an auth plugin that traps
- **WHEN** a request using it is sent
- **THEN** the request SHALL NOT be sent
- **AND** the result SHALL be `error: plugin aws-sigv4 trapped: <message>` and count as a request error

### Requirement: Auth scheme extension point
An `auth` plugin SHALL register named schemes, usable as `auth: {type: plugin, scheme: <plugin>/<scheme>, ...params}`. The host SHALL pass the fully resolved request: method, URL, ordered headers, body bytes and the params. The plugin SHALL return headers to add or replace, and query parameters to add. It SHALL NOT change the method, the host or the body.

#### Scenario: HMAC signing
- **GIVEN** a request with `auth: {type: plugin, scheme: hmac/v1, key_var: hmac_key}` and the plugin granted `secrets:read:hmac_key` and `clock`
- **WHEN** the request is sent
- **THEN** the sent request SHALL carry the `X-Signature` and `X-Timestamp` headers the plugin returned

#### Scenario: Plugin tries to change the host
- **WHEN** an auth plugin's result rewrites the URL host
- **THEN** the request SHALL fail with `plugin <name>: auth plugins may not change method, host or body`

### Requirement: Assertion extension point
An `assertion` plugin SHALL register named functions, usable as `- plugin: {fn: <plugin>/<fn>, ...args}` in `assertions`. The plugin SHALL receive the redacted exchange and the args. It SHALL return `pass`, or `fail` or `error` with a message, which SHALL be reported like a built-in assertion.

#### Scenario: Custom business rule
- **GIVEN** `- plugin: {fn: shop/price-consistent}`, and a response where the line item totals don't add up to `total`
- **WHEN** the assertion runs
- **THEN** it SHALL report `fail: shop/price-consistent: total 99.0 != sum 98.5`

### Requirement: Value generator extension point
A `generator` plugin SHALL register dynamic variables named `{{$<plugin>.<name> args...}}`. They SHALL be re-evaluated for each occurrence, like the built-in dynamic variables. Generators SHALL return a string. Generators without `random` or `clock` grants SHALL be deterministic for the same args.

#### Scenario: Custom generator
- **GIVEN** a `faker` plugin registering `email`
- **WHEN** a body contains `{{$faker.email}}` twice
- **THEN** both occurrences SHALL be replaced by strings the plugin produced

### Requirement: Transformer extension point
A `transformer` plugin SHALL register request transformers (run after interpolation and before auth) and response transformers (run before assertions and captures). They're applied per request via `transform: {request: [...], response: [...]}`. History SHALL store both the original and the transformed response body.

#### Scenario: Decrypt a response before assertions
- **GIVEN** `transform: {response: [acme/decrypt]}`
- **WHEN** an encrypted body arrives
- **THEN** assertions SHALL evaluate the decrypted body
- **AND** history SHALL store both bodies

### Requirement: Importer extension point
An `importer` plugin SHALL register a format usable as `rustman import <format> <src>`. With `fs:read` granted for the source path, the plugin SHALL receive the source bytes. It SHALL return request files and environments using the workspace schema, which the host validates and writes. The plugin SHALL NOT write files itself.

#### Scenario: Plugin importer
- **GIVEN** a plugin registering format `soapui`
- **WHEN** the user runs `rustman import soapui project.xml`
- **THEN** the returned requests SHALL be validated and written as `*.rman.yaml` files by the host

#### Scenario: Invalid output
- **WHEN** an importer returns a request with an unregistered `kind`
- **THEN** nothing SHALL be written, and the error SHALL name the plugin and the offending request

### Requirement: Request kind extension point
A `kind` plugin SHALL register a new request kind in the `workspace` kind registry, with a JSON Schema for its fields. It SHALL execute requests through granted host functions and return an exchange. The TUI pane SHALL be described declaratively (fields, editors, response views) and rendered by `tui`. Plugin kinds SHALL NOT override built-in kinds.

#### Scenario: Plugin kind loads
- **GIVEN** a plugin registering kind `soap`
- **WHEN** the workspace loads `kind: soap` request files
- **THEN** they SHALL load and run through the plugin
- **AND** the TUI SHALL render the declared pane

#### Scenario: Built-in kind collision
- **WHEN** a plugin tries to register kind `http`
- **THEN** loading SHALL fail with `plugin <name>: kind 'http' is reserved`

### Requirement: Report renderer extension point
A `report` plugin SHALL register an output format usable as `--reporter <plugin>/<format>:path` and in `rustman report render --format <plugin>/<format>`. It SHALL receive the redacted `rustman.report/v1` document and return the bytes to write, which the host writes.

#### Scenario: Company report format
- **GIVEN** a plugin registering `acme/xlsx`
- **WHEN** the user runs `rustman report render it.json --format acme/xlsx -o r.xlsx`
- **THEN** `r.xlsx` SHALL contain the bytes the plugin returned

### Requirement: Agents cannot manage plugins
The agent interface SHALL NOT expose tools to add, remove, trust or grant plugins. MCP server sessions SHALL load only plugins whose plugin-set hash is already trusted, and their plugins SHALL send only safe-method (read-only) requests. Plugin capabilities SHALL appear to agents only through the tools they back (for example a request kind used by `send_request`).

#### Scenario: Untrusted plugins in agent mode
- **GIVEN** a workspace whose plugin set isn't trusted
- **WHEN** `rustman mcp serve` starts
- **THEN** the server SHALL start without plugins and log `plugins not loaded: workspace not trusted`

#### Scenario: Plugins in agent sessions only read
- **GIVEN** a trusted plugin with a `net:` grant, loaded by `rustman mcp serve`
- **WHEN** it sends a `POST` to its granted host during an agent's request
- **THEN** the host SHALL refuse it with `policy-denied`, since an agent's mutations need the human's confirmation, which a plugin call cannot ask for

### Requirement: Plugin SDK and template
A `rustman-plugin-sdk` crate SHALL provide Rust bindings for the WIT world of each extension point. `rustman plugin new <name> --kind <extension point>` SHALL scaffold a buildable project with a manifest, a sample implementation, tests and a `cargo build --target wasm32-wasip2` recipe.

#### Scenario: Scaffold builds
- **WHEN** the user runs `rustman plugin new my-auth --kind auth` and builds the project
- **THEN** a `.wasm` component and a valid manifest SHALL be produced
- **AND** `rustman plugin add ./my-auth` SHALL accept them

### Requirement: Host test harness
The SDK SHALL ship a test harness that runs a plugin component in the real host sandbox, with fake clock, randomness, network and secrets. Plugin tests SHALL then be deterministic. `rustman plugin test <path>` SHALL run the plugin's declared test cases and exit with 0 on pass or 1 on failure.

#### Scenario: Deterministic signing test
- **GIVEN** a test case fixing the clock to `2026-01-01T00:00:00Z` and a fake secret
- **WHEN** `rustman plugin test ./my-auth` runs twice
- **THEN** both runs SHALL produce byte-identical signatures and exit with code 0
