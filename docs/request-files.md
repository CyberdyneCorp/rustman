# Request files

A Rustman workspace is a folder with a `rustman.toml` file. Every saved request is one YAML file named `*.rman.yaml`, and folders group requests into collections. Everything is plain text, meant to be reviewed and versioned in Git.

Every YAML example on this page is loaded by a test (`crates/rustman-core/tests/docs.rs`): the examples are written into a temporary workspace at the path in their first-line comment, and the workspace must load. The examples are also in the exact form Rustman writes, so saving them produces no diff.

## Workspace layout

```text
shop/
├── rustman.toml              workspace marker and settings
├── environments/             one file per environment (see environments.md)
│   └── local.yaml
├── users/                    a collection
│   ├── _collection.yaml      optional: order, parallel, inherited headers, auth and variables
│   ├── login.rman.yaml
│   └── get-user.rman.yaml
└── .rustman/                 per-user state (history, secrets): git-ignored
```

`rustman init [dir]` creates `rustman.toml`, `environments/local.yaml`, a sample `examples/ping.rman.yaml`, and adds `.rustman/` to `.gitignore` (appending to an existing file). `rustman init --bare` creates only `rustman.toml` and the `.gitignore` entry. Running it where `rustman.toml` already exists fails with `error: <dir> is already a rustman workspace` (exit code 2) and changes nothing.

Commands find the workspace by looking for `rustman.toml` in the current directory and then in each parent directory. `--workspace <dir>` overrides the search. Without a workspace, commands that need one fail with `error: no rustman workspace found (run 'rustman init')` (exit code 2).

## rustman.toml

```toml
# rustman.toml
[workspace]
name = "shop"
version = 1

[defaults]
timeout_ms = 30000          # total request timeout
follow_redirects = true
max_redirects = 10
verify_tls = true
default_environment = "local"

[http]
ca_bundle = "certs/dev-ca.pem"   # extra trusted CAs (PEM), relative to the workspace root
proxy = "http://127.0.0.1:3128"
max_body_bytes = 10485760         # larger response bodies are spilled to disk
max_download_bytes = 1073741824   # an undecoded response body larger than this fails
client_cert = "certs/client.pem" # mutual TLS for requests without their own client certificate
client_key = "certs/client.key"
# or: client_pkcs12 = "certs/client.p12" with client_pkcs12_password = "{{p12_password}}"

[variables]
api_version = "v2"
```

