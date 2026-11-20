# API docs

`rustman docs build` turns a workspace into a static documentation site: collections become sections, requests become endpoint pages. The site is plain HTML, CSS and a small script, works offline and over `file://`, and is deterministic, so it can be committed and reviewed in Git. `--format markdown` writes the same content as GitHub-flavored Markdown instead, and `--format both` writes both side by side.

```text
$ rustman docs build
wrote 12 files to site/
```

The behavior is specified in [`openspec/changes/add-docs-generator`](../openspec/changes/add-docs-generator/).

## The generated site

- `index.html` lists every section (a collection, or a nested folder as a subsection) in workspace order, with its description and its endpoints. Requests at the workspace root form a section named after the workspace.
- `endpoints/<slug>.html` is one page per request. The slug comes from the request's path (`users/create.rman.yaml` → `users-create`), with `-2`, `-3`... on collisions, assigned in workspace order.
- `assets/app.<hash>.css`, `assets/app.<hash>.js` and `assets/search-index.<hash>.js` are named after their content, so browsers never keep a stale copy.
- `search-index.json` is the search index (endpoint names, methods, paths, descriptions and parameter names).
- `reference/<slug>.html` holds the GraphQL schema and JSON-RPC method pages (see [Protocol references](#protocol-references)).
- `.rustman-docs-manifest` lists the files the build wrote.

An `http` endpoint page shows:

| Part | Content |
|------|---------|
| Request line | The method and URL template, with variables kept as `{{name}}` |
| Description | The request's `description`, then its `docs.description`, rendered as CommonMark |
| Auth | The scheme name (`basic`, `bearer`, `api_key`, `oauth2`, `digest`, `aws_sigv4`, `jwt`), never credentials |
| Query parameters, Headers | Tables with the enabled rows; headers include the ones inherited from collections |
| Body | The body type, then one row per field (top-level JSON keys, form fields, multipart parts) or the raw text |
| Expected behavior | One `Expected: ...` line per assertion, such as `Expected: status 201` |
| Produces | One `Produces: {{var}} from ...` line per capture |
| Example response | Only for requests that opted in (see [Examples](#examples)) |
| Code samples | One tab per language (see [Code samples](#code-samples)) |

Requests of other kinds (`graphql`, `jsonrpc`, `grpc`, `mcp`, `ws`, `sse`, and kinds this build does not know, which are read as opaque requests instead of failing the build) get a page listing their request-file fields as a table, with the same redaction: header and metadata values of `Authorization`, `Cookie`, `Set-Cookie`, `Proxy-Authorization` and `X-Api-Key` are masked, and `auth` shows its type only.

Navigation and content are plain HTML and readable with JavaScript disabled; only the search box and the theme toggle need JavaScript. Raw HTML in descriptions is shown as text, links other than `http(s)`, `mailto`, fragments and relative paths become `#`, and Markdown images become links, so a page never loads anything from the network.

### Search

The search box filters the prebuilt index in the browser: every word typed must match a name, method, path, kind, description or parameter name, as a word prefix, a substring, or (three letters or more) the letters of one word in order (`lgn` finds `login`). Redaction placeholders are never indexed. A page opened with `?q=users` starts with that search.

### Themes

The site follows the browser's `prefers-color-scheme`. The toggle in the header stores the choice in local storage; when storage is unavailable (private windows, `file://` restrictions), the system preference is used. Both themes meet WCAG AA contrast for text, checked by a test over the CSS tokens.

## OpenAPI enrichment

For a collection imported with `rustman import openapi`, the import snapshot under `.rustman/openapi/` adds to each request generated from an operation (matched by its `openapi:operationId=` or `openapi:path=` tag): the operation's summary, its description (only when the request file has none: the request file wins), parameter descriptions in the query and header tables, request and response schemas, and a `deprecated` badge on the page and in the navigation.

## The `[docs]` table

All keys are optional:

```toml
[docs]
title = "Shop API"                      # default: the workspace name
logo = "brand/logo.png"                 # a PNG/JPEG/GIF/WebP/ICO file inside the workspace, copied into assets/
examples = ["users/list", "Get user"]   # requests (ids or names) that publish an example
example_redact_paths = ["$.users[*].email"]
publish_variables = ["base_url"]
samples = ["curl", "httpie", "fetch", "python", "rust"]
template_dirs = ["docs-templates"]
```

`samples` and `template_dirs` are described in [Code samples](#code-samples).

## Request file `docs` key

```yaml
version: 1
kind: http
name: List users
docs:
  example: true        # publish the latest recorded exchange
  hidden: true         # leave the request out: no page, navigation or search entry
  description: |
    Extra **Markdown** shown after the request's description.
url: "{{base_url}}/users"
```

## Examples

A request shows a recorded response only when it opts in, with `docs: {example: true}` or by being listed in `[docs].examples`; other requests show none. The example is the latest exchange with a `2xx` status from [request history](history.md), labeled with its recording date, or with `--examples snapshot` the request's stored `default` [snapshot](snapshots.md). It shows the status, the response headers (without `Date`, `Server`, `Connection`, `Keep-Alive`, `Transfer-Encoding` and `Content-Length`) and the body, pretty-printed when it is JSON. Bodies longer than 64 KB are cut with a note. The build prints `published example: <id>` for each one on stderr.

Personal data that is not a declared secret can be masked with `example_redact_paths`: every value a JSONPath selects becomes `«redacted:<path>»`.

## Redaction

Every value written to the site passes the [environments](environments.md#redaction) redaction step: the secret values of every environment, read from the secret store, appear as `«redacted:<name>»`, and so do literal credentials written in request and collection files. `Authorization`, `Cookie`, `Set-Cookie`, `Proxy-Authorization` and `X-Api-Key` values are always masked in examples, and so are the values of headers whose name has a credential word (`token`, `secret`, `password`, `passwd`, `apikey`, `auth`, `credential`, or `api` followed by `key`, split on `-` and `_`: `X-Auth-Token`, `X-Client-Secret`); on endpoint pages they are shown only when they are a variable reference such as `Bearer {{token}}`. A literal value of such a header, or of `api_key` auth, is masked in every code sample as `«redacted:<Header-Name>»`. Literal credentials elsewhere in a request file are masked the same way in pages, code samples, examples and the search index: the `user:password` part of a URL (as `«redacted:userinfo»`, and the password alone as `«redacted:password»`), and the values of query parameters and body fields (JSON at any depth, form and multipart fields, GraphQL `variables`, JSON-RPC, MCP and gRPC params) whose name reads like a credential: one containing `password`, `token`, `secret` or `api_key`, or one of the header credential words above (`?api_key=…` becomes `?api_key=«redacted:api_key»`). Values that are `{{variable}}` references, and values shorter than 6 characters, are left as written.

After rendering, a leak scan looks for every known secret value in every output file, as is and in its JSON-escaped, HTML-escaped, URL-encoded, base64 and hex forms. A file with a hit is not written (a previous copy is deleted) and the build fails:

```text
error: secret value found in site/endpoints/users-list.html
```

## Variables

Variables appear by name only: `{{base_url}}/users`. A value is published only when the variable is listed in `[docs].publish_variables` and `--env <name>` selects the environment to take it from; the index page then has a Variables table. A secret cannot be published:

```text
error: token is a secret and cannot be published
```

## Code samples

Each `http` page, and each `graphql` and `jsonrpc` (over HTTP) page, which are sent as a POST, shows code samples made by the same snippet engine as [`rustman export snippet --placeholders`](snippets.md), so with no environment selected the two print the same code, byte for byte, for requests without secret or dynamic variables (the differences are listed below). The default tabs are `curl`, `httpie`, `fetch` (JavaScript), `python` (Requests) and `rust` (reqwest), in that order; the tabs work without JavaScript. JSON, form, multipart and file bodies use each language's own API.

Samples never read an environment or the secret store:

- Variables stay as `{{name}}` in curl and HTTPie, and become constants declared at the top of code (`BASE_URL = "{{base_url}}"`). Values from `rustman.toml [variables]`, collection and request `variables` are workspace content and fill those constants.
- A variable declared as a secret in any environment is `«redacted:name»`.
- Dynamic values (`{{$uuid}}`, `{{$timestamp}}`, `{{$env NAME}}`) are kept as written, so builds are reproducible and nothing is read from the machine that builds.

`[docs].samples` selects and orders the languages, with any id `rustman export snippet --list` prints or the aliases `fetch`, `python` and `rust`:

```toml
[docs]
samples = ["curl", "go"]
template_dirs = ["templates"]
```

An unknown id fails the build with exit code 2.

### Custom sample templates

A file `<id>.tmpl` in a `[docs].template_dirs` folder (the first folder that has one wins) replaces the built-in template of that language, for docs only: `rustman export snippet` keeps the built-in templates. The template is [minijinja](https://docs.rs/minijinja) and sees the same redacted view and filters as the built-in ones (`method`, `url`, `headers`, `body`, `constants`, the `q`, `qp`, `lit` and `hv` quoting filters, and `comment`). Its first line declares the context version it was written for:

```jinja
{# docs_context_version = 1 #}
{{ comment }} {{ method }} {{ url }}
http.NewRequest({{ method|q }}, {{ url|q }}, nil)
```

A template that declares another version, or none, is skipped with a warning and the built-in template is used:

```text
warning: docs: template templates/go.tmpl targets docs_context_version 0, this rustman provides 1; using the built-in go template
```

A template that fails to render stops the build with exit code 2. Its output passes the redaction step and the leak scan like every other page.

## Markdown output

`--format markdown` writes `README.md` (the index: published variables, then a link to every section file in workspace order, nested sections indented, then the protocol references), `sections/<slug>.md`, `endpoints/<slug>.md` and `reference/<slug>.md`. Tables are GitHub-flavored, code samples are fenced code blocks under one `###` heading per language, and bodies, schemas and examples longer than 20 lines are folded in a `<details>` block so the tables stay readable on GitHub. User text is escaped (Markdown punctuation with backslashes, `<`, `>` and `&` as entities), and descriptions are the same sanitized HTML as the site's, written as one HTML block with no blank line (raw HTML in a description becomes a paragraph of text), so no Markdown renderer parses links or images out of them. Redaction, the leak scan, ordering and determinism are the same as for HTML.

```text
$ rustman docs build --format markdown --out docs/api
wrote 14 files to docs/api/
```

## Protocol references

When the workspace has visible `graphql` requests and a schema for their endpoint, the site gets a **GraphQL schema** section: one page per type (objects, interfaces, unions, enums, input objects and custom scalars, sorted by name), listing fields with their types, arguments and defaults, deprecation reasons and descriptions. The schema is the one `rustman` validates the request against: its `schema_file`, the `schema_file` of its `[[graphql.endpoints]]` entry, or the cached introspection result under `.rustman/graphql/schemas/` (filled by `rustman graphql introspect`).

When it has visible `jsonrpc` requests with an OpenRPC document, from `[jsonrpc.catalogs]` or the `rustman rpc discover` cache under `.rustman/cache/openrpc/`, the site gets a **JSON-RPC methods** section: one page per method with its summary and description, params (name, required, schema), result schema, documented errors and a deprecation badge.

To find a cached schema or document, the request's endpoint is resolved with no environment and then with each environment in turn, without the secret store or the process environment; the resolved URL is only a lookup key and is never written, the pages show the endpoint as the request file writes it. Without such requests, or without a schema or document, no section is generated. Each distinct schema or document gets one section.

## Layout templates

The pages are rendered with [minijinja](https://docs.rs/minijinja) templates: `layout.html`, `index.html`, `endpoint.html` and `reference.html`. A file with one of these names in a `[docs].template_dirs` folder replaces the built-in one. Templates see the page model only (`site`, `endpoint`, `reference` and its `set`, `assets`, `root`, `version`), whose values are already redacted and are HTML-escaped when printed; descriptions are HTML made from Markdown and are printed with `|safe`. Templates are rendered with `trim_blocks` and `lstrip_blocks`: a block tag (`{% if %}`, `{% for %}`) on a line of its own writes nothing, not even the line break. The leak scan also checks the output of custom templates.

## The output folder

`--out` defaults to `site/`. A build replaces only the files listed in the folder's `.rustman-docs-manifest` and removes the ones it no longer produces, so files you add (a `CNAME`, say) stay. A non-empty folder without a manifest is refused:

```text
error: site/ is not empty and was not generated by rustman docs; use --force
```

Two builds of the same workspace, history and options are byte-identical, in both formats: files and navigation are sorted (workspace order, then byte-wise), GraphQL types and JSON-RPC methods are sorted by name, there are no build timestamps (the version is in the footer; example dates come from the recorded exchange), files are UTF-8 without a BOM with `\n` line endings on every platform, anchors are stable slugs, and assets are named after their content hash. A golden test checks the hash of every file on Linux, macOS and Windows.

### Publishing to GitHub Pages

The site needs no server: publish the folder as is. A workflow that builds the docs on every push to `main` and deploys them with GitHub's Pages actions (set **Settings → Pages → Source** to **GitHub Actions**):

```yaml
name: docs
on:
  push:
    branches: [main]
permissions:
  contents: read
  pages: write
  id-token: write
jobs:
  deploy:
    runs-on: ubuntu-latest
    environment:
      name: github-pages
      url: ${{ steps.deployment.outputs.page_url }}
    steps:
      - uses: actions/checkout@v4
      - name: Install rustman
        run: cargo install --git https://github.com/CyberdyneCorp/rustman rustman-cli --locked
      - run: rustman docs build --out site
      - uses: actions/upload-pages-artifact@v3
        with:
          path: site
      - id: deployment
        uses: actions/deploy-pages@v4
```

The runner has no secret store, so secret values are unknown there: requests show `{{token}}` references as usual, and a literal credential written in a request file is still redacted. Builds that publish examples need the history, which lives in `.rustman/` and is not committed; build those locally instead. Because the output is deterministic, committing `site/` and reviewing `git diff site/` also works, and `--format markdown --out docs/api` gives pages GitHub renders directly.

## Preview

`rustman docs serve` builds the HTML site into a temporary folder and serves it on `127.0.0.1` only (never on other interfaces; a request whose `Host` is not `localhost`, `127.0.0.1` or `[::1]` gets `421`, so a DNS-rebinding page cannot read it), on `--port` (default 4000; `0` picks a free port). It takes `--env` and `--examples` like `docs build`, writes nothing into the workspace, and runs until Ctrl+C:

```text
$ rustman docs serve
docs: built 12 files
docs: serving on http://127.0.0.1:4000
```

Every change under the workspace folder (except `.git/`) rebuilds the site once changes stop for 200 ms, into a fresh folder that replaces the served one at once, and open pages reload, typically within half a second of saving. Pages get a small inline reload script that listens on `/__rustman/reload` (server-sent events); it is added as pages are served and is never part of `docs build` output.

Every response carries `X-Content-Type-Options: nosniff`, and files other than pages (stylesheets, scripts, the logo) are also served with `Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; sandbox`, so a file opened on its own runs no script. The `logo` is a raster image only (PNG, JPEG, GIF, WebP or ICO): an SVG logo is refused with exit code 2, because a static host would serve it without such a policy and its scripts would run on the docs origin.

When a build fails, the error is printed on stderr and shown in an overlay at the bottom of every page, over the last good build, and the server keeps running; the next successful build removes it. Build messages pass the same redaction as the site. A port that is already taken exits 2:

```text
$ rustman docs serve --port 4000
error: docs: cannot bind 127.0.0.1:4000: address in use
```

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | The site was written. |
| 1 | The leak scan found a secret value in an output file; that file was not written. |
| 2 | Usage or configuration error: a non-empty `--out` without a manifest (without `--force`), a secret in `publish_variables`, an unknown `--env`, an invalid `example_redact_paths` JSONPath or template, an unknown `samples` language, a custom sample template that fails to render, a `logo` outside the workspace or not a PNG, JPEG, GIF, WebP or ICO image; for `docs serve`, a port that cannot be bound. |

`docs serve` exits 0 when stopped with Ctrl+C; build errors while it runs do not stop it.
