# Proposal

## Why

Most teams already have API collections in Postman, Insomnia, Bruno, browser HAR captures or `.http` files. If moving them into Rustman means retyping them, people won't switch. Importers turn those files into a Rustman workspace in one command. They never write a secret into a tracked file, and they report exactly what couldn't be carried over. Exporters to Postman v2.1 and `.http` let teams keep working with colleagues who haven't switched.

## What Changes

- `rustman import <format> <src> [--into <dir>] [--dry-run] [--on-conflict skip|overwrite|rename]`, where `<format>` is one of `postman`, `insomnia`, `bruno`, `har`, `http`, or `auto` (detected from content).
- Supported sources:

| Source | What's imported |
|--------|-----------------|
| Postman Collection v2.0/v2.1 | folders, requests, auth types, body modes, collection and folder variables, environment files |
| Insomnia | v4 JSON export and v5 YAML export |
| Bruno | `.bru` collection folders, plus `environments/*.bru` |
| HAR 1.2 | deduplicated; filtered by host, method and content type; static assets dropped by default |
| `.http` files (JetBrains and VS Code REST Client) | `###` separators, `@var` definitions, `http-client.env.json` |

- Postman pre-request and test scripts are **never executed**. They're kept as comments in the request file and listed in the import report. Trivial patterns (`pm.response.to.have.status(200)`, `pm.expect(pm.response.code).to.eql(201)`) get a suggested declarative `assertions` entry.
- Every source's variable syntax is translated to Rustman's `{{var}}` placeholders and dynamic variables.
- Secrets are detected in Authorization values, auth fields, password-like and token-like variables, and API-key headers or query parameters. They're moved into `environments` secrets with placeholders, never written into request files, and listed by name in the report.
- GraphQL bodies become `kind: graphql` request files.
- Output is deterministic: slug-based file names with collision suffixes, and stable ordering through `_collection.yaml`. Re-importing the same source is idempotent, with a configurable conflict policy.
- A machine-readable import report (`--report <file>`, JSON) and a human summary list counts, unsupported features with their source locations, and the secrets that were moved.
- `rustman export postman|http <path> [-o <file>]` writes a collection out as Postman v2.1 or `.http`, with secrets kept as placeholders.

## Capabilities

### New Capabilities

- `collection-import`: converts third-party collection formats (Postman, Insomnia, Bruno, HAR, `.http`) into workspace request files, collections and environments, and exports workspaces back to Postman v2.1 and `.http`.

### Modified Capabilities

None. Uses `workspace` (request files, `_collection.yaml`, deterministic writing), `environments` (variables, secrets, redaction) and `assertions` (suggested entries) without changing their requirements. `curl` import and export stay in `http-requests`, and OpenAPI import stays in `openapi-import`.

## Out of Scope

- Executing Postman, Insomnia or Bruno scripts, or translating scripts beyond the documented trivial patterns.
- Importing Postman monitors, mock servers, API definitions or cloud workspace sync. Only exported files are read; the Postman API is never called.
- SoapUI, Paw/RapidAPI and Thunder Client formats (they can come later as plugins).
- Exporting to Insomnia or Bruno.

## Impact

- New crate `crates/rustman-import`, used by the `rustman-cli` subcommands `import` and `export`.
- Dependencies: `serde_json`, the workspace's YAML crate, a small hand-written `.bru` and `.http` parser (no new heavy dependencies).
- The TUI gets an "Import…" palette action that runs the same engine with `--dry-run` preview (layout from `add-terminal-ui`).
