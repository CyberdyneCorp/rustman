# Importing collections

`rustman import` turns collections exported from other tools into request files, `_collection.yaml` files and environments in the current workspace. It never writes a secret into a workspace file, never runs a script, and reports exactly what it could not carry over.

This build reads Postman Collection v2.0 and v2.1 exports and Postman environment exports, Insomnia v4 (JSON) and v5 (YAML) exports, Bruno collection folders, HAR 1.2 captures and `.http` / `.rest` files. Single curl commands have their own command, [`rustman import curl`](http.md#import), and so do OpenAPI documents, [`rustman import openapi`](import-openapi.md), which is not detected by `rustman import <src>`.

```text
rustman import <src>                    # detect the format from the content
rustman import auto <src>               # the same
rustman import postman <src>            # a Postman Collection v2.0 / v2.1 export
rustman import postman-env <src>        # a Postman environment export
rustman import insomnia <src>           # an Insomnia v4 JSON or v5 YAML export
rustman import bruno <dir>              # a Bruno collection folder (or one .bru file)
rustman import har <src> [filters]      # a HAR 1.2 capture
rustman import http <src>               # a .http / .rest file, or a folder of them
```

Options (the same for every form):

| Flag | Effect |
|------|--------|
| `--into <dir>` | The collection folder to write into. It must be inside the workspace. Default: a folder at the workspace root named after the slug of the source collection (`Billing API` → `billing-api/`). |
| `--dry-run` | Convert everything and print the planned files, the secrets that would be moved and the report, without writing any workspace file or keychain entry. |
| `--on-conflict skip\|overwrite\|rename` | What to do with a file that already exists (default `skip`). See [Re-importing](#re-importing). |
| `--report <file>` | Also write the report as JSON (`rustman.import-report/v1`). Parent folders are created. A dry run writes this file and the migration report, nothing else. |
| `--migration-report <file.md>` | Also write the [script migration report](#script-migration-report) as Markdown. Parent folders are created. |
| `--max-size <size>` | Refuse larger sources (default `100M`; bytes, or with `K`, `M` or `G`). For a folder, every file read counts toward the limit. |
| `--fail-on-unsupported` | Exit with 1 when a feature was unsupported, dropped or unmapped. |
| `--env <name>` | The environment that receives secrets moved out of requests (default: `RUSTMAN_ENV`, then `default_environment`). |
| `--workspace <dir>` | The workspace (default: search upwards from the current directory). |

Running outside a workspace fails with `no rustman workspace found (run 'rustman init')`; `--into ../elsewhere` fails with `import: --into must be inside the workspace`. Both exit with 2.

## Format detection

`auto` (and a bare `rustman import <src>`) looks at the content, never the file name:

| Source | Recognized by |
|--------|---------------|
| Postman collection | `info.schema` (`https://schema.getpostman.com/json/collection/v2.1.0/collection.json`, or v2.0.0) |
| Postman environment | `_postman_variable_scope: environment` |
| Insomnia v4 | `_type: export` and `__export_format: 4` |
| Insomnia v5 | a top-level `type: collection.insomnia.rest/5.x` line |
| HAR | a JSON object with `log.entries` |
| Bruno | a folder with `bruno.json`; a single file with a `meta {` block and a request block |
| HTTP file | a request line such as `GET https://...` (not JSON); a folder holding `.http` or `.rest` files |

Anything else fails with exit code 2:

```text
error: import: cannot detect format of data.json (expected postman, insomnia, bruno, har, http)
```

## Postman collections

| Postman | Rustman |
|---------|---------|
| Folders | Nested folders (collections), each with a `_collection.yaml` holding its `name` and the item order as `order:` |
| Items | `kind: http` request files: method, URL, query, headers, body, auth |
| Collection and folder auth | `auth` in `_collection.yaml`, inherited by the requests inside |
| Collection variables | `variables` in the root `_collection.yaml` (secret ones are moved, see below) |
| Folder `variable` lists | Dropped, each reported as `folder variable <name> dropped: Postman ignores folder-level variables`: the v2.1 schema allows them, but Postman never resolves them (a request in the folder gets the collection's value), so importing them would change which value is sent |
| Path variables (`/users/:id`) | `/users/{{id}}`, with `id` under the request's `variables`. Path variables are a namespace of their own in Postman: when the name is also used elsewhere (a collection variable `{{id}}`, an auth field), the path variable is renamed (`path_id`, `path_id_2`, ...) so it never shadows the other |
| Disabled headers, query parameters, form fields and variables | Dropped, and listed in the report |
| `noauth` on a request | `auth: {type: none}` (turns inherited auth off) |
| `inherit` on a request or folder | No `auth` (inherits the parent's auth) |

Body modes:

| Postman body mode | Rustman body |
|-------------------|--------------|
| `raw`, language JSON (or no language and a JSON `Content-Type`) | `type: json`, with the text kept byte for byte, so unquoted placeholders such as `{{$timestamp}}` survive |
| `raw`, any other language | `type: text` |
| `urlencoded` | `type: form` |
| `formdata` | `type: multipart`; file parts keep their path, made relative to the request file when it is inside the workspace. A path outside the workspace keeps only its file name, with a `review` note. |
| `file` | `type: file` |
| `graphql` | A `kind: graphql` request file with `endpoint`, `document` and `variables`. Variables that are not a flat JSON object are kept as a `# graphql-variables:` comment, with a `review` note. |

Auth types `basic`, `bearer`, `apikey` (header or query) and `oauth2` with the client-credentials or authorization-code grant (with or without PKCE) map to the [auth helpers](http.md). Any other type (`hawk`, `digest`, `awsv4`, `ntlm`, OAuth2 `implicit` or `password` grants, ...) is written as `auth: {type: none}` and reported as `<type> auth unsupported` with the item path. Writing `none` rather than leaving `auth` out matters: a request without `auth` inherits the folder or collection auth, and Postman would never have sent that parent credential to this host. A token Postman cached for OAuth2 is never carried over: Rustman fetches its own.

## Postman environments

`rustman import postman-env staging.postman_environment.json` (or auto-detection) writes `environments/<slug>.yaml`. Variables Postman marks `secret`, and variables whose name looks like a secret, are listed under `secrets`; their values go to the secret store, never into the file:

```yaml
# environments/staging.yaml
variables:
  base_url: https://staging.billing.example.com/v1
secrets:
  - api_key
```

An existing environment file is merged: new variables and secrets are added, and existing ones follow the conflict policy.

## Insomnia exports

`rustman import insomnia export.json` reads an Insomnia v4 JSON export (Preferences → Data → Export, "Insomnia v4"); `export.yaml` in the v5 YAML format of recent versions is read the same way.

| Insomnia | Rustman |
|----------|---------|
| Workspace / collection | The collection folder (`_collection.yaml` with its `name` and `description`) |
| Request groups (folders) | Nested folders, in Insomnia's sort order (`metaSortKey` / `meta.sortKey`); folder environment values become `variables`, folder headers and auth are inherited |
| Requests | `kind: http` request files: method, URL, query parameters, headers, body, auth |
| GraphQL requests (`application/graphql` body) | `kind: graphql` request files |
| Body `application/json` / other text, form, multipart, file | `json` / `text`, `form`, `multipart`, `file` |
| Auth `basic`, `bearer`, `apikey`, `oauth2` (client credentials, authorization code), `none` | The matching auth helper; other types are written as `auth: {type: none}` (so no parent credential is inherited) and reported as `<type> auth unsupported` |
| Pre-request and after-response scripts | `# imported-script` comments, never executed |
| gRPC and WebSocket requests | Left out, reported as unsupported |
| Disabled headers, parameters and form fields | Dropped, and listed in the report |

Environments are flattened: each sub-environment becomes `environments/<slug>.yaml` holding the base environment's values with the sub-environment's overrides (a base `host` overridden by `Prod` gives `environments/prod.yaml` with Prod's `host`). A base environment without sub-environments becomes an environment of its own. Values that are objects or arrays become one variable per leaf, named as Insomnia templates reference them: `{"auth": {"token": "..."}}` gives `auth.token` (so `{{ _.auth.token }}` becomes `{{auth.token}}`) and `{"hosts": ["a"]}` gives `hosts[0]`. Each leaf is checked for secrets on its own, so `auth.token` is moved to the secret store. Every value set by an environment marked private (`isPrivate: true`) is a secret whatever its name, like the values of `http-client.private.env.json`: it goes to the secret store and is listed under `secrets` without a value. Base-environment values a private sub-environment does not override stay plain. Sub-environments whose names give the same file name (`Prod` and `prod`) are kept apart: the later one is imported as `prod-2`, and the report says so (this holds for every source's environments).

`{{ _.name }}` becomes `{{name}}`. Template tags other than plain variables (`{% response ... %}`, `{% uuid %}`, `{% now %}`) have no Rustman equivalent: they are kept as written and each is reported once per request as `unsupported template tag: <tag>`, with the request's item path.

## Bruno collections

`rustman import bruno <folder>` (or a bare `rustman import <folder>` with a `bruno.json` in it) reads a Bruno collection folder. The collection is named after `bruno.json`'s `name`.

| Bruno | Rustman |
|-------|---------|
| Folders | Nested folders; `folder.bru` gives the name, order (`meta.seq`), headers, auth, variables and scripts. Symbolic links to folders are not followed (reported as `symlinked folder <path> skipped`), and folders nested deeper than 32 levels are an error |
| `collection.bru` | The collection's `_collection.yaml`: headers, auth (`auth { mode: ... }`), variables, scripts |
| `.bru` request files | `kind: http` request files, ordered by `meta.seq`, then file name |
| Method block (`get`, `post`, ...) | Method and URL; `body:` and `auth:` select the body and auth blocks |
| `params:query`, `params:path` | Query parameters; `/users/:id` becomes `/users/{{id}}` with `id` under the request's `variables` (renamed `path_id` when `id` is used elsewhere, as for Postman) |
| `headers` | Headers (entries starting with `~` are disabled: dropped and reported) |
| `body:json`, `body:text`, `body:xml`, `body:form-urlencoded`, `body:multipart-form` (`@file(path)` parts), `body:graphql` + `body:graphql:vars` | `json`, `text`, `form`, `multipart`, or a `kind: graphql` file |
| `auth:basic`, `auth:bearer`, `auth:apikey`, `auth:oauth2`; `auth: none` / `inherit` | The matching auth helper; other modes are written as `auth: {type: none}` (so no parent credential is inherited) and reported as unsupported |
| `vars:pre-request` | Request variables |
| `vars:post-response` reading `res.body...` | `json` captures (`user_id: res.body.data.id` → `{var: user_id, from: json, path: $.data.id}`); other expressions are reported |
| `script:pre-request`, `script:post-response`, `tests` | `# imported-script` comments, never executed |
| `docs` | `description` |
| `environments/*.bru` | `environments/<name>.yaml`; `vars:secret [ ... ]` names are listed under `secrets` (Bruno never exports their values: set them with `rustman secret set`); a name listed there that also has a value in `vars` is a secret too, and its value goes to the secret store |

`assert` entries map to declarative assertions:

| Bruno assertion | Rustman assertion |
|-----------------|-------------------|
| `res.status: eq 200` | `status: 200` |
| `res.body.a.b: eq v` / `neq v` | `json: {path: $.a.b, equals: v}` / `not_equals` |
| `res.body.a: gt n` / `lt n` | `json: {path: $.a, gt: n}` / `lt` |
| `res.body.a: contains v` | `json: {path: $.a, contains: v}` |
| `res.body.a: isDefined` | `json: {path: $.a, exists: true}` |

Operands are read as JSON when they parse (`200`, `"ok"`, `true`), and as strings otherwise. Any other assertion (on headers, `res.status` with an operator other than `eq`, `matches`, ...) is reported as unsupported with its line.

A `.bru` file that does not follow the grammar fails the import with exit code 2 and the line: ``import: shop/users/get.bru: line 10: block `headers` is not closed``.

## HAR captures

`rustman import har capture.har` imports each entry of a HAR 1.2 capture (browser developer tools → "Save all as HAR") as a request file, in a folder per host (`api-example-com/`). The file is read one entry at a time, and response bodies are never read into memory or written anywhere.

| Flag (repeatable) | Keeps |
|-------------------|-------|
| `--host <host>` | Requests to this host (`api.example.com`, or with its port) |
| `--method <method>` | Requests with this method |
| `--content-type <type>` | Requests whose request body or response has this MIME type (`application/json`) |
| `--include-static` | Images, fonts, stylesheets and scripts too |

Without `--include-static`, static assets (by response MIME type `image/*`, `font/*`, `text/css` or JavaScript, or by URL extension such as `.png`, `.css`, `.js`, `.woff2`) are dropped, each listed in the report as `static asset dropped: GET https://cdn.example.com/app.css`. Entries with the same method, URL (query parameters in any order) and body are imported once; the report counts the duplicates and the entries left out by the filters. The filters are only accepted by `rustman import har`.

Query parameters come from the URL as written (still percent-encoded); the entry's `queryString` list, which browsers fill with the same parameters decoded, is used only when the URL has no query, so parameters are never doubled.

Transport and browser-managed headers are not written: `Host`, `Content-Length`, `Connection`, `Transfer-Encoding`, `Accept-Encoding`, `Keep-Alive`, `Upgrade`, `TE`, `Trailer`, HTTP/2 pseudo-headers (`:authority`, `:path`, ...) and `sec-*` headers. `Cookie` and `Authorization` values are moved to secrets like any other credential.

## HTTP files

`rustman import http api.http` reads a `.http` or `.rest` file (JetBrains HTTP Client and VS Code REST Client); a folder imports every `.http` and `.rest` file in it, one folder per file.

```text
@api_version = v1

### List items
GET {{host}}/{{api_version}}/items
Accept: application/json

###
# @name create-item
POST {{host}}/{{api_version}}/items
Content-Type: application/json

{"name": "lamp"}
```

- Requests are separated by `###` lines; the text after `###`, or a `# @name` / `// @name` comment, names the request (default: `METHOD /path`).
- The request line is `METHOD URL [HTTP/version]` (a bare URL is a `GET`); `?` and `&` lines right after it continue the query. Headers follow up to a blank line, and the body after it.
- Bodies are `json` (by `Content-Type`, or a body starting with `{` or `[` when there is none), `form` (`application/x-www-form-urlencoded`), `file` (`< ./path`) or `text`.
- `@name = value` lines become the collection's `variables`.
- JetBrains response handlers (`> {% ... %}`) are kept as `# imported-script` comments and never executed; handler files (`> ./script.js`) are reported as unsupported.

Environments come from the files next to the `.http` file (or in the folder):

| File | Becomes |
|------|---------|
| `http-client.env.json` | `environments/<name>.yaml` variables |
| `http-client.private.env.json` | Secrets of those environments (values go to the secret store) |
| `.vscode/settings.json`, `rest-client.environmentVariables` | `environments/<name>.yaml` variables |

`$shared` values apply to every environment that does not set them. Values that are objects (such as JetBrains auth settings) are reported as unsupported. As everywhere, variables whose name looks like a secret are moved to the secret store.

## Variables

`{{name}}` placeholders stay as they are. Dynamic variables are translated: `{{$guid}}` and `{{$randomUUID}}` become `{{$uuid}}`; `{{$timestamp}}`, `{{$randomInt}}` and Rustman's own dynamic variables stay. Any other (`{{$randomCity}}`) is kept unchanged and reported as `unmapped dynamic variable $randomCity`.

## Secrets

Before anything is written, the importer looks for secrets in three passes.

**1. Named fields.** A literal value is a secret in:

- `Authorization`, `Proxy-Authorization` and `Cookie` headers, headers whose name looks like a secret (`X-API-Key`) or ends in `-auth`, `-token`, `-key`, `-secret` or `-signature` (any case: `X-Custom-Auth`), and headers whose value starts with an auth scheme (see below);
- query parameters, form fields and multipart text parts whose name looks like a secret (HAR multipart bodies exported as raw `text`, as Chromium does, are split into parts first);
- `"key": "Basic dTpw..."` (any auth scheme, see below) under any key of a JSON body: the scheme stays and the credential moves;
- the password (or token) in the userinfo of a URL, of a request URL or of any variable holding one (`postgres://admin:p4ss@db/x`): `https://admin:p4ss@host` becomes `https://admin:{{url_password}}@host`, and a variable `db_url` gets `{{db_url_password}}`;
- `"key": "value"` pairs of JSON, raw and GraphQL-variables bodies (also every string of a `"key": [...]` array, and numbers of four digits or more such as `"pin": 12345678`), and `key=value` pairs of form-encoded raw bodies, whose key looks like a secret (`"password": "{{password}}"`);
- basic passwords, bearer tokens, API-key values and OAuth2 client secrets (a cached OAuth2 access token is dropped and reported: Rustman fetches its own);
- collection, folder, request (Postman path variables, Bruno `vars:pre-request`) and environment variables whose name looks like a secret, whose value starts with an auth scheme, or that the source marks secret (Postman `type: secret`, Bruno `vars:secret`, Insomnia private environments, `http-client.private.env.json`);
- literal values that scripts give to secret-named variables, headers or keys (`pm.environment.set('apiSecret', '...')`, `req.setHeader("Authorization", "Bearer ...")`, `const apiKey = '...'`, `secret = '...'`), and the literal passed to `btoa(...)` or `atob(...)` (a Basic credential, stored as `basic_credentials`).

A name looks like a secret when it matches `token|secret|password|passwd|api[_-]?key|client[_-]?secret` (case-insensitive); or names a session, cookie, credential, bearer, JWT, access or private key, a signature, passcode or passphrase; or is exactly `auth`, `key`, `sid`, `code` or `auth_code` (OAuth authorization codes); or ends in `sig`, `pass` or `otp` (`X-Sig`); or has `pwd`, `pass`, `passwd`, `pin`, `otp`, `sid`, `verifier`, `csrf` or `xsrf` as a whole segment, split at punctuation and lowercase-to-uppercase changes (`X-Pwd`, `pinCode`, `code_verifier`; `spinner` and `bypass` stay plain). A variable or header value that starts with an auth scheme and a credential (`Bearer`, `Basic`, `Digest`, `Token`, `ApiKey`, `Bot`, `SSWS`, `Negotiate` or `NTLM`, any case) is a secret whatever its name; in a header the scheme stays and only the credentials move (`X-Client: Token {{x_client}}`).

**2. Well-known tokens, everywhere.** Every text the import writes is scanned for tokens of these formats, whatever field holds them: URLs (paths included), query parameters, headers, bodies, auth fields, variables, request, folder and collection names, descriptions (Bruno `docs` included), and string literals in scripts. The formats are GitHub (`ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_`, `github_pat_`), GitLab (`glpat-`), Slack (`xoxa-`, `xoxb-`, `xoxp-`, ...), Stripe live keys (`sk_live_`, `rk_live_`), AWS access key ids (`AKIA`/`ASIA` and 16 characters), Google API keys (`AIza` and 35 characters), JWTs (three base64url segments starting `eyJ`) and PEM private-key blocks. Tokens are also found when percent-encoded (`-----BEGIN%20RSA%20PRIVATE%20KEY-----...`). Each is stored under a name for its kind (`github_token`, `jwt`, `aws_access_key`, `google_api_key`, `private_key`, ...).

**3. Repeats.** Every value moved by the first two passes is then looked for where it repeats in those same texts (another header, the URL path or query, a body, a name or description, an environment variable): as written (*raw*, also when the value itself holds `{{`) and, for secret-like values, *encoded* (percent-encoded in upper or lower case, JSON-escaped with or without `\/`, HTML-escaped). A value is *secret-like* when it has at least 8 characters and either comes from at least two of the classes letters, digits and symbols, or has 16 characters or more (`p4ssw0rd!`, `sk-live-42ab`, `8473629184736291`, a 32-letter token; not `password`, `123456`, `example` or `application`). Placeholders are substituted raw when a request is sent, so a repeat becomes `{{name}}` only where that sends the same bytes under every environment.

A secret moved from the collection itself (stored in the import's target environment, which the collection's own placeholders already use) is *collection-scoped*; a secret of a source environment file (Postman, Insomnia, Bruno, `http-client.env.json` environments) is *environment-scoped*: it resolves only while that environment is active.

In requests, collections and folders:

- a raw repeat of a collection-scoped secret-like value that fills the whole field, or sits on token boundaries (no letter or digit right before or after it), becomes `{{name}}` (`X-Note: SAME-VAL-42` → `X-Note: {{x_api_key}}`, `/keys/SAME-VAL-42?x=1` → `/keys/{{x_api_key}}?x=1`). A value of a well-known token shape (GitHub, JWT, AWS, Slack, Stripe, Google, PEM) is matched anywhere, also right after letters (`abcghp_...` → `abc{{github_token}}`);
- an encoded repeat of a collection-scoped secret-like value (`Pa\"ss` in a JSON string, `ab%2Fcd` in a path, `p%40ss` in a form body), or a raw one inside a longer word (`myPassw0rd!`), becomes the literal `«redacted:name»`, since `{{name}}` would send different bytes (invalid JSON, an extra path segment, an extra form field). The report says `request <file > item>: field <field> contained secret <name> in encoded form; it was redacted, edit the field to use {{name}}` (or `inside other text`);
- any repeat of an environment-scoped secret-like value becomes `«redacted:name»`, since `{{name}}` would send another value, or none, under another environment. The report says `request <file > item>: field <field> repeated secret <name> of environment <env>; it was redacted, edit the field`;
- after these, each field is also read percent-decoded (`+` as a space), JSON-unescaped (`\uXXXX` too) and HTML-unescaped; when one of these readings still shows a secret-like value (a partly or differently encoded repeat such as `p%40ss&w0rd=1`, `my+secret+pass`, `p\u0040ss`), the whole field becomes `«redacted:name»`, with the `in encoded form` note.

In environment files, a raw secret-like repeat of that same environment's secret, as the whole value or on token boundaries, becomes `{{name}}` (`cb_url: https://x.test/cb?t={{token}}`); an encoded or in-word repeat of it, and any repeat of another environment's secret, becomes `«redacted:name»` with a note, as above.

Everywhere, a repeat of a value that is not secret-like is left as written, and the report gets a `review` note `the value of secret <name> also appears in <file > item>:<field>; review it`, never showing the value. So a Basic password `password` leaves `grant_type=password` and `/password/reset` alone, and a secret `123456` leaves `zip=123456` and `api.123456.com` alone. In free text (names, descriptions) every replacement is `«redacted:name»`; names and file names change only for secret-like values.

Values shorter than 6 characters or longer than 16 KiB are not looked for. Environment files only get the first rule, and only with the secrets of that same environment: a plain variable whose whole value is one of its environment's secrets becomes that secret's placeholder; nothing else in an environment file changes.

Outside repeats, a value moved from a field that takes templates (URLs, headers, bodies, auth, variables) becomes the `{{name}}` placeholder; in free text (names, descriptions, report notes and locations) it becomes `«redacted:name»`. In scripts, which are kept as written and never run, every moved value is shown as `«redacted:name»`, as in the report's unconverted statements. For example `Authorization: Bearer eyJhbGciOi...` becomes `Authorization: Bearer {{authorization_token}}`, and a description `Use key ghp_...` becomes `Use key «redacted:github_token»`. Values that are already placeholders are left alone. The printed summary and the `--report` file are checked once more for secret-like values before they are written.

Variables keep their scope: a folder variable (Insomnia, Bruno) moved under another name (`password_2`, because the collection's `password` holds another value) is renamed only in that folder's requests and subfolders, not where the collection's `password` applies, and not below a subfolder or request that declares `password` again. A new secret name never takes the name of any variable declared anywhere in the source (collection, folders, requests, environments) other than the variable being moved, nor any name the source references as `{{name}}` without declaring it (such a reference is meant to come from your own environment): with a plain collection variable `token_2`, a folder's moved `token` becomes `token_3`, so no placeholder resolves to another variable.

The target environment is `--env`, else the active one. Without either, secrets go to a new environment named after the collection (`environments/billing-api.yaml`), and the report says so. A name already used in that environment is reused only when it holds the same value; otherwise the next free name (`token_2`) is used, so an import never replaces a stored secret.

Secret values are stored in the OS keychain, or `.rustman/secrets.yaml` when there is none (see [Secret storage](environments.md)). The report lists every moved secret by name, environment and source location, never by value, and there is no option to keep secrets inline: the only way to put a literal secret into a file is to edit it by hand. As a last guard, the writer refuses any file whose secret fields still hold a literal value. Values that look like secrets (24 characters or more, letters and digits, no spaces) but are kept as plain variables are listed as `review` notes, by name only.

Known limits: outside the named fields above, only well-known token formats and repeats of moved values are found, so some secrets are written as they are. Review these by hand after an import:

- XML bodies and other raw text that is neither `"key": "value"` nor `key=value` (`<password>p4ss</password>`), and literals inside a GraphQL query document (`login(password: "p4ss")`; GraphQL *variables* are scanned);
- descriptions and docs, beyond well-known tokens and repeats of moved values;
- bare base64: a secret that appears base64-encoded without an auth scheme (`X-B64: UzNjcjN0LVBhc3Mh`, a token inside a base64 blob) is not found and is written as it is. A `Basic <base64>` value is found by its scheme;
- repeats in encodings other than percent-encoding, JSON escaping and HTML escaping;
- repeats of values that are not secret-like (`password`, `123456`), which are only reported;
- string literals in scripts that are not given to a secret-named variable, header or key, `btoa` or `atob`, and are not a well-known token: the script is kept as written and only a `review` note points at it;
- expected values in assertions suggested from scripts (`pm.expect(body.token).to.eql("...")`), which are written as checks.

## Scripts

Pre-request and test scripts are never executed. Each is kept verbatim (any moved secret value redacted) in an `# imported-script` comment block at the top of the request file, or of `_collection.yaml` for collection and folder scripts, and the report lists it as `script not executed`.

Statements in the mapping table below are also written as declarative entries (Bruno `assert` blocks are mapped as described in [Bruno collections](#bruno-collections)). Statements are found without running anything: the script is split at `;`, line breaks and block braces (the callback of `pm.test("...", () => ...)` or `function () { ... }` counts as its inner statements), whitespace outside strings is ignored, and `'single'` and `"double"` quotes are the same.

| Script statement | Written as |
|------------------|------------|
| `pm.response.to.have.status(201)`, `pm.expect(pm.response.code).to.eql(201)` (or `.equal`) | `status: 201` |
| `pm.response.to.be.ok` | `status: "2xx"` |
| `pm.response.to.have.header("ETag")` | `header: {name: ETag, exists: true}` |
| `pm.response.to.have.header("Content-Type", "application/json")` | `header: {name: Content-Type, equals: application/json}` |
| `pm.expect(pm.response.responseTime).to.be.below(300)` | `latency: {total_lt: 300}` |
| `pm.expect(pm.response.text()).to.include("done")` | `body: {contains: done}` |
| `const body = pm.response.json()` (or `let`, `var`) | Tracked: `body.a.b` then means `$.a.b` |
| `pm.expect(body.user.id).to.eql(42)` (or `.equal`, `.deep.equal`) | `json: {path: "$.user.id", equals: 42}` |
| `pm.expect(body.user.id).to.not.eql(0)` | `json: {path: "$.user.id", not_equals: 0}` |
| `pm.expect(body.user).to.exist` | `json: {path: "$.user", exists: true}` |
| `pm.expect(d.items[0].tags).to.have.lengthOf(2)`, `pm.expect(d.items.length).to.eql(2)` | `json: {path: "$.items[0].tags", length: 2}` |
| `pm.expect(body.name).to.be.a("string")` (or `.an`) | `json: {path: "$.name", type: string}` |
| `pm.expect(body.total).to.be.above(0)` / `.below(9)` | `json: {path: "$.total", gt: 0}` / `lt: 9` |
| `pm.expect(body.roles).to.include("admin")` | `json: {path: "$.roles", contains: admin}` |
| `pm.environment.set("v", <source>)`, `pm.globals.set(...)` | A capture with `persist: true` |
| `pm.collectionVariables.set("v", <source>)`, `pm.variables.set(...)` | A capture without `persist` |

A setter's `<source>` maps when it is `pm.response.json().a.b` or a tracked alias path (`from: json`, `path: $.a.b`), `pm.response.headers.get("ETag")` (`from: header`, `name: ETag`) or `pm.cookies.get("sid")` (`from: cookie`, `name: sid`). `pm.response.json()` may also be used directly in place of an alias, and paths may use `[0]` and `["key"]` segments.

Everything else stays comment-only. The summary counts the statements:

```text
scripts: 31/40 statements converted (77%)
```

The percentage is rounded down. The JSON report has the same counts under `migration` (`found`, `mapped`, `unconverted`, `percent`), and per request its file path, counts and unconverted statements (any moved secret value redacted).

### Script migration report

`--migration-report <file.md>` writes a Markdown report with one section per request that has unconverted statements. Each row shows the statement, the request file and a suggestion:

| Statement | Suggestion |
|-----------|------------|
| `pm.expect(pm.response.json().items.every(i => i.price > 0)).to.be.true` | A `schema` assertion with `items.price` constrained by `exclusiveMinimum: 0`, with the inline schema to paste (`>=`, `<` and `<=` give `minimum`, `exclusiveMaximum` and `maximum`) |
| `postman.setNextRequest("checkout")` | `no declarative equivalent; consider a plugin`, plus a pointer to `order:` in `_collection.yaml` for run order |
| anything else | `no declarative equivalent; consider a plugin` |

Moved secret values are redacted in the report, as everywhere else.

## File names

Files are named after the slug of the item name (lowercase ASCII letters and digits, `-` between words, at most 64 characters) plus `.rman.yaml`. Items with the same slug in one folder get `-2`, `-3` suffixes in source order (`Get User` and `get user!` become `get-user.rman.yaml` and `get-user-2.rman.yaml`). Every generated path, relative to the workspace root and including the collection folder (`--into`), is at most 180 characters and 20 levels deep, so that it stays under Windows' 260-character `MAX_PATH` with up to 80 characters left for the workspace root. A source whose names or nesting go beyond that fails before anything is written (exit code 2) with `generated path too long (<n> characters, limit 180): <path>`, the path shortened to its first 40 and last 30 characters; shorten the names, flatten the folders or pass a shorter `--into`. Paths are never silently truncated. Everything is written with the workspace's deterministic emitter, so importing the same source into two empty workspaces gives byte-identical trees.

## Re-importing

When a target file already exists, `--on-conflict` decides:

| Policy | Request files | `_collection.yaml` | Environment variables |
|--------|---------------|--------------------|-----------------------|
| `skip` (default) | kept, reported `skipped (exists)` | kept | kept |
| `overwrite` | replaced, reported `overwritten` | replaced | replaced |
| `rename` | written next to it with the next free suffix, reported `renamed to <path>` | kept; renamed requests are appended to its `order:` (reported `order extended with ...`) | kept |

Imports into one workspace run one at a time: an import holds `.rustman/import.lock` from reading the workspace to writing the last file, and a second import waits for it, so neither loses the other's changes to `environments/<name>.yaml`. A file whose content would not change is never rewritten or duplicated, under every policy: it is listed with the action `unchanged`, counted in `skipped`, summarized as `unchanged: <n> files already up to date`, and never reported as a conflict.

An import is all or nothing: every file is first written next to its target as a temporary `.rmtmp` file, then the secrets are stored, then the files are moved into place. If a write or the secret store fails, the temporary files and any folder made for them are removed and nothing is stored.

## The report

Every import prints a summary:

```text
imported Billing API (postman) into billing-api/
written: 11 requests, 4 collections, 1 environments; skipped: 0; dropped: 4; secrets moved: 9; unsupported: 1; unmapped: 1
3 scripts preserved as comments, not executed
scripts: 5/5 statements converted (100%)
secret: authorization_token -> environment 'local' (billing.json > Invoices / List invoices)
dropped: disabled header X-Debug dropped (billing.json > Users / List)
unsupported: hawk auth unsupported (billing.json > Invoices / Legacy export)
unmapped: unmapped dynamic variable $randomCity (billing.json > Users / Create)
script not executed: test (billing.json > Auth / Login)
```

`--report <file>` writes the same data as JSON, following [`schemas/import-report.schema.json`](schemas/import-report.schema.json): `schema` (`rustman.import-report/v1`), `source`, `destination`, `dry_run`, `summary` (the counts above), `files` (each planned file and its action), `conflicts`, `secrets` (name, environment, location), `notes` (kind, message and location: the source file plus the item path or line) and `migration` (the [script statement counts](#scripts)).

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | The import completed. Unsupported, dropped or unmapped features are reported but are not an error. |
| 1 | `--fail-on-unsupported` and some feature was unsupported, dropped or unmapped. |
| 2 | Unreadable, invalid, oversized (`import: source exceeds 100 MiB (use --max-size)`) or undetectable source; no workspace; `--into` outside the workspace. |

## Exporting

`rustman export` writes a collection folder or a single request file back out for other tools:

```text
rustman export postman <path> [-o <file>]   # a Postman Collection v2.1 JSON
rustman export http <path> [-o <file>]      # a .http file
```

`<path>` is a collection folder (the workspace root too) or a request file inside the workspace. Without `-o` the result is printed on standard output; with `-o` it is written to the file (parent folders are created) and `wrote <file>` goes to standard error.

Secrets are exported as the `{{name}}` placeholders the request files hold, never as values: `rustman export http api` writes `Authorization: Bearer {{token}}` for a request whose bearer token is the secret `token`. As a further guard, any secret value of the active environment (`--env`) that appears in the output, for example pasted into a description, is printed as `«redacted:<name>»`.

Features the target cannot hold are listed as warnings on standard error, such as `warning: export: api/me.rman.yaml: 1 assertions not exported (no equivalent)`:

| Feature | Postman v2.1 | `.http` |
|---------|--------------|---------|
| Folders, order, names, descriptions | Kept | Flattened into `###` blocks in run order; descriptions dropped |
| Collection and folder auth | Kept on the folder | Written into each request that inherits it |
| Collection and folder headers | Warning | Written into each request |
| Variables | Collection and folder `variable` lists (Postman ignores folder lists when it runs a request); request variables as URL variables (`{{id}}` path segments become `:id`) | `@name = value` lines at the top (one value per name; a different later value is a warning) |
| Basic, bearer, API key auth | Kept | `Authorization: Basic <user> <password>`, `Authorization: Bearer <token>`, the key header or query parameter |
| OAuth2 auth | Kept | Warning |
| Bodies | `raw` (JSON or text), `urlencoded`, `formdata`, `file`, `graphql` | As text, form fields joined with `&`, multipart with `< path` file parts, `< path` for a file body, GraphQL as a JSON `POST`; a `Content-Type` is added when missing |
| `# imported-script` blocks | Back to `prerequest` and `test` events (other sources' scripts are a warning) | Warning |
| Assertions, captures, settings, tags | Warning | Warning |
| `{{$uuid}}` | Written as Postman's `{{$guid}}` | Kept |

Importing an exported Postman collection into an empty workspace gives byte-identical request files when the collection used no feature from the warnings above, so a Postman collection survives import → export → import unchanged.

The export goes through the same neutral IR as imports (`rustman_import::export::collect` reads the workspace into it, `rustman_import::export::export` writes the target).

## Using the importer as a library

The pipeline lives in the `rustman-import` crate, so other front-ends and producers share it: a source parser builds the neutral IR (`ImportedCollection`: folders, requests, environments, notes with source locations), `rustman_import::plan` translates variables, extracts secrets, suggests assertions and lays out the files without writing anything, and `rustman_import::apply` stores the secrets and writes the files.
