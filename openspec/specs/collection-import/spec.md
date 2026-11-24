# collection-import Specification

## Purpose
Moves existing API collections from Postman, Insomnia, Bruno, HAR captures and `.http` files into a Rustman workspace without retyping, without leaking secrets into tracked files, and with an exact report of what could not be carried over; and exports workspaces back to Postman v2.1 and `.http` for interop.

## Requirements

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
- **THEN** the request SHALL be written with `auth: {type: none}` and the report SHALL list `hawk auth unsupported` with the item path

### Requirement: Unsupported auth disables inheritance
A Postman, Insomnia or Bruno request or folder whose auth type is unsupported SHALL be written with `auth: {type: none}`, so it never inherits a folder or collection credential the source would not have sent.
#### Scenario: Unsupported auth does not inherit the parent credential
- **GIVEN** a collection with bearer auth `{{internal_token}}` and a request using `awsv4` auth
- **WHEN** it is imported and the request is exported to `.http`
- **THEN** the exported request SHALL NOT carry `Authorization: Bearer {{internal_token}}`

### Requirement: Postman environments
`rustman import postman-env <file>` (and auto-detect of a Postman environment export) SHALL create `environments/<slug>.yaml`. Values of variables Postman marks `secret`, and values detected as secrets, SHALL be stored as `environments` secrets, not in the YAML file.

#### Scenario: Secret variable
- **GIVEN** a Postman environment `Staging` with `api_key` of type `secret`
- **WHEN** it is imported
- **THEN** `environments/staging.yaml` SHALL list `api_key` under `secrets` and the value SHALL be stored in the keychain

### Requirement: Scripts are never executed
Postman pre-request and test scripts, and Insomnia and Bruno scripts, SHALL NOT be executed during or after import. Their source SHALL be preserved verbatim in a `# imported-script` comment block in the request file, except that any literal value moved out as a secret SHALL appear as `«redacted:<name>»`, and each SHALL be listed in the report with its location.

#### Scenario: Test script preserved
- **GIVEN** an item whose test script calls `pm.environment.set("token", ...)`
- **WHEN** it is imported
- **THEN** no code SHALL run, the script SHALL appear as a comment block in the file, and the report SHALL list it as `script not executed`

#### Scenario: Secret literal in a preserved script
- **GIVEN** a pre-request script `pm.environment.set('apiSecret', 's3cr3t-value');`
- **WHEN** it is imported
- **THEN** the value SHALL be stored as a secret and the comment block SHALL read `# pm.environment.set('apiSecret', '«redacted:apisecret»');`

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

#### Scenario: Nested environment values
- **GIVEN** an Insomnia environment with `{"auth": {"token": "t-1"}}`
- **WHEN** it is imported
- **THEN** it SHALL become the variable `auth.token`, `{{ _.auth.token }}` SHALL become `{{auth.token}}`, and `t-1` SHALL be stored as a secret, never written to a tracked file

#### Scenario: Private environment
- **GIVEN** an Insomnia environment with `isPrivate: true` and `data: {plain: v-1}`
- **WHEN** it is imported
- **THEN** `plain` SHALL be listed under `secrets` without a value, `v-1` SHALL be stored as a secret, and no tracked file SHALL contain `v-1`

#### Scenario: Unsupported template tag
- **GIVEN** a header value `{% response 'body', 'req_1', '$.id' %}`
- **WHEN** it is imported
- **THEN** the report SHALL list `unsupported template tag: response` with the request name

### Requirement: Bruno collections
A Bruno collection folder SHALL import each `.bru` file (meta, method block, headers, query, body, auth, `vars:pre-request`, `assert` block) as a request file, `bruno.json` ordering (`meta.seq`) into `_collection.yaml`, and `environments/*.bru` into `environments/*.yaml`. Bruno `assert` entries with `eq`, `neq`, `gt`, `lt`, `contains` and `isDefined` on `res.status` or `res.body.*` SHALL map to declarative assertions.

#### Scenario: Symbolic link loop
- **GIVEN** a Bruno folder with a symbolic link to its parent folder
- **WHEN** it is imported
- **THEN** the link SHALL NOT be followed, the import SHALL finish, and the report SHALL list `symlinked folder <path> skipped`

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

### Requirement: Imported secret references are neutralized
A `{{secret:...}}` reference in an imported source, or one a translation produces (`{{ _.secret:... }}`), SHALL be written escaped as `\{{secret:...}}`, literal text that never resolves a provider secret, in request, collection and environment files and in assertion values. The report SHALL list each one as a `review` note. Plain `{{name}}` placeholders and references the source already escaped SHALL stay as written.

