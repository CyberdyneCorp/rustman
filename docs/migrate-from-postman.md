# Migrating from Postman

This guide moves a Postman workspace (collections, environments, scripts, Newman jobs) to Rustman. It covers what maps to what, the steps of a migration, and what does not carry over. Everything here is what this build does; the details of each step live in the linked pages, mainly [Importing collections](import.md).

Two differences shape the whole migration:

- **Rustman runs no scripts.** Pre-request and test scripts are kept as comments and never executed. The importer converts the common `pm.*` statements into declarative assertions and captures, and lists everything else so you can replace it.
- **Rustman is local-first.** Collections and environments are plain files in a Git repository; there are no accounts, cloud workspaces or sync. Secret values never go into those files: they live in the OS keychain (or an external secret store).

## Concept map

| Postman / Newman | Rustman | See |
|------------------|---------|-----|
| Workspace | A workspace: a folder with `rustman.toml`, usually a Git repository | [Request files](request-files.md#workspace-layout) |
| Collection | A folder with a `_collection.yaml` (`name`, `order:`, `auth`, `headers`, `variables`) | [Collections](request-files.md#collections) |
| Folder | A nested folder with its own `_collection.yaml`; auth and headers are inherited | [Collections](request-files.md#collections) |
| Request | A `*.rman.yaml` request file (`kind: http`, or `kind: graphql` for GraphQL bodies) | [Request files](request-files.md) |
| Saved examples (responses) | Not imported; use [snapshot assertions](snapshots.md) to keep a reference response, or an OpenAPI document for the mock server | [Known gaps](#known-gaps) |
| Collection variables | `variables` in the root `_collection.yaml` | [Variables](environments.md#variables) |
| Environment | `environments/<name>.yaml`, selected with `--env`, `RUSTMAN_ENV` or `default_environment` | [Environments](environments.md) |
| Globals | Not imported; put shared values in `[variables]` of `rustman.toml` | [Precedence](environments.md#precedence) |
| Secret-type variables | Names listed under `secrets:` in the environment file, values in the OS keychain | [Secrets](environments.md#secrets) |
| Postman Vault, external vaults | `{{secret:<provider>/<path>#field}}` references to 1Password, HashiCorp Vault, SOPS, cloud secret managers, ... | [Secret providers](secrets.md) |
| Path variables (`:id`) | `{{id}}` in the URL, with `id` under the request's `variables` | [Importing collections](import.md#postman-collections) |
| Dynamic variables | `{{$uuid}}` (from `$guid` / `$randomUUID`), `{{$timestamp}}`, `{{$randomInt}}`, `{{$isoTimestamp}}`, `{{$randomString n}}`; the faker-style `$random*` variables have no equivalent | [Dynamic variables](environments.md#dynamic-variables) |
| `pm.test` / `pm.expect` checks | Declarative `assertions:` (status, header, JSONPath, JSON Schema, body, latency) | [Assertions and captures](assertions.md) |
| `pm.environment.set` / `pm.collectionVariables.set` | `captures:` (with `persist: true` for environment and global setters) | [Captures](assertions.md#captures) |
| Logic in scripts (loops, signing, computed values) | WASM [plugins](plugins.md): auth schemes, assertion functions, value generators, transformers | [Extension points](plugins.md#extension-points) |
| `postman.setNextRequest` | No equivalent; run order comes from `order:` in `_collection.yaml` | [Collections](request-files.md#collections) |
| Collection Runner | `rustman run [paths]` with `--filter`, `--iterations`, `--bail`, `--parallel` | [`rustman run`](cli.md#rustman-run) |
| Data files (`-d data.csv`) | `rustman run --data data.csv` (CSV or JSON), or `data:` in `_collection.yaml` | [Data-driven runs](data-runs.md) |
| Newman (`newman run ... -r junit`) | `rustman run --reporter junit:out/junit.xml` in CI, with exit codes per outcome | [Reporters](cli.md#reporters), [Exit codes](cli.md#exit-codes) |
| Newman HTML reporters | `rustman run --report html=out/report.html` (also `markdown`, `sarif`, `json`, `junit`) | [Reports](reports.md) |
| Auth: Basic, Bearer, API key, OAuth2 (client credentials, authorization code, PKCE) | The same helpers, imported as they are | [Auth](http.md#auth) |
| Auth: Digest, AWS Signature, JWT | Built in, but not converted by the importer: set them again by hand | [Signature auth](auth.md) |
| Auth: Hawk, NTLM, OAuth 1.0, Akamai EdgeGrid | Auth-scheme plugins | [Auth schemes](plugins.md#auth-schemes) |
| Cookie jar | One jar per environment in `.rustman/cookies/`; `rustman cookies clear` | [Cookies](http.md#cookies) |
| Code generation | `rustman export snippet --lang <lang>` (or `Alt+C` in the TUI) | [Code snippets](snippets.md) |
| Mock servers | `rustman mock serve <openapi>`: served from an OpenAPI document, locally | [Mock server](mock.md) |
| Monitors | `monitors/*.rman-monitor.yaml`, run by `rustman monitor run`, the daemon, cron, systemd, launchd or a CI schedule | [Monitors](monitors.md) |
| API documentation | `rustman docs build` / `rustman docs serve` from your request files | [API docs](api-docs.md) |
| Interceptor, proxy capture | `rustman record` (a local recording proxy) | [Traffic recorder](record.md) |
| Performance tests | `rustman bench` (load) and `rustman stress` (breaking points) | [Load testing](bench.md), [Stress testing](stress.md) |
| Postbot and AI agent features | `rustman mcp serve`: an MCP server any agent (Claude Code, Claude Desktop, IDE agents) can drive, read-only unless `rustman.toml` allows more | [Agents (MCP server)](agent.md) |
| Postman's MCP request type | `kind: mcp` request files and `rustman mcp` client commands | [MCP client](mcp-client.md) |
| Team workspaces, sharing, history sync | Git: review request-file changes in pull requests | |

## Step by step

### 1. Export from Postman

- **Collections:** in Postman, open the collection's menu, choose **Export** and pick **Collection v2.1** (v2.0 is read too). One file per collection.
- **Environments:** export each environment you use (the environment's menu, **Export**). Each gives a `*.postman_environment.json` file.
- Globals are not exported in a format Rustman reads; note the values you need (see [Known gaps](#known-gaps)).

Secret values may be in these files. They stay on your machine: the importer moves them to the keychain and never writes them into the workspace, but delete the exported files once the migration is done.

### 2. Create a workspace

```bash
rustman init my-api && cd my-api
```

`rustman init` writes `rustman.toml`, a `local` environment and a sample request. Pass `--bare` to skip the sample. If you keep the workspace in an existing repository, run `rustman init` there instead.

### 3. Import the environments

```bash
rustman import postman-env staging.postman_environment.json
rustman import postman-env prod.postman_environment.json
```

Each becomes `environments/<slug>.yaml`. Variables that Postman marks `secret`, and variables whose name looks like a credential (`token`, `password`, `api_key`, ...), are listed under `secrets:`; their values go to the OS keychain (or `.rustman/secrets.yaml` when there is none), never into the file. Check the result with:

```bash
rustman env list
rustman secret list --env staging
```

### 4. Preview, then import the collection

Start with a dry run. It converts everything and prints the planned files, the secrets it would move and the report, without writing anything:

```bash
rustman import postman billing.postman_collection.json --into billing/ --env staging --dry-run
```

Then import for real, keeping both reports:

```bash
rustman import postman billing.postman_collection.json --into billing/ --env staging \
  --report out/import-report.json --migration-report out/migration.md
```

- `--into` is the collection folder (default: a folder named after the collection).
- `--env` is the environment that receives secrets found in the requests (default: the active environment; without one, a new environment named after the collection).
- `--report` writes the JSON report (`rustman.import-report/v1`); `--migration-report` writes the Markdown list of script statements that were not converted, each with a suggestion.
- Re-running the import is safe: existing files are skipped by default (`--on-conflict skip|overwrite|rename`), and unchanged files are never rewritten.

The summary says what happened:

```text
imported Billing API (postman) into billing/
written: 11 requests, 4 collections, 1 environments; skipped: 0; dropped: 4; secrets moved: 9; unsupported: 1; unmapped: 1
3 scripts preserved as comments, not executed
scripts: 31/40 statements converted (77%)
```

Every `dropped`, `unsupported`, `unmapped` and `secret` line names the item it comes from. In CI, `--fail-on-unsupported` makes an import that loses anything exit with 1.

### 5. Review the converted scripts

Open a few request files. Each Postman script is kept verbatim in an `# imported-script` comment block at the top of the file (collection and folder scripts in `_collection.yaml`), and the statements Rustman understood are also written as `assertions:` and `captures:`. For example, a Postman test such as

```js
pm.test("created", () => {
  pm.response.to.have.status(201);
  const body = pm.response.json();
  pm.expect(body.id).to.exist;
  pm.environment.set("invoice_id", body.id);
});
```

is written as

```yaml
assertions:
  - status: 201
  - json:
      path: $.id
      exists: true
captures:
  - var: invoice_id
    from: json
    path: $.id
    persist: true
```

The statements that convert are listed in the [script mapping table](import.md#scripts): status checks, `pm.response.to.be.ok`, header presence and value, response time, body text, `pm.response.json()` field checks (equality, existence, length, type, comparisons, `include`), and the environment, global, collection and plain variable setters whose value comes from the JSON body, a header or a cookie.

Go through `out/migration.md` for the rest. Typical replacements:

| Script pattern | Rustman replacement |
|----------------|---------------------|
| Checks over every item of an array (`items.every(...)`) | A `schema` assertion; the migration report suggests the inline schema to paste |
| Validating a response against a JSON Schema with `tv4` / `ajv` | A `schema` assertion pointing at the schema file |
| `postman.setNextRequest(...)`, request chaining | Order the requests with `order:` in `_collection.yaml`; values flow between them through captures |
| Logging in from a pre-request script to fetch a token | A login request first in `order:` with a capture of the token, or the built-in OAuth2 helper |
| Signing requests (HMAC, custom headers) | Built-in [Digest, AWS SigV4 or JWT auth](auth.md), or an [auth-scheme plugin](plugins.md#auth-schemes) |
| Computed values (timestamps, random data) | [Dynamic variables](environments.md#dynamic-variables), or a [value-generator plugin](plugins.md#value-generators) |
| Custom checks with no declarative form | An [assertion-function plugin](plugins.md#assertion-functions) |

Once the replacements are in place, delete the `# imported-script` blocks you no longer need; they are never run.

### 6. Fill in what the import left out

- **Auth the importer reported as unsupported** (`digest`, `awsv4`, `hawk`, `ntlm`, OAuth2 `implicit` or `password` grants, ...) is written as `auth: {type: none}`, so the request inherits no parent credential. Set Digest, AWS SigV4 or JWT auth by hand ([Signature auth](auth.md)); the others need a plugin.
- **Globals:** add the values you used to `[variables]` in `rustman.toml`, or to the environments. Declare credentials as secrets and store them with `rustman secret set <name> --env <env>`.
- **OAuth2 tokens** Postman had cached are not carried over. For the authorization-code grant, log in once on a workstation with `rustman auth login <request> --env <env>`.
- **Unmapped dynamic variables** (`$randomCity`, ...) are kept as written and reported; replace them with a fixed value, a data file column, or a value-generator plugin.

### 7. Run the collection

```bash
rustman run billing --env staging
rustman run billing --env staging --data customers.csv --data-label email
rustman run . --env staging --filter 'billing/**' --bail
```

`rustman run` sends every request under the given folders in collection order, checks the assertions and chains captures, like the Collection Runner. `--iterations <n>` repeats the run; `--data` runs once per CSV or JSON row with the columns as variables, like `newman run -d`.

You can also open the workspace in the terminal UI with `rustman` and send requests one at a time.

### 8. Replace Newman in CI

| Newman | Rustman |
|--------|---------|
| `newman run collection.json -e staging.json` | `rustman run billing --env ci` |
| `-d data.csv`, `-n 5` | `--data data.csv`, `--iterations 5` |
| `--bail` | `--bail` (or `--bail=iteration` with data files) |
| `--folder Invoices` | `rustman run billing/invoices`, or `--filter 'billing/invoices/**'` |
| `--env-var token=...` | `--var token=...` for plain values; declared secrets for credentials (below) |
| `-r cli,junit --reporter-junit-export out.xml` | `--reporter pretty --reporter junit:out/junit.xml` |
| `-r htmlextra` | `--report html=out/report.html` |
| `--insecure`, `--timeout-request` | `--insecure`, `--timeout 5s` |

Give CI its own environment (`environments/ci.yaml`) declaring the same secrets, and feed them from the CI's secret variables into the file store at the start of the job ([the CI pattern](environments.md#the-ci-pattern)). In GitHub Actions:

```yaml
env:
  RUSTMAN_ENV: ci
  RUSTMAN_SECRET_STORE: file
steps:
  - uses: actions/checkout@v5
  - run: cargo install --git https://github.com/CyberdyneCorp/rustman rustman-cli --locked
  - run: printf '%s' "$API_TOKEN" | rustman secret set api_token
    env:
      API_TOKEN: ${{ secrets.API_TOKEN }}
  - run: rustman run billing --reporter pretty --reporter junit:out/junit.xml
```

The exit code tells CI what happened: 0 when every request passed, 1 when an assertion failed, 2 for a usage or configuration error, 3 when a request errored (network, TLS, timeout, unresolved variable). Secrets are redacted as `«redacted:<name>»` in every reporter and log.

### 9. Move the rest

- **Mock servers:** `rustman mock serve api.yaml` serves an OpenAPI 3.0/3.1 document, with responses from its examples or synthesized from its schemas. If your Postman mock was built from saved examples, describe the API in OpenAPI first ([Mock server](mock.md)).
- **Monitors:** write a `monitors/<name>.rman-monitor.yaml` with the targets, environment and schedule, then run it with `rustman monitor daemon`, or generate a cron, systemd, launchd or GitHub Actions configuration with `rustman monitor schedule-recipe <name> --for github-actions`. Monitors are read-only unless the file sets `allow_mutations` ([Monitors](monitors.md)).
- **AI features:** register `rustman mcp serve` with your agent (`claude mcp add rustman -- rustman mcp serve` for Claude Code) to let it list, send and test your saved requests under the workspace's permissions and safety policy ([Agents (MCP server)](agent.md)).
- **Going back, or sharing with Postman users:** `rustman export postman billing -o billing.postman_collection.json` writes a Collection v2.1 file; secrets are exported as `{{name}}` placeholders, never as values ([Exporting](import.md#exporting)).

## Known gaps

What the importer does not carry over, or Rustman does not do:

- **Script execution.** No JavaScript runs, by design. Unconverted statements are listed in the migration report and stay as comments.
- **Saved examples (responses)** in a collection are not imported.
- **Globals** exports are not read; move them by hand.
- **Folder-level variables** are dropped (Postman itself never resolves them) and reported.
- **Disabled headers, query parameters, form fields and variables** are dropped and reported.
- **Request settings** (`protocolProfileBehavior`: redirects, TLS, encoding options) are not imported; set them under `settings:` in the request file or in `rustman.toml` ([Settings](request-files.md#settings)).
- **Auth types** other than Basic, Bearer, API key and OAuth2 client-credentials / authorization-code are written as `auth: {type: none}` and reported. Digest, AWS SigV4 and JWT exist in Rustman and must be set by hand; the rest need a plugin.
- **Cached OAuth2 tokens** are dropped; Rustman fetches its own.
- **Faker-style dynamic variables** (`$randomCity`, `$randomEmail`, ...) are kept unchanged and reported as unmapped.
- **Secrets in free text** (XML bodies, GraphQL query documents, descriptions, base64 blobs) may not be found; review them after an import ([Secrets](import.md#secrets) lists the limits).
- **Postman Flows, cloud collaboration, comments, forks and pull requests inside Postman** have no equivalent: use Git and your code-review tool.
- **Mock servers** are served from an OpenAPI document, not from a collection's examples.