Only `[workspace]` with `name` and `version = 1` is required; every other key has the default shown above (`default_environment`, `ca_bundle`, `proxy` and the client certificate keys are unset by default). The `[http]` client certificate (`client_cert` and `client_key`, or `client_pkcs12` and `client_pkcs12_password`) applies to every request that sets none of its own. The optional `[safety]` table (`allowed_hosts`, `production_hosts`, `rate`, `concurrency`) configures the [safety policy](safety.md#safety-in-rustmantoml) for automated senders. Unknown keys produce a warning, not an error (except in `[safety]`, where they are errors). A `version` newer than this build supports fails with `error: rustman.toml version 2 is newer than this rustman supports (1)`.

## Common fields

Every request file has these fields, in this order:

| Field | Required | Meaning |
|-------|----------|---------|
| `version` | yes | Always `1`. |
| `kind` | yes | The request kind: `http`, `graphql` or `mcp`. |
| `name` | yes | The name shown in reports and used by `rustman send <name>`. |
| `description` | no | Free text. |
| `tags` | no | A list of strings. |
| `variables` | no | Request-level variables (see environments.md for precedence). |
| `assertions` | no | Checks on the response (see [assertions.md](assertions.md)). |
| `captures` | no | Values extracted from the response for later requests. |

`graphql` and `mcp` files are parsed and listed, but executing them reports them as skipped with `request kind '<kind>' is not supported by this build` until the GraphQL and MCP client changes land.

## HTTP requests

A minimal HTTP request:

```yaml
# users/get-user.rman.yaml
version: 1
kind: http
name: Get user
method: GET
url: '{{base_url}}/users/1'
```

The `kind: http` fields, in the order Rustman writes them:

| Field | Meaning |
|-------|---------|
| `method` | Any RFC 9110 token, so custom methods such as `PURGE` work. Defaults to `GET`. `GET USERS` is rejected with `invalid HTTP method 'GET USERS'`. |
| `url` | The request URL. `{{variables}}` are resolved at send time. |
| `query` | An ordered list of `name`/`value` pairs. Names may repeat. |
| `headers` | An ordered list of `name`/`value` pairs. |
| `body` | One of the body types below. |
| `auth` | `none`, `basic`, `bearer`, `api_key`, `oauth2`, `digest`, `aws_sigv4` or `jwt`. |
| `settings` | Per-request overrides for timeouts, redirects, TLS and transport. |

Header and query order is kept when the request is sent:

```yaml
# cache/purge.rman.yaml
version: 1
kind: http
name: Purge tagged pages
method: PURGE
url: '{{base_url}}/path'
query:
  - name: tag
    value: a
  - name: tag
    value: b
headers:
  - name: X-Purge-Key
    value: '{{purge_key}}'
```

This is sent as `PURGE /path?tag=a&tag=b`.

A `query`, `headers` or form `fields` row with `enabled: false` stays in the file but is not sent, and a disabled header does not replace a header inherited from a collection. The TUI's per-row toggle writes it; the key is left out for enabled rows.

## Body types

Every body has a `type`. File paths are resolved relative to the request file and must stay inside the workspace (see [File paths](#file-paths)).

### json

`content` is a YAML value converted to JSON, so `{name: Ana, age: 3}` is sent as `{"name":"Ana","age":3}` with `Content-Type: application/json`. Only `true` and `false` are booleans and numbers with a leading zero stay strings, so `no`, `on` and `0755` are sent as the strings `"no"`, `"on"` and `"0755"`. Other numbers follow YAML 1.2, which JSON writes in decimal: `0x1F`, `0o17` and `+12` are sent as `31`, `15` and `12`. An integer too large for 64 bits can't be sent exactly, so it is a load error; quote it, or use a string `content` (below).

```yaml
# users/create-user.rman.yaml
version: 1
kind: http
name: Create user
method: POST
url: '{{base_url}}/users'
body:
  type: json
  content:
    name: Ana
    age: 3
    roles:
      - admin
    active: true
```

A string `content` is sent as is, which is useful for JSON that YAML can't express directly:

```yaml
# users/create-raw.rman.yaml
version: 1
kind: http
name: Create user from raw JSON
method: POST
url: '{{base_url}}/users'
body:
  type: json
  content: '{"name":"Ana","age":3}'
```

### text

Sent with `Content-Type: text/plain; charset=utf-8`. A literal `{{` is written as `\{{`.

```yaml
# notes/create-note.rman.yaml
version: 1
kind: http
name: Create note
method: POST
url: '{{base_url}}/notes'
body:
  type: text
  content: |
    Shopping list for {{user_name}}
    Template syntax looks like \{{this}}
```

### form

URL-encoded fields (`application/x-www-form-urlencoded`), in order:

```yaml
# auth/login-form.rman.yaml
version: 1
kind: http
name: Log in with a form
method: POST
url: '{{base_url}}/login'
body:
  type: form
  fields:
    - name: user
      value: ana
    - name: password
      value: '{{password}}'
```

### multipart

Each part has a `name` and either a text `value` or a `file`. A file part is sent with its file name (`a.png`) and a content type guessed from the extension (`image/png`), unless `content_type` is set.

```yaml
# users/upload-avatar.rman.yaml
version: 1
kind: http
name: Upload avatar
method: POST
url: '{{base_url}}/users/1/avatar'
body:
  type: multipart
  parts:
    - name: caption
      value: Profile picture
    - name: avatar
      file: ./fixtures/a.png
    - name: metadata
      file: ./fixtures/meta.json
      content_type: application/json
```

### file

The raw bytes of a workspace file, sent with `Content-Type: application/octet-stream` unless a header overrides it.

```yaml
# blobs/put-blob.rman.yaml
version: 1
kind: http
name: Upload blob
method: PUT
url: '{{base_url}}/blobs/1'
headers:
  - name: Content-Type
    value: application/pdf
body:
  type: file
  path: ../fixtures/report.pdf
```

### none

No body. This is the same as leaving `body` out, but makes the intent explicit.

```yaml
# users/delete-user.rman.yaml
version: 1
kind: http
name: Delete user
method: DELETE
url: '{{base_url}}/users/{{user_id}}'
body:
  type: none
```

## Auth

```yaml
# auth/basic.rman.yaml
version: 1
kind: http
name: Basic auth
url: '{{base_url}}/me'
auth:
  type: basic
  username: ana
  password: '{{password}}'
```

The other types:

- `{type: bearer, token}` sends `Authorization: Bearer <token>`.
- `{type: api_key, name, value, in}` sends the key as a header (`in: header`, the default) or a query parameter (`in: query`).
- `{type: oauth2, grant, token_url, client_id, ...}` with `grant: client_credentials` or `grant: authorization_code` (which also needs `auth_url`). Optional: `client_secret`, `scopes`.
- `{type: digest, username, password}`, `{type: aws_sigv4, region, service, ...}` and `{type: jwt, algorithm, ...}` compute their credentials for every request; see [Signature auth](auth.md).
- `{type: none}` sends no auth, even when the collection defines some.

Auth never replaces an explicit `Authorization` header.

```yaml
# auth/oauth2.rman.yaml
version: 1
kind: http
name: OAuth2 client credentials
url: '{{base_url}}/me'
auth:
  type: oauth2
  grant: client_credentials
  token_url: https://auth.example.com/token
  client_id: rustman
  client_secret: '{{client_secret}}'
  scopes:
    - read
```

## Settings

Every key is optional and overrides `rustman.toml` for this request:

```yaml
# slow/report.rman.yaml
version: 1
kind: http
name: Slow report
url: https://reports.example.com/build
settings:
  timeout_ms: 120000
  connect_timeout_ms: 5000
  follow_redirects: false
  max_redirects: 3
  verify_tls: true
  http2_prior_knowledge: false
  proxy: none
  cookies: false
  decompress: true
  ca_bundle: ../certs/dev-ca.pem
  client_cert: ../certs/client.pem
  client_key: ../certs/client.key
```

`proxy: none` bypasses every proxy, including `HTTPS_PROXY`. A PKCS#12 client certificate uses `client_pkcs12` plus `client_pkcs12_password`, which should reference a secret variable.

## Assertions and captures

Each assertion is a one-key mapping naming its type. Captures extract values for later requests in the same run. [assertions.md](assertions.md) covers every operator.

```yaml
# users/login.rman.yaml
version: 1
kind: http
name: Log in
method: POST
url: '{{base_url}}/login'
body:
  type: json
  content:
    user: ana
    password: '{{password}}'
assertions:
  - status: 200
  - status: '2xx'
  - header:
      name: content-type
      contains: json
  - json:
      path: $.access_token
      exists: true
  - schema:
      file: ../schemas/token.json
  - body:
      not_contains: error
  - latency:
      total_lt: 500
captures:
  - var: token
    from: json
    path: $.access_token
  - var: session
    from: cookie
    name: sid
    persist: true
```

## Collections

Every folder under the workspace root is a collection, except `environments/`, folders whose name starts with `.` or `_` (such as `.rustman/`, `__snapshots__/` or `_removed/`), and folders with no request file at any depth (such as `mocks/`). Symlinked folders are not followed.

Requests run in file-name order unless the folder has a `_collection.yaml` with `order:`. Entries named there (request file names without `.rman.yaml`, or sub-folder names) run first, in that order, and the rest follow by file name. An `order:` entry that matches nothing produces a warning.

A `_collection.yaml` may also set `name`, `description`, `parallel: true` (under `rustman run --parallel <n>`, the folder's requests run concurrently instead of one after another; see [the CLI reference](cli.md#rustman-run)), `data` and `secret_columns` (the folder's default [data file](data-runs.md) for `rustman run`, and its secret columns), and `headers`, `auth` and `variables` that the requests inside inherit. A request's own value wins. A header set closer to the request replaces every header with the same name (case-insensitive) set further up.

```yaml
# users/_collection.yaml
name: Users
description: User management endpoints
order:
  - login
  - get-user
headers:
  - name: X-Team
    value: core
auth:
  type: bearer
  token: '{{token}}'
variables:
  page_size: 50
```

With this file, `users/get-user.rman.yaml` above is sent with `X-Team: core` and the bearer token, while a request with `auth: {type: none}` is sent without `Authorization`.

A collection created by `rustman import openapi` also records its `source` (`format`, `location` and the `sha256` of the document), which `--sync` uses; see [OpenAPI import](import-openapi.md#import-snapshot).

## File paths

Paths in `body.path`, multipart `file`, `settings.ca_bundle`, `settings.client_cert`, `settings.client_key`, `settings.client_pkcs12` and schema `file` are relative to the request file. The `[http]` file paths in `rustman.toml` (`ca_bundle`, `client_cert`, `client_key`, `client_pkcs12`) are relative to the workspace root. A path that leaves the workspace, through `..`, an absolute path or a symlink, fails to load:

```text
users/upload.rman.yaml:9: file path escapes workspace: ../../etc/passwd
```

Paths containing `{{variables}}` are checked after the variables are resolved, before anything is sent, with the same error (any secret value in the path is redacted from it). `rustman.toml` values are not interpolated, so `{{` there is literal, except `[http] client_pkcs12_password`, which is resolved with the request's variables so that it can reference a secret.

## Errors

Load errors name the file and line, and Rustman reports every broken file at once:

```text
legacy/soap.rman.yaml:2: unknown request kind 'soap' (expected one of: http, graphql, mcp)
users/list.rman.yaml:5: unknown field `header`, expected one of version, kind, name, description, tags, method, url, query, headers, body, auth, settings, variables, assertions, captures
```

Unknown fields, duplicate keys, wrong types, bad indentation and unclosed quotes are all errors with exact positions. Rustman never guesses what a misspelled key meant.

## Stable serialization

When Rustman writes a request file (save or import), keys follow the order in the tables above, indentation is 2 spaces, the file ends with a newline, and strings are quoted only when YAML needs it (for example `'{{base_url}}/users'`, `'2xx'` or `'no'`). Loading and saving a file that Rustman wrote produces no diff. Comments in a hand-edited file aren't preserved when Rustman rewrites it.