#### Scenario: Planted reference in a Postman header
- **GIVEN** a Postman request with header `X-Db: {{secret:vault/kv/prod/db#password}}` and URL `{{base_url}}/leak`
- **WHEN** it is imported
- **THEN** the request file SHALL hold `X-Db: \{{secret:vault/kv/prod/db#password}}`, the URL SHALL stay `{{base_url}}/leak`, and the report SHALL have a `review` note naming the neutralized reference

#### Scenario: Planted reference in an OAuth2 scope or GraphQL variable
- **GIVEN** a Postman request whose OAuth2 `scope` or GraphQL variable `pw` is `{{secret:vault/kv/prod/db#password}}`
- **WHEN** it is imported
- **THEN** the scope or variable SHALL be written as `\{{secret:vault/kv/prod/db#password}}`, the GraphQL variables SHALL stay structured, and the report SHALL have a `review` note

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

### Requirement: Value-based secret detection
Any variable or header value starting with `Bearer `, `Basic ` or `Digest ` (case-insensitive), and any header whose name ends in `-auth`, `-token`, `-key`, `-secret` or `-signature` (case-insensitive), SHALL be treated as a secret, whatever the name patterns say.

#### Scenario: Credential-shaped header
- **GIVEN** headers `X-Custom-Auth: ca-1` and `X-Upstream: Bearer up-1`
- **WHEN** they are imported
- **THEN** they SHALL be written as `{{x_custom_auth}}` and `Bearer {{x_upstream}}`, and `ca-1` and `up-1` SHALL be stored as secrets

### Requirement: Scoped variable renames
When a secret folder variable is moved under another name, references SHALL be rewritten only inside the declaring folder's subtree, except below a subfolder or request that declares the same name again.

#### Scenario: Folder override of a collection variable
- **GIVEN** a Bruno collection variable `password=A`, folder `F` variable `password=B`, a root request and a request in sibling folder `G` using `{{password}}`
- **WHEN** it is imported
- **THEN** the root and `G` requests SHALL resolve to `A` and the request in `F` SHALL resolve to `B`

### Requirement: Token scan
Every text written to a tracked file (URLs, headers, bodies, auth, variables, names, descriptions, script literals) SHALL be scanned, as written and percent-decoded, for GitHub, GitLab, Slack, Stripe live, AWS, Google API key, JWT and PEM private-key tokens, which SHALL be moved.

#### Scenario: Token in a description
- **GIVEN** a collection description `Use key ghp_...`
- **WHEN** it is imported
- **THEN** the description SHALL read `Use key «redacted:github_token»` and the token SHALL be stored as a secret

### Requirement: Repeat replacement
A raw repeat of a secret-like collection-scoped value (8+ characters and two character classes or 16+ characters) SHALL become `{{name}}` only when it fills the field or sits on token boundaries (token shapes: anywhere). Encoded or in-word repeats, and every repeat of an environment-scoped secret in a request, SHALL become `«redacted:name»` with a report note. Other repeats SHALL be left unchanged and reported.

#### Scenario: Repeated secret-like value
- **GIVEN** `X-Api-Key: SAME-VAL-42` and the same value in another header and in the URL path
- **WHEN** it is imported
- **THEN** every occurrence SHALL be `{{x_api_key}}`

#### Scenario: Encoded repeat
- **GIVEN** a secret `Pa"ss&w0rd+1` that also appears JSON-escaped in a body and percent-encoded in a query
- **WHEN** it is imported
- **THEN** both SHALL become `«redacted:password»`, the report SHALL say the field contained secret `password` in encoded form, and no file SHALL hold the value in any form

#### Scenario: Environment secret repeated in a request
- **GIVEN** environment `Prod` with secret `token` and a request header with the same value
- **WHEN** it is imported
- **THEN** the header SHALL be `«redacted:token»` and the report SHALL say the field repeated secret `token` of environment `prod`

#### Scenario: Ordinary word as a secret
- **GIVEN** a Basic password `password` and a body `{"grant_type":"password"}` and URL `/password/reset?type=password`
- **WHEN** it is imported
- **THEN** the body and URL SHALL be unchanged and the report SHALL note that the value of secret `password` also appears there

### Requirement: Decoded repeats
After repeat replacement, each field SHALL be read percent-decoded (`+` as a space), JSON-unescaped and HTML-unescaped; when a reading shows a secret-like moved value, the whole field SHALL become `«redacted:name»` with a report note.

