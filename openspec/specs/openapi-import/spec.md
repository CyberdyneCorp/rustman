# openapi-import Specification

## Purpose
Turns an OpenAPI document into ready-to-send workspace content (collections, request files, environments and auth config) and keeps that content in sync as the document evolves, without discarding the user's edits.

## Requirements

### Requirement: Supported document versions and formats
The system SHALL import OpenAPI 3.0.x and 3.1.x documents encoded as JSON or YAML, detected by content rather than file extension. A Swagger 2.0 document SHALL be rejected with exit code 2 and the message `Swagger 2.0 is not supported; convert it to OpenAPI 3 first (e.g. with swagger2openapi)`.

#### Scenario: YAML 3.1 document
- **GIVEN** a file `petstore.yaml` declaring `openapi: 3.1.0`
- **WHEN** the user runs `rustman import openapi petstore.yaml`
- **THEN** the import SHALL succeed with exit code 0

#### Scenario: Swagger 2.0 rejected
- **GIVEN** a document declaring `swagger: "2.0"`
- **WHEN** it is imported
- **THEN** the command SHALL exit with code 2 and print the conversion hint
- **AND** no workspace file SHALL be created or modified

### Requirement: Document sources
The source argument SHALL accept a local path or an `http(s)` URL. URL fetches SHALL use the `http-requests` engine with TLS verification on, a 30 s timeout and a 20 MiB size cap. Exceeding the cap SHALL fail with exit code 2.

#### Scenario: Import from URL
- **WHEN** the user runs `rustman import openapi https://api.example.com/openapi.json`
- **THEN** the document SHALL be downloaded and imported

#### Scenario: Oversized document
- **GIVEN** a URL serving a 25 MiB document
- **WHEN** it is imported
- **THEN** the command SHALL exit with code 2 with a message naming the 20 MiB limit

### Requirement: Reference resolution
Local `$ref`s (`#/...` and relative file paths beside a local source) SHALL be resolved. A file reference SHALL stay in the root document's directory or below; any other file reference SHALL be refused. Remote `$ref`s (`http(s)://`) SHALL be refused unless `--allow-remote-refs` is given. Circular references SHALL be preserved as references, not expanded infinitely.

#### Scenario: Remote ref refused by default
- **GIVEN** a document containing `$ref: https://schemas.example.com/user.json`
- **WHEN** it is imported without `--allow-remote-refs`
- **THEN** the command SHALL exit with code 2 naming the unresolved remote reference

#### Scenario: File ref outside the document directory refused
- **GIVEN** a local document containing `$ref: ../shared.yaml#/Pet` or an absolute file path
- **WHEN** it is imported
- **THEN** the command SHALL exit with code 2 saying the reference leaves the document's directory, and the file SHALL NOT be read

#### Scenario: Circular schema
- **GIVEN** a `Node` schema whose `children` items reference `Node`
- **WHEN** the document is imported
- **THEN** the import SHALL complete and the synthesized example SHALL stop at a nesting depth of 3

### Requirement: Validation diagnostics
Before generating anything, the system SHALL validate the document and print diagnostics with a JSON pointer location. Structural errors SHALL abort with exit code 2. Non-fatal issues, such as a missing `operationId` or examples that don't match their schema, SHALL be reported as warnings and the import SHALL continue.

#### Scenario: Missing operationId warning
- **GIVEN** an operation without `operationId`
- **WHEN** the document is imported
- **THEN** a warning with pointer `/paths/~1users/get` SHALL be printed
- **AND** the request file SHALL be named from method and path

### Requirement: One request file per operation grouped by tag
The import SHALL create a collection named after the API title (or `--into <dir>`), with one sub-collection per first tag (`untagged` when none) and one `kind: http` request file per operation, named from `operationId` in kebab-case (otherwise `<method>-<path-slug>`). Each request file SHALL record its `operationId`, method and path template.

#### Scenario: Tagged operations
- **GIVEN** operations `listPets` and `createPet` tagged `pets`
- **WHEN** the document is imported
- **THEN** the files `<api>/pets/list-pets.rman.yaml` and `<api>/pets/create-pet.rman.yaml` SHALL exist

