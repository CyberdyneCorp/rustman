# Spec Delta

## Purpose

Moves existing API collections from Postman, Insomnia, Bruno, HAR captures and `.http` files into a Rustman workspace without retyping, without leaking secrets into tracked files, and with an exact report of what could not be carried over; and exports workspaces back to Postman v2.1 and `.http` for interop.

## ADDED Requirements

### Requirement: Import command and format detection
`rustman import <format> <src>` SHALL accept `postman`, `insomnia`, `bruno`, `har`, `http` or `auto` (the default when the format is omitted). `auto` SHALL detect the format from content, not the file extension. An undetectable source SHALL fail with exit code 2 and `import: cannot detect format of <src> (expected postman, insomnia, bruno, har, http)`.

#### Scenario: Auto-detect Postman
- **GIVEN** a file `export.json` whose `info.schema` is `https://schema.getpostman.com/json/collection/v2.1.0/collection.json`
- **WHEN** the user runs `rustman import export.json`
- **THEN** it SHALL be imported as a Postman v2.1 collection

#### Scenario: Unknown content
- **GIVEN** a JSON file with no recognizable markers
- **WHEN** the user runs `rustman import data.json`
- **THEN** the command SHALL exit with code 2 and print the cannot-detect message

### Requirement: Import destination
Imported files SHALL be written under `--into <dir>` (default: a new collection folder at the workspace root named after the source collection's slug). The destination SHALL be inside the workspace. Running outside a workspace SHALL fail with exit code 2 and suggest `rustman init`.

#### Scenario: Default destination
- **GIVEN** a Postman collection named `Billing API`
- **WHEN** it is imported without `--into`
- **THEN** request files SHALL be written under `billing-api/`

#### Scenario: Destination outside workspace
- **WHEN** the user passes `--into ../elsewhere`
- **THEN** the command SHALL exit with code 2 and `import: --into must be inside the workspace`

### Requirement: Dry run
`--dry-run` SHALL perform the full conversion and print the planned file tree, the secrets that would be moved and the import report, without writing any file or keychain entry.

#### Scenario: Preview only
- **GIVEN** a collection with 12 requests
- **WHEN** the user runs `rustman import postman c.json --dry-run`
- **THEN** the 12 planned file paths SHALL be printed and the workspace SHALL be unchanged

### Requirement: Postman collections
Postman Collection v2.0 and v2.1 SHALL import folders as nested collections, items as `kind: http` request files (method, URL, query, headers, body), item order into `_collection.yaml` `order:`, and collection- and folder-level auth and variables into `_collection.yaml`. Disabled headers and query params SHALL be dropped and listed in the report.

#### Scenario: Nested folders keep order
- **GIVEN** a collection with folder `Users` containing `List`, `Create`, `Delete` in that order
- **WHEN** it is imported
- **THEN** `users/_collection.yaml` SHALL contain `order: [list, create, delete]`

#### Scenario: Disabled header
- **GIVEN** a request header `X-Debug` with `disabled: true`
- **WHEN** it is imported
- **THEN** the request file SHALL NOT contain `X-Debug` and the report SHALL list it as dropped

### Requirement: Postman body modes and auth
Body modes SHALL map as `raw` (JSON → `json`, otherwise `text`), `urlencoded` → `form`, `formdata` → `multipart` (file parts keep their path, made relative to the workspace when possible), `file` → `file`, `graphql` → a `kind: graphql` file. Auth `basic`, `bearer`, `apikey` and `oauth2` (client credentials, authorization code) SHALL map to `http-requests` auth; other types SHALL be reported as unsupported.

#### Scenario: GraphQL body
- **GIVEN** a Postman request with body mode `graphql`, a query and variables
- **WHEN** it is imported
- **THEN** a `kind: graphql` file SHALL be written with the endpoint, query and variables

#### Scenario: Unsupported auth
- **GIVEN** a request using Postman `hawk` auth
- **WHEN** it is imported
- **THEN** the request SHALL be written without auth and the report SHALL list `hawk auth unsupported` with the item path

### Requirement: Postman environments
`rustman import postman-env <file>` (and auto-detect of a Postman environment export) SHALL create `environments/<slug>.yaml`. Values of variables Postman marks `secret`, and values detected as secrets, SHALL be stored as `environments` secrets, not in the YAML file.

#### Scenario: Secret variable
- **GIVEN** a Postman environment `Staging` with `api_key` of type `secret`
- **WHEN** it is imported
- **THEN** `environments/staging.yaml` SHALL list `api_key` under `secrets` and the value SHALL be stored in the keychain

### Requirement: Scripts are never executed
Postman pre-request and test scripts, and Insomnia and Bruno scripts, SHALL NOT be executed during or after import. Their source SHALL be preserved verbatim in a `# imported-script` comment block in the request file, and each SHALL be listed in the report with its location.

#### Scenario: Test script preserved
- **GIVEN** an item whose test script calls `pm.environment.set("token", ...)`
- **WHEN** it is imported
- **THEN** no code SHALL run, the script SHALL appear as a comment block in the file, and the report SHALL list it as `script not executed`

### Requirement: Script-to-assertion suggestions
For trivially mappable script statements, the importer SHALL write an equivalent declarative entry: `pm.response.to.have.status(N)` and `pm.expect(pm.response.code).to.eql(N)` → `status: N`; `pm.response.to.have.header("H")` → `header: {name: H, exists: true}`; `pm.environment.set("v", pm.response.json().a.b)` → a `json` capture of `$.a.b` into `v`. Everything else SHALL remain comment-only.

#### Scenario: Status check mapped
- **GIVEN** a test script `pm.test("ok", () => pm.response.to.have.status(201));`
- **WHEN** it is imported
- **THEN** the request file SHALL contain the assertion `status: 201`

#### Scenario: Token capture mapped
- **GIVEN** `pm.environment.set("token", pm.response.json().data.token);`
- **WHEN** it is imported
- **THEN** the request file SHALL contain a capture `{var: token, from: json, path: "$.data.token"}`

### Requirement: Insomnia exports
Insomnia v4 JSON exports and v5 YAML exports SHALL import request groups as collections, requests (including GraphQL) as request files, and environments (base and sub-environments flattened per sub-environment) as `environments/*.yaml`. Template tags other than plain variables SHALL be reported as unsupported with their location.

#### Scenario: Sub-environment flattening
- **GIVEN** an Insomnia base environment with `host` and sub-environment `Prod` overriding `host`
- **WHEN** it is imported
- **THEN** `environments/prod.yaml` SHALL contain the overridden `host`

#### Scenario: Unsupported template tag
- **GIVEN** a header value `{% response 'body', 'req_1', '$.id' %}`
- **WHEN** it is imported
- **THEN** the report SHALL list `unsupported template tag: response` with the request name

### Requirement: Bruno collections
A Bruno collection folder SHALL import each `.bru` file (meta, method block, headers, query, body, auth, `vars:pre-request`, `assert` block) as a request file, `bruno.json` ordering (`meta.seq`) into `_collection.yaml`, and `environments/*.bru` into `environments/*.yaml`. Bruno `assert` entries with `eq`, `neq`, `gt`, `lt`, `contains` and `isDefined` on `res.status` or `res.body.*` SHALL map to declarative assertions.

#### Scenario: Bruno assert block
- **GIVEN** a `.bru` file with `assert { res.status: eq 200 }`
- **WHEN** it is imported
- **THEN** the request file SHALL contain `status: 200`

### Requirement: HAR captures
HAR 1.2 files SHALL import each entry as a request file grouped into collections by host. Identical method + URL + body entries SHALL be deduplicated. Static assets (images, fonts, stylesheets, scripts, by MIME type or extension) SHALL be dropped unless `--include-static`. `--host`, `--method` and `--content-type` filters SHALL be repeatable.

#### Scenario: Static assets dropped
- **GIVEN** a HAR with 40 entries, of which 30 are `.png`, `.css` and `.js`
- **WHEN** it is imported without `--include-static`
- **THEN** 10 request files at most SHALL be written and the report SHALL count 30 dropped static entries

#### Scenario: Host filter
- **GIVEN** a HAR with entries for `api.example.com` and `cdn.example.com`
- **WHEN** it is imported with `--host api.example.com`
- **THEN** only `api.example.com` requests SHALL be written

### Requirement: HAR transport header hygiene
HAR imports SHALL drop transport and browser-managed headers (`Host`, `Content-Length`, `Connection`, `Transfer-Encoding`, `Accept-Encoding`, HTTP/2 pseudo-headers, `sec-*`) from request files. `Cookie` headers SHALL be treated as secrets.

#### Scenario: Pseudo-headers dropped
- **GIVEN** a HAR entry with headers `:authority`, `Content-Length` and `Accept`
- **WHEN** it is imported
- **THEN** the request file SHALL keep only `Accept`

### Requirement: HTTP files
`.http` and `.rest` files SHALL import requests separated by `###` lines, using the request line, headers and body after a blank line. `@name = value` definitions SHALL become collection variables. A sibling `http-client.env.json` (JetBrains) or `.vscode/settings.json` `rest-client.environmentVariables` SHALL become environments, with `http-client.private.env.json` values stored as secrets.

#### Scenario: Two requests
- **GIVEN** a file with `GET {{host}}/a`, a `###` line, then `POST {{host}}/b` with a JSON body
- **WHEN** it is imported
- **THEN** two request files SHALL be written, the second with a `json` body

#### Scenario: Private env file
- **GIVEN** `http-client.private.env.json` defines `dev.token`
- **WHEN** the folder is imported
- **THEN** `token` SHALL be a secret of environment `dev`

### Requirement: Variable syntax translation
Source variable references SHALL be translated to `{{name}}`: Postman/Insomnia/Bruno `{{name}}` stay as-is, Insomnia `{{ _.name }}` becomes `{{name}}`, and dynamic variables map as `{{$guid}}`/`{{$randomUUID}}` → `{{$uuid}}`, `{{$timestamp}}` → `{{$timestamp}}`, `{{$randomInt}}` → `{{$randomInt}}`. Unmappable dynamic variables SHALL be left unchanged and reported.

#### Scenario: Insomnia variable
- **GIVEN** an Insomnia URL `{{ _.base_url }}/users`
- **WHEN** it is imported
- **THEN** the request URL SHALL be `{{base_url}}/users`

#### Scenario: Unmappable dynamic variable
- **GIVEN** a Postman body using `{{$randomCity}}`
- **WHEN** it is imported
- **THEN** the placeholder SHALL be kept and the report SHALL list `unmapped dynamic variable $randomCity`

### Requirement: Secret extraction
Before writing, the importer SHALL detect secrets: `Authorization`, `Proxy-Authorization`, `Cookie` and API-key header or query values; basic, bearer, API-key and OAuth2 credential fields; and variables whose name matches `token|secret|password|passwd|api[_-]?key|client[_-]?secret` (case-insensitive). Literal secret values SHALL be replaced by `{{name}}` placeholders and stored as `environments` secrets.

#### Scenario: Literal bearer token
- **GIVEN** a Postman request with header `Authorization: Bearer eyJhbGciOi...`
- **WHEN** it is imported into environment `local`
- **THEN** the request file SHALL contain `Authorization: Bearer {{authorization_token}}`, the value SHALL be stored as a secret, and no request file SHALL contain `eyJhbGciOi`

#### Scenario: No environment selected
- **GIVEN** no active environment and `--env` not passed
- **WHEN** a secret is extracted
- **THEN** it SHALL be stored in a new environment named after the source collection and the report SHALL say so

### Requirement: Secret report
The import summary and report SHALL list every moved secret by variable name, source location and target environment, never by value. `--keep-secrets-inline` SHALL NOT exist; the only way to put a literal secret into a file is to edit it by hand.

#### Scenario: Report lists names only
- **GIVEN** an import that moved `api_key` and `password`
- **WHEN** the report is printed
- **THEN** it SHALL show both names and locations and SHALL NOT contain either value

### Requirement: Deterministic file naming
File names SHALL be the slug of the item name (lowercase ASCII, `-` separators, max 64 chars) plus `.rman.yaml`. Two items with the same slug in one folder SHALL get `-2`, `-3` suffixes in source order. Files SHALL be written with the `workspace` deterministic emitter, so importing the same source twice into empty folders SHALL produce byte-identical trees.

#### Scenario: Slug collision
- **GIVEN** two requests named `Get User` and `get user!` in the same folder
- **WHEN** they are imported
- **THEN** the files SHALL be `get-user.rman.yaml` and `get-user-2.rman.yaml`

#### Scenario: Reproducible output
- **GIVEN** the same Postman file imported into two empty workspaces
- **WHEN** the trees are compared
- **THEN** they SHALL be byte-identical

### Requirement: Re-import conflict policy
When a target file already exists, `--on-conflict` SHALL decide: `skip` (default: keep the existing file), `overwrite` (replace it), or `rename` (write with the next free suffix). Every conflict and its resolution SHALL appear in the report.

#### Scenario: Default skip
- **GIVEN** `billing-api/list-invoices.rman.yaml` already exists and was edited
- **WHEN** the same collection is imported again
- **THEN** the existing file SHALL be unchanged and the report SHALL list it as `skipped (exists)`

#### Scenario: Overwrite
- **WHEN** the import runs with `--on-conflict overwrite`
- **THEN** the file SHALL be replaced with the freshly converted request

### Requirement: Import report
Every import SHALL print a summary (requests, collections, environments written; skipped; dropped; secrets moved; unsupported features). `--report <file>` SHALL write the same data as JSON with schema `rustman.import-report/v1`, where each unsupported feature has a kind, message and source location (file and item path or line).

#### Scenario: JSON report
- **WHEN** the user runs `rustman import c.json --report out/import.json`
- **THEN** `out/import.json` SHALL contain `"schema": "rustman.import-report/v1"` and the counts SHALL match the summary

### Requirement: Import exit codes
Import SHALL exit `0` when everything was converted, `1` when it completed but some features were unsupported, dropped or unmapped (strict mode via `--fail-on-unsupported` only), and `2` for unreadable, invalid or undetectable sources. Without `--fail-on-unsupported`, partial conversions SHALL exit `0`.

#### Scenario: Partial conversion is not an error by default
- **GIVEN** a collection with one unsupported auth type
- **WHEN** it is imported
- **THEN** the exit code SHALL be 0 and the report SHALL list the unsupported item

#### Scenario: Strict mode
- **WHEN** the same import runs with `--fail-on-unsupported`
- **THEN** the exit code SHALL be 1

### Requirement: Import size limits
Sources larger than 100 MiB (configurable with `--max-size`) SHALL be refused with exit code 2. HAR response bodies SHALL NOT be stored in request files.

#### Scenario: Oversized HAR
- **GIVEN** a 300 MiB HAR file
- **WHEN** it is imported without `--max-size`
- **THEN** the command SHALL exit with code 2 and `import: source exceeds 100 MiB (use --max-size)`

### Requirement: Export to Postman and HTTP files
`rustman export postman <path> [-o <file>]` SHALL write a Postman Collection v2.1 JSON, and `rustman export http <path> [-o <file>]` a `.http` file, for a request file or collection folder. Secrets SHALL be exported as `{{name}}` placeholders, never as values. Features with no target equivalent (e.g. declarative assertions in `.http`) SHALL be listed as warnings.

#### Scenario: Round trip through Postman
- **GIVEN** a collection imported from Postman with no unsupported features
- **WHEN** it is exported with `rustman export postman` and re-imported into an empty workspace
- **THEN** the request files SHALL be byte-identical to the first import

#### Scenario: Secrets stay placeholders
- **GIVEN** a request whose bearer token is the secret `token`
- **WHEN** it is exported to `.http`
- **THEN** the output SHALL contain `Authorization: Bearer {{token}}` and no secret value