#### Scenario: Partly encoded repeat
- **GIVEN** a secret `p@ss&w0rd=1` and a raw body `note=p%40ss&w0rd=1`
- **WHEN** it is imported
- **THEN** the body SHALL be `«redacted:password»` and the report SHALL say so

### Requirement: Environment file repeats
In an environment file, a raw secret-like repeat of that environment's own secret, whole or on token boundaries, SHALL become `{{name}}`; any other secret-like repeat SHALL become `«redacted:name»` with a report note.

#### Scenario: Secret inside a URL variable
- **GIVEN** environment `Prod` with `token: Tok3n-ABCDEF12` and `cb_url: https://x.test/cb?t=Tok3n-ABCDEF12`
- **WHEN** it is imported
- **THEN** `environments/prod.yaml` SHALL hold `cb_url: https://x.test/cb?t={{token}}`

### Requirement: Fresh secret names
A new secret name SHALL NOT be the name of any variable declared or referenced as `{{name}}` in the source, other than the variable being moved.

#### Scenario: Undeclared reference
- **GIVEN** a request header `Authorization: Bearer {{token}}` with no `token` declared, and another request's literal bearer token
- **WHEN** it is imported
- **THEN** the literal SHALL be stored as `token_2`, never as `token`

#### Scenario: Plain variable with the suffixed name
- **GIVEN** collection variables `token=A` and `token_2={{env_tok}}`, and folder variable `token=B`
- **WHEN** it is imported
- **THEN** the folder's references SHALL resolve to `B`, not to `{{env_tok}}`

### Requirement: Path variable namespace
Postman and Bruno path variables SHALL NOT shadow a variable of the same name: when the name is used elsewhere, the path variable SHALL be renamed (`path_<name>`).

#### Scenario: Path variable named like the collection's token
- **GIVEN** a collection variable `token`, bearer auth `{{token}}` and a request URL `/:token/x`
- **WHEN** it is imported
- **THEN** the URL SHALL be `/{{path_token}}/x` and the request SHALL still send the collection's `{{token}}`

### Requirement: One import at a time
An import SHALL hold an exclusive lock on `.rustman/import.lock` from reading the workspace to writing its last file; a concurrent import SHALL wait for it.

#### Scenario: Two imports at once
- **GIVEN** two imports into one workspace that both add secrets to `environments/local.yaml`
- **WHEN** they run at the same time
- **THEN** `environments/local.yaml` SHALL list the secrets of both

### Requirement: Postman folder variables
Postman folder `variable` lists SHALL be dropped with a report note, because Postman does not resolve them.

#### Scenario: Folder variable overriding a collection variable
- **GIVEN** collection variable `password=A` and folder `F` variable `password=B`
- **WHEN** a request in `F` using `{{password}}` is imported
- **THEN** it SHALL resolve to `A`, and the report SHALL list `folder variable password dropped`

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

#### Scenario: Path length limit
- **GIVEN** a source whose folder names and nesting would give a path over 180 characters or 20 levels below the workspace root
- **WHEN** it is imported
- **THEN** the import SHALL fail before writing anything with `generated path too long`, showing the path shortened, never in full

#### Scenario: Reproducible output
- **GIVEN** the same Postman file imported into two empty workspaces
- **WHEN** the trees are compared
- **THEN** they SHALL be byte-identical

### Requirement: Re-import conflict policy
When a target file already exists, `--on-conflict` SHALL decide: `skip` (default: keep the existing file), `overwrite` (replace it), or `rename` (write with the next free suffix, appending renamed requests to the existing `_collection.yaml` `order:`). Every conflict and its resolution SHALL appear in the report. A file whose content would not change SHALL be reported as `unchanged`, under every policy, and SHALL NOT be reported as a conflict.

#### Scenario: Default skip
- **GIVEN** `billing-api/list-invoices.rman.yaml` already exists and was edited
- **WHEN** the same collection is imported again
- **THEN** the existing file SHALL be unchanged and the report SHALL list it as `skipped (exists)`

#### Scenario: Overwrite
- **WHEN** the import runs with `--on-conflict overwrite`
- **THEN** the file SHALL be replaced with the freshly converted request

#### Scenario: Unchanged file under rename
- **GIVEN** `billing-api/users/create.rman.yaml` exists with exactly the content the import would write
- **WHEN** the collection is imported with `--on-conflict rename`
- **THEN** the file SHALL be listed as `unchanged`, no `create-2.rman.yaml` SHALL be written, and no conflict SHALL be reported for it

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

