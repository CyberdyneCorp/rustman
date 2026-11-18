# Spec Delta

## Purpose

Defines how a Rustman workspace is laid out on disk: the `rustman.toml` root, one request per `*.rman.yaml` file, nested collections with a stable order, and `rustman init`. Workspaces are plain text meant to be reviewed and versioned in Git.

## ADDED Requirements

### Requirement: Workspace discovery
The system SHALL treat a directory containing `rustman.toml` as a workspace. Commands SHALL locate the workspace by searching the current directory and then its ancestors, stopping at the first match. `--workspace <dir>` SHALL override the search. If no workspace is found, commands that need one SHALL fail with `error: no rustman workspace found (run 'rustman init')` and exit code 2.

#### Scenario: Run from a nested folder
- **GIVEN** `/proj/rustman.toml` exists
- **WHEN** the user runs `rustman run users` from `/proj/api/users`
- **THEN** the workspace root SHALL be `/proj`

#### Scenario: No workspace
- **GIVEN** no `rustman.toml` exists in the current directory or any ancestor
- **WHEN** the user runs `rustman run .`
- **THEN** the system SHALL print `error: no rustman workspace found (run 'rustman init')` and exit with code 2

### Requirement: Workspace settings file
`rustman.toml` SHALL hold `[workspace] name` and `version = 1`, plus optional `[defaults]` (`timeout_ms`, `follow_redirects`, `max_redirects`, `verify_tls`, `default_environment`) and an `[http]` table (see Workspace HTTP settings). Unknown keys SHALL produce a warning, not an error. A `version` greater than the supported one SHALL fail with `error: rustman.toml version <n> is newer than this rustman supports (1)`.

#### Scenario: Minimal settings
- **GIVEN** `rustman.toml` contains only `[workspace]\nname = "shop"\nversion = 1`
- **WHEN** the workspace loads
- **THEN** every default SHALL apply (timeout 30000 ms, redirects followed up to 10, TLS verified)

#### Scenario: Future version
- **GIVEN** `rustman.toml` has `version = 2`
- **WHEN** any command loads the workspace
- **THEN** it SHALL fail with the newer-version error and exit code 2

### Requirement: Workspace HTTP settings
The optional `[http]` table SHALL accept `ca_bundle`, `proxy`, `max_body_bytes`, and a workspace client certificate (`client_cert`, `client_key`, `client_pkcs12`, `client_pkcs12_password`) that applies to requests that set none of their own. `[http]` file paths SHALL be relative to the workspace root, and `client_pkcs12_password` SHALL be the one interpolated value, so that it can reference a secret variable.

#### Scenario: Workspace client certificate
- **GIVEN** `[http] client_cert = "certs/client.pem"` and `client_key = "certs/client.key"`
- **WHEN** a request without its own certificate is sent
- **THEN** it SHALL present the workspace certificate, resolved from the workspace root

### Requirement: Request file schema
Each saved request SHALL be one UTF-8 YAML file named `*.rman.yaml`. It SHALL contain `version: 1`, `kind` and `name`. Optional fields SHALL be `description`, `tags`, `assertions` and `captures`, plus the kind-specific fields. Kinds come from a registry: this change registers `http`, `graphql` and `mcp`, and later capabilities register more. A missing or unregistered kind SHALL fail to load with an error naming the file, line and registered kinds.

#### Scenario: Minimal HTTP request
- **GIVEN** a file `get-user.rman.yaml` containing `version: 1`, `kind: http`, `name: Get user`, `method: GET`, `url: "{{base_url}}/users/1"`
- **WHEN** the workspace loads
- **THEN** the request SHALL be available as `Get user`

#### Scenario: Unknown kind
- **GIVEN** a request file with `kind: soap`
- **WHEN** the workspace loads
- **THEN** loading SHALL report `<path>:2: unknown request kind 'soap' (expected one of: <registered kinds>)`, e.g. `http, graphql, mcp` when only this change is present

### Requirement: HTTP request fields
A `kind: http` file SHALL support these fields: `method` (any RFC 9110 token, so custom methods are allowed; defaults to `GET`), `url`, `query` (an ordered list of name/value pairs, where repeats are allowed), `headers` (an ordered list of name/value pairs) and `body`. `auth` and `settings` (overrides for timeout, redirects and TLS) SHALL also be supported. Header and query order SHALL be preserved when sending.

#### Scenario: Custom method and repeated query
- **GIVEN** `method: PURGE` and `query: [{name: tag, value: a}, {name: tag, value: b}]`
- **WHEN** the request is sent
- **THEN** the request line SHALL be `PURGE /path?tag=a&tag=b`

#### Scenario: Invalid method token
- **GIVEN** `method: "GET USERS"`
- **WHEN** the workspace loads
- **THEN** loading SHALL fail with `invalid HTTP method 'GET USERS'`

### Requirement: Body types
`body` SHALL be one of: `{type: json, content: <yaml value or string>}`, `{type: text, content}`, `{type: form, fields: [...]}` (urlencoded), `{type: multipart, parts: [...]}` (with text parts or `file:` parts), `{type: file, path}` (raw binary sent from disk) or `{type: none}`. File paths SHALL be resolved relative to the request file and SHALL NOT escape the workspace root.

