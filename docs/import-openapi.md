# OpenAPI import

`rustman import openapi` turns an OpenAPI document into a collection of request files: one folder per tag and one `kind: http` request per operation.

```text
rustman import openapi petstore.yaml
rustman import openapi https://api.example.com/openapi.json --into apis/shop
```

It prints `imported <n> operations into <dir>` and one `created environment '<name>'` line per new environment on stdout, and every warning on stderr. `--sync` updates a collection imported earlier (see [Sync](#sync)). The behavior is specified in [`openspec/specs/openapi-import`](https://github.com/CyberdyneCorp/rustman/blob/main/openspec/specs/openapi-import/spec.md).

## Supported documents

- OpenAPI **3.0.x** and **3.1.x**, in JSON or YAML. The format is detected from the content, not the file extension: a document whose first character is `{` or `[` is JSON, anything else is YAML.
- **Swagger 2.0** is rejected with exit code 2 and `Swagger 2.0 is not supported; convert it to OpenAPI 3 first (e.g. with swagger2openapi)`. Nothing is written.
- Any other `openapi` version, or a document without an `openapi` field, exits with 2.

OpenAPI 3.0 schemas are rewritten to their 3.1 (JSON Schema 2020-12) form when they are read: `nullable: true` becomes a `"null"` type (or an `anyOf` with `{"type": "null"}` when the schema has no `type`), and boolean `exclusiveMinimum` / `exclusiveMaximum` become numbers. A 3.0 document and its 3.1 equivalent import to the same result.

## Sources

The source is a local path or an `http(s)` URL. A URL is fetched with the same HTTP engine as `rustman send`, with TLS verification on, a 30-second timeout and redirects followed. A document (or a referenced file) larger than **20 MiB** is refused with exit code 2:

```text
error: https://api.example.com/huge.json is larger than the 20 MiB limit
```

A URL that cannot be reached (DNS, connection refused, timeout, TLS) exits with 3, like other requests that could not complete.

## References

- Local references (`#/components/schemas/Pet`) are resolved.
- References to files beside a local document (`common/pet.yaml#/Pet`, `schemas/shared.json`) are read and resolved, including the references inside them. They must stay in the root document's directory or below: an absolute path, or one that climbs out of it with `..` (or through a symlink), is refused with exit code 2 (`reference '../shared.json' at ... leaves the document's directory`), so an untrusted document cannot read other files.
- References to other `http(s)` documents, and relative references in a document read from a URL, are **refused** with exit code 2 unless `--allow-remote-refs` is given:

  ```text
  error: remote reference 'https://schemas.example.com/user.json' at /paths/~1users/get/responses/200/content/application~1json/schema is not resolved; pass --allow-remote-refs to fetch it
  ```

- Circular references (a `Node` whose `children` are `Node`s) are kept as references and never expanded.

Referenced files are copied into the imported model under `components/x-rustman-external`; at most 64 documents are read per import.

## Diagnostics

The document is validated before anything is written. Each diagnostic names a JSON pointer:

```text
warning: /paths/~1users/get: operation has no operationId; its request file is named from the method and path
error: /info/title: required string 'title' is missing
```

Errors stop the import with exit code 2, and nothing is written. They are: a missing `info`, `info.title` or `info.version`; a `paths` that is not an object (or is missing in 3.0); a path that does not start with `/`; an operation or path item that is not an object; a parameter without `name` or a valid `in`; and a `$ref` that points at nothing.

Warnings are printed and the import continues: a missing or duplicate `operationId`, an operation without responses, a path parameter that is not `required` or does not match the path template, and an `example` (or `examples.*.value`) that does not match its schema (the first violation is quoted).

## The generated collection

The collection is a folder in the workspace root named after `info.title` in kebab-case, or the folder given with `--into`. That folder must not exist or must be empty; an existing collection is never overwritten. Files are written to a temporary folder first and moved into place, so a failed import leaves nothing behind.

```text
swagger-petstore/
  _collection.yaml        # name, order of the tag folders, auth, source and its hash
  pets/
    _collection.yaml      # name and description of the tag, order of the requests
    list-pets.rman.yaml
    create-pet.rman.yaml
    show-pet-by-id.rman.yaml
  store/
    _collection.yaml
    get-inventory.rman.yaml
  untagged/               # operations without tags
    get-users.rman.yaml
environments/
  production.yaml         # one environment per server
.rustman/openapi/
  swagger-petstore.json   # the import snapshot (git-ignored)
```

- Each operation goes to the folder of its **first tag**, or `untagged`.
- A request file is named from the `operationId` in kebab-case (`getPetByID` → `get-pet-by-id.rman.yaml`), otherwise from the method and path (`GET /users/{id}/posts` → `get-users-id-posts.rman.yaml`). When two operations in a folder get the same name, the second is suffixed `-2` (then `-3`, ...) and a warning is printed.
- The request's `name` is the `operationId` (or `METHOD /path`), and its `description` is the operation's summary and description.
- The URL is `{{baseUrl}}` followed by the path, with every path parameter as a variable: `/pets/{petId}` becomes `{{baseUrl}}/pets/{{petId}}`.
- Query and header parameters become `query` and `headers` rows whose value is a `{{variable}}` named after the parameter. Optional parameters are written disabled (`enabled: false`), so they are kept in the file but not sent. `Accept`, `Content-Type` and `Authorization` header parameters are ignored, as OpenAPI specifies.
- Two tags record where the request came from: `openapi:operationId=<id>` and `openapi:path=<template>`. They let a later re-import match request files to operations.

```yaml
version: 1
kind: http
name: showPetById
description: Info for a specific pet
tags:
  - openapi:operationId=showPetById
  - openapi:path=/pets/{petId}
method: GET
url: '{{baseUrl}}/pets/{{petId}}'
```

Variables such as `baseUrl` and `petId` come from environments, `--var` or captures, as for any request (see [Environments](environments.md)).

## Example bodies

An operation with a request body gets a `body` from the first JSON media type (`application/json` or any `+json` type), or the first media type when none is JSON. The value is, in order:

1. the media type's `example`;
2. the value of its first `examples` entry;
3. the schema's own example (`examples[0]` or `example`) or `default`;
4. a value **synthesized** from the schema.

The synthesizer follows `$ref`s and builds a value that validates against the schema:

- objects get their `required` properties (every property that is not `readOnly` when nothing is required); `allOf` members are merged, and `oneOf`/`anyOf` take the first branch that is not `null`;
- `const`, then the first non-null `enum` value, then a nested `example`/`default` win over anything synthesized;
- strings follow their `format` (`date-time` → `2024-01-01T00:00:00Z`, `email` → `user@example.com`, `uuid`, `uri`, `ipv4`, ...; otherwise `string`), padded with `x` to `minLength` (at most 4096 characters) and cut to `maxLength`;
- integers and numbers start at 0 and move inside `minimum`/`maximum`, `exclusiveMinimum`/`exclusiveMaximum` and onto `multipleOf`; booleans are `true`;
- arrays get one item (raised to `minItems`, lowered to `maxItems`);
- objects and arrays stop at a nesting depth of 3: deeper ones are empty, so a circular schema (a `Node` whose `children` are `Node`s) gives `{children: [{children: []}]}`.

JSON bodies are written as `type: json`, `application/x-www-form-urlencoded` as `type: form` and a string example of a `text/*` or XML type as `type: text`. When the media type is not the default of the body type (for example `application/vnd.api+json`), a `Content-Type` header is added. Other media types (such as `multipart/form-data`) get no body and a warning.

## Environments

Each entry of `servers` becomes `environments/<name>.yaml`, named from its `description` in kebab-case (`server-1`, `server-2`, ... without one). It defines `baseUrl` (the URL with each server variable replaced by its `default`, without a trailing `/`) and every server variable, and declares the auth [secrets](environments.md) without values. A relative server URL is resolved against the document's URL; in a local file it is kept as is, with a warning.

An environment file that already exists is never overwritten by an import: a warning says so, and `--sync` merges it instead.

## Auth

`components.securitySchemes` become auth config. The document-wide `security` sets the `auth` of the collection's `_collection.yaml`; an operation with its own `security` gets its own `auth` (`type: none` for `security: []`). The first alternative of a requirement is used.

| Scheme | Auth | Secrets |
| --- | --- | --- |
| `http`, `basic` | `basic` | `<scheme>_username`, `<scheme>_password` |
| `http`, `bearer` | `bearer` | `<scheme>_token` |
| `apiKey` in `header` / `query` | `api_key` in that location | `<scheme>_key` |
| `apiKey` in `cookie` | `api_key` header `Cookie: <name>={{<scheme>_key}}` | `<scheme>_key` |
| `oauth2` with `clientCredentials` (else `authorizationCode`, sent with PKCE) | `oauth2` with the flow's URLs and scopes | `<scheme>_client_id`, `<scheme>_client_secret` |

Credentials are always `{{variables}}` declared as secrets: no credential value is ever written to a file. Set them with [`rustman secret set`](environments.md). `openIdConnect`, `mutualTLS`, other `http` schemes (such as `digest`) and `oauth2` with only the `implicit` or `password` flow are left unconfigured, with a warning:

```text
warning: /components/securitySchemes/oidc: security scheme 'oidc' (openIdConnect) is not configured; set up its auth by hand
```

## Secret references in the document

The document cannot name your secrets. A [secret reference](secrets.md) (`{{secret:...}}`) in a path or parameter name, an example body, a server URL or variable default, or a security scheme is written escaped, as `\{{secret:...}}`: literal text that is sent as written and never reads a provider secret. Each one gets a warning at its JSON pointer, in `rustman import openapi` (also with `--sync`) and in the `import_openapi` agent tool:

```text
warning: /paths/~1leak/post: secret reference {{secret:vault/kv/prod/db#password}} from the document neutralized: it is kept as literal text and never resolved
```

## Import snapshot

Every import stores the bundled, normalized document at `.rustman/openapi/<collection>.json` (`<collection>` is the collection folder's path in kebab-case) and records where it came from in the collection's `_collection.yaml`:

```yaml
source:
  format: openapi
  location: shop.yaml
  sha256: '<the SHA-256 of the document text>'
```

`location` is the path relative to the workspace root, or the URL without any `user:password@`. The snapshot is the base of the next sync. It lives in `.rustman/`, which is git-ignored: a teammate who syncs a collection imported elsewhere imports it again into an empty folder instead.

## Sync

`rustman import openapi <src> --sync` brings a collection imported earlier up to date with a new version of the document. It is a three-way merge: the snapshot is the base, the request files are local, the new document is remote. For each operation (matched by the `openapi:operationId=` tag, or method and `openapi:path=`):

- a field (`name`, `description`, `method`, `url`, `body`, `auth`) takes the new value only when you have not changed it; when both changed, yours is kept and the operation is listed as a conflict, `kept local`;
- `query`, `headers` and `tags` merge row by row: rows you added always stay, rows you deleted stay deleted, and new rows from the document are added;
- `settings`, `variables`, `assertions` and `captures` are never touched;
- new operations are written (new folders get their `_collection.yaml`, existing folders get the new requests in `order`);
- removed operations are moved to `<collection>/_removed/`, never deleted. Folders whose name starts with `_` are not run.

Environments are merged the same way, variable by variable, and the snapshot and `source` are replaced. A collection without a snapshot is not synced (exit code 2). `--sync` on a folder that does not exist or is empty is a plain import.

Every sync prints a summary; `--json` prints it as JSON (`added`, `removed`, `changed`, `conflicts`, `unchanged`, `environments`, `dryRun`), and `--dry-run` prints it without writing any file.

### Walkthrough

```yaml
# shop.yaml
openapi: 3.1.0
info:
  title: Shop
  version: '1'
servers:
  - url: https://api.example.com/v1
    description: Production
security:
  - bearerAuth: []
paths:
  /pets:
    post:
      operationId: createPet
      tags: [pets]
      requestBody:
        content:
          application/json:
            schema:
              type: object
              required: [name, age]
              properties:
                name: {type: string}
                age: {type: integer, minimum: 1}
      responses:
        '201': {description: Created}
  /pets/{petId}:
    delete:
      operationId: deletePet
      tags: [pets]
      parameters:
        - {name: petId, in: path, required: true, schema: {type: string}}
      responses:
        '204': {description: Deleted}
components:
  securitySchemes:
    bearerAuth: {type: http, scheme: bearer}
```

```console
$ rustman import openapi shop.yaml
imported 2 operations into shop
created environment 'production'
```

The body is synthesized from the schema, and the token is a secret placeholder:

```yaml
# shop/pets/create-pet.rman.yaml (generated)
version: 1
kind: http
name: createPet
tags:
  - openapi:operationId=createPet
  - openapi:path=/pets
method: POST
url: '{{baseUrl}}/pets'
body:
  type: json
  content:
    name: string
    age: 1
```

```yaml
# environments/production.yaml (generated)
variables:
  baseUrl: https://api.example.com/v1
secrets:
  - bearerAuth_token
```

You edit the body:

```yaml
# shop/pets/create-pet.rman.yaml
version: 1
kind: http
name: createPet
tags:
  - openapi:operationId=createPet
  - openapi:path=/pets
method: POST
url: '{{baseUrl}}/pets'
body:
  type: json
  content:
    name: Rex
    age: 3
```

The next version of the document adds an example, drops `deletePet` and adds `listPets`:

```yaml
# shop-v2.yaml
openapi: 3.1.0
info:
  title: Shop
  version: '2'
servers:
  - url: https://api.example.com/v2
    description: Production
security:
  - bearerAuth: []
paths:
  /pets:
    get:
      operationId: listPets
      tags: [pets]
      responses:
        '200': {description: OK}
    post:
      operationId: createPet
      tags: [pets]
      requestBody:
        content:
          application/json:
            schema:
              type: object
              required: [name, age]
              properties:
                name: {type: string}
                age: {type: integer, minimum: 1}
            example: {name: Fido, age: 2}
      responses:
        '201': {description: Created}
components:
  securitySchemes:
    bearerAuth: {type: http, scheme: bearer}
```

```console
$ rustman import openapi shop-v2.yaml --sync --dry-run
added: listPets (pets/list-pets.rman.yaml)
conflict: createPet body: kept local
removed: deletePet (moved to _removed/delete-pet.rman.yaml)
environment production: updated
1 added, 0 changed, 1 removed, 1 conflict, 0 unchanged (dry run: nothing was written)
$ rustman import openapi shop-v2.yaml --sync
added: listPets (pets/list-pets.rman.yaml)
conflict: createPet body: kept local
removed: deletePet (moved to _removed/delete-pet.rman.yaml)
environment production: updated
1 added, 0 changed, 1 removed, 1 conflict, 0 unchanged
```

Your body is kept, `deletePet` is in `shop/_removed/`, and the environment points at the new server:

```yaml
# environments/production.yaml (generated)
variables:
  baseUrl: https://api.example.com/v2
secrets:
  - bearerAuth_token
```

The commands on this page run in CI: `crates/rustman-cli/tests/docs_import_openapi.rs` writes each `# <file>` block, runs each `$ rustman ...` line and compares its output and every `(generated)` block with what the import wrote.