### Requirement: Extended script-to-assertion mapping
On top of the existing mappings, the importer SHALL map:
- `pm.response.to.be.ok` → `status: "2xx"`;
- `pm.expect(pm.response.responseTime).to.be.below(N)` → `latency: {total_lt: N}`;
- `pm.expect(pm.response.text()).to.include("s")` → `body: {contains: "s"}`;
- `pm.response.to.have.header("H", "v")` → `header: {name: H, equals: v}`.

Statements outside the mapping table SHALL remain comment-only.

#### Scenario: Response time check
- **GIVEN** `pm.test("fast", () => pm.expect(pm.response.responseTime).to.be.below(300));`
- **WHEN** it is imported
- **THEN** the request file SHALL contain `latency: {total_lt: 300}`

#### Scenario: Header value check
- **GIVEN** `pm.response.to.have.header("Content-Type", "application/json");`
- **WHEN** it is imported
- **THEN** the request file SHALL contain `header: {name: Content-Type, equals: application/json}`

### Requirement: JSON body field mapping
The importer SHALL track `const|let|var <name> = pm.response.json()` and map `pm.expect(<name>.a.b).to.eql(v)` and `.to.equal(v)` to `json: {path: "$.a.b", equals: v}`. It SHALL also map `.to.exist` to `exists: true`, `.to.have.lengthOf(n)` to `length: n`, `.to.be.a("t")` to `type: t`, and `.to.be.above(n)` and `.to.be.below(n)` to `gt` and `lt`.

#### Scenario: Field equality
- **GIVEN** `const body = pm.response.json(); pm.expect(body.user.id).to.eql(42);`
- **WHEN** it is imported
- **THEN** the request file SHALL contain `json: {path: "$.user.id", equals: 42}`

#### Scenario: Array index and length
- **GIVEN** `const d = pm.response.json(); pm.expect(d.items[0].tags).to.have.lengthOf(2);`
- **WHEN** it is imported
- **THEN** the request file SHALL contain `json: {path: "$.items[0].tags", length: 2}`

### Requirement: Variable setter mapping
`pm.environment.set`, `pm.collectionVariables.set`, `pm.globals.set` and `pm.variables.set` SHALL map to captures when the value is `pm.response.json()<path>`, a tracked JSON variable path, `pm.response.headers.get("H")` or `pm.cookies.get("c")`. Environment and global setters SHALL be marked `persist: true`.

#### Scenario: Header captured
- **GIVEN** `pm.environment.set("etag", pm.response.headers.get("ETag"));`
- **WHEN** it is imported
- **THEN** the request file SHALL contain the capture `{var: etag, from: header, name: ETag, persist: true}`

#### Scenario: Collection variable from tracked body
- **GIVEN** `const b = pm.response.json(); pm.collectionVariables.set("orderId", b.order.id);`
- **WHEN** it is imported
- **THEN** the request file SHALL contain `{var: orderId, from: json, path: "$.order.id"}` without `persist`

### Requirement: Migration summary
The import report SHALL include, per request and in total, the number of script statements found, mapped and left unconverted, plus the overall percentage converted. The pretty summary SHALL print `scripts: <mapped>/<found> statements converted (<pct>%)`.

#### Scenario: Summary line
- **GIVEN** a collection with 40 script statements, 31 of them mappable
- **WHEN** it is imported
- **THEN** the summary SHALL print `scripts: 31/40 statements converted (77%)`

### Requirement: Migration report
`rustman import <format> <file> --migration-report <file.md>` SHALL write a Markdown report with one section per request holding unconverted statements. Each entry SHALL show the original statement, the request file path, and either a suggested manual equivalent or `no declarative equivalent; consider a plugin`. Secret values SHALL be redacted.

#### Scenario: Suggested equivalent
- **GIVEN** an unconverted statement `pm.expect(pm.response.json().items.every(i => i.price > 0)).to.be.true;`
- **WHEN** the migration report is written
- **THEN** its entry SHALL suggest a `schema` assertion with `items.price` constrained by `exclusiveMinimum: 0`

#### Scenario: No equivalent
- **GIVEN** an unconverted statement `postman.setNextRequest("checkout");`
- **WHEN** the migration report is written
- **THEN** its entry SHALL read `no declarative equivalent; consider a plugin`, and also point to collection ordering in `_collection.yaml`