#### Scenario: JSON body from YAML
- **GIVEN** `body: {type: json, content: {name: "Ana", age: 3}}`
- **WHEN** the request is sent
- **THEN** the body SHALL be `{"name":"Ana","age":3}` and `Content-Type` SHALL default to `application/json`

#### Scenario: Multipart with file part
- **GIVEN** a multipart part `{name: avatar, file: ./fixtures/a.png}`
- **WHEN** the request is sent
- **THEN** the part SHALL carry filename `a.png` and a content type guessed from the extension (`image/png`)

#### Scenario: Path escape rejected
- **GIVEN** `body: {type: file, path: ../../etc/passwd}`
- **WHEN** the workspace loads
- **THEN** loading SHALL fail with `file path escapes workspace: ../../etc/passwd`

### Requirement: Reserved request kinds
Files with `kind: graphql` or `kind: mcp` SHALL be parsed and listed. Until the capability for that kind is installed, executing one SHALL produce a `skipped` result with reason `request kind '<kind>' is not supported by this build`.

#### Scenario: GraphQL file before GraphQL support
- **GIVEN** a collection containing one `kind: graphql` file
- **WHEN** `rustman run` executes the collection
- **THEN** that request SHALL be reported as skipped with the unsupported-kind reason and SHALL NOT count as a failure

### Requirement: Collections and ordering
Every directory under the workspace root SHALL be a collection, except `environments/`, directories whose name starts with `.` or `_` (e.g. `.rustman/`, `__snapshots__/`, `_removed/`), and directories with no request file at any depth (e.g. `mocks/`). Directories can nest. Order SHALL follow an optional `_collection.yaml` `order:` list, then remaining entries by file name.

#### Scenario: Explicit order
- **GIVEN** a folder with `login.rman.yaml`, `list.rman.yaml`, `delete.rman.yaml` and `_collection.yaml` with `order: [login, list]`
- **WHEN** the folder is run
- **THEN** the order SHALL be `login`, `list`, `delete`

#### Scenario: Tool directories are not collections
- **GIVEN** a workspace with `users/get.rman.yaml`, `users/__snapshots__/get.default.snap.yaml`, and `mocks/users.rman-mock.yaml`
- **WHEN** the workspace loads
- **THEN** `users` SHALL be the only collection

### Requirement: Collection metadata
A `_collection.yaml` MAY define `name`, `description`, and collection-level `headers`, `auth` and `variables`, which SHALL be inherited by the requests inside; a request's own value SHALL override the inherited one.

#### Scenario: Inherited header
- **GIVEN** `_collection.yaml` with header `X-Team: core` and a request that sets no `X-Team`
- **WHEN** the request is sent
- **THEN** it SHALL carry `X-Team: core`

#### Scenario: Inherited auth
- **GIVEN** `_collection.yaml` defines bearer auth and a child request has no `auth`
- **WHEN** the child is sent
- **THEN** it SHALL use the collection's bearer auth

#### Scenario: Request overrides inheritance
- **GIVEN** a child request sets `auth: {type: none}`
- **WHEN** it is sent
- **THEN** no `Authorization` header SHALL be added

### Requirement: Workspace initialization
`rustman init [dir]` SHALL create `rustman.toml`, `environments/local.yaml`, a sample `examples/ping.rman.yaml`, and a `.gitignore` entry for `.rustman/`. It SHALL refuse to overwrite an existing `rustman.toml`, failing with `error: <dir> is already a rustman workspace` (exit 2). `--bare` SHALL skip the sample files (`environments/local.yaml` and `examples/ping.rman.yaml`), creating only `rustman.toml` and the `.gitignore` entry.

#### Scenario: Fresh init
- **GIVEN** an empty directory
- **WHEN** the user runs `rustman init`
- **THEN** `rustman.toml`, `environments/local.yaml` and `examples/ping.rman.yaml` SHALL exist
- **AND** `.gitignore` SHALL contain `.rustman/`

#### Scenario: Bare init
- **GIVEN** an empty directory
- **WHEN** the user runs `rustman init --bare`
- **THEN** only `rustman.toml` and `.gitignore` SHALL exist, and `.gitignore` SHALL contain `.rustman/`

#### Scenario: Existing gitignore is appended
- **GIVEN** a `.gitignore` containing `target/`
- **WHEN** `rustman init` runs
- **THEN** `.gitignore` SHALL contain `target/` and `.rustman/` and nothing else SHALL be removed

#### Scenario: Re-init refused
- **GIVEN** the directory already has `rustman.toml`
- **WHEN** `rustman init` runs
- **THEN** it SHALL exit 2 without modifying any file

### Requirement: Stable serialization
When Rustman writes a request file (save, import), it SHALL write keys in schema order with 2-space indentation, end the file with a trailing newline, and quote strings only when YAML requires it. A file that is loaded and saved without edits SHALL be byte-identical to the original.

#### Scenario: Round trip
- **GIVEN** a request file written by Rustman
- **WHEN** it is loaded and saved without edits
- **THEN** `git diff` SHALL show no changes