#### Scenario: Name collision
- **GIVEN** two operations that produce the same file name
- **WHEN** imported
- **THEN** the second SHALL be suffixed `-2` and a warning SHALL be printed

### Requirement: Parameters and example bodies
Path, query and header parameters SHALL become `{{variables}}` named after the parameter. Bodies SHALL use, in order: the media type's `example`, its first `examples` entry, the schema's `example`/`default`, or a value synthesized from the schema that satisfies its required properties, types, formats and enums. The first JSON media type SHALL be preferred.

#### Scenario: Synthesized body
- **GIVEN** a request schema with required `name` (string) and `age` (integer, minimum 0) and no examples
- **WHEN** the operation is imported
- **THEN** the body SHALL be valid JSON containing `name` as a string and `age` as an integer ≥ 0

#### Scenario: Path parameter
- **GIVEN** path `/pets/{petId}`
- **WHEN** imported
- **THEN** the request URL SHALL be `{{baseUrl}}/pets/{{petId}}`

### Requirement: Servers become environments
Each `servers` entry SHALL become an environment that defines `baseUrl`. Its name SHALL come from the server `description` in kebab-case, falling back to `server-<n>`. Server variables SHALL be substituted with their defaults and also exposed as environment variables. Existing environments with the same name SHALL NOT be overwritten unless `--sync` is given.

#### Scenario: Two servers
- **GIVEN** servers `https://staging.example.com` ("Staging") and `https://api.example.com` ("Production")
- **WHEN** imported
- **THEN** environments `staging` and `production` SHALL exist with the respective `baseUrl`

### Requirement: Security schemes become auth config
`securitySchemes` SHALL map to the `http-requests` auth helpers: `http/basic`, `http/bearer`, `apiKey` (header, query or cookie) and `oauth2` (clientCredentials, authorizationCode with PKCE). Credential values SHALL be emitted as secret placeholders and never written in plain text. `openIdConnect` and `mutualTLS` SHALL produce a warning and be left unconfigured.

#### Scenario: Bearer scheme
- **GIVEN** a global `bearerAuth` scheme of type `http`, scheme `bearer`
- **WHEN** imported
- **THEN** generated requests SHALL use bearer auth with token `{{bearerAuth_token}}`, declared as a secret with no value

### Requirement: Import snapshot
Every successful import SHALL store the normalized document at `.rustman/openapi/<collection>.json` and record the source and its hash in the collection metadata, so later syncs can compute three-way merges.

#### Scenario: Snapshot written
- **WHEN** an import completes
- **THEN** `.rustman/openapi/<collection>.json` SHALL exist and the collection metadata SHALL contain the source and its SHA-256 hash

### Requirement: Sync preserves user edits
`rustman import openapi <src> --sync` SHALL three-way merge the snapshot (base), the current request files (local) and the new document (remote). It SHALL add new operations and update fields the user hasn't changed. It SHALL keep user-edited fields, user-added assertions, captures and variables. Removed operations SHALL be moved to an `_removed/` sub-collection, not deleted.

#### Scenario: User-edited body kept
- **GIVEN** the user edited the body of `create-pet` after import
- **AND** the new document changes that operation's example
- **WHEN** the user syncs
- **THEN** the user's body SHALL be kept and the conflict SHALL be listed as `kept local`

#### Scenario: Operation removed upstream
- **GIVEN** `deletePet` no longer exists in the new document
- **WHEN** the user syncs
- **THEN** its request file SHALL move to `<api>/_removed/` and be reported as removed

### Requirement: Sync change summary
Every sync SHALL print, and with `--json` emit, a summary of added, removed, changed and conflicting operations with their `operationId`s. `--dry-run` SHALL print the summary without writing any file.

#### Scenario: Dry-run sync
- **WHEN** the user runs `rustman import openapi new.yaml --sync --dry-run`
- **THEN** the summary SHALL be printed
- **AND** no workspace file SHALL change
