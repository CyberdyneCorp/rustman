# Contract rule catalog

<!-- Generated from crates/rustman-contract/src/catalog.rs; regenerate with
RUSTMAN_UPDATE_DOCS=1 cargo test -p rustman-contract --test catalog -->

`rustman contract diff` compares two OpenAPI 3.0/3.1 documents and reports every difference with one of the rules below. `rustman contract rules` prints the same list (`--json` for a JSON array).

Catalog version: **2**. The version is recorded in every diff and changes whenever a rule is added, removed or reclassified.

## How differences are classified

Documents are compared after loading them as [OpenAPI import](import-openapi.md) does: references bundled, OpenAPI 3.0 schemas rewritten to 3.1 (`nullable: true` is the same as a `null` type) and path-item parameters merged into each operation. Operations are matched by method and path template, ignoring parameter names (`/users/{id}` and `/users/{userId}` are the same operation, reported as `path-parameter-renamed`). Path parameters are matched by position, other parameters by name and location (header names ignore case).

Schemas are compared with a direction:

- **Request** schemas (parameters and request bodies) break clients when they get stricter: a new required field, a narrowed enum, a tighter constraint, a type that rejects old values.
- **Response** schemas break clients when they lose guarantees: a removed field, a field that is no longer required, a type that accepts new kinds of values, a narrowed enum. A widened response enum or a looser response constraint may surprise clients and is `unknown`.

`$ref`s are followed (each difference points to where the change is, such as `/components/schemas/User/properties/email`), and recursive schemas are compared once per level. The members of an `allOf` add up to one object: their properties, `required` lists and types are compared like those of any object, so moving a field between members, or wrapping a schema in an `allOf`, is not a change, and a field that disappears with a removed member is. What else an `allOf` member says (constraints, enums, nested `oneOf`), and any change inside `oneOf`, `anyOf` and `not`, is not proven equivalent or compatible: it is reported as `composition-changed` (`unknown`).

Response headers are compared like parameters, by name (ignoring case), and their schemas in the response direction.

`items` and `additionalProperties` are constraints too. Absent, `true` and an empty schema accept anything, `false` accepts nothing, and a schema is in between: a move towards `false` (`additionalProperties` absent -> `{type: integer}` -> `false`, or `items` appearing) is stricter, a move the other way looser, and two schemas are compared member by member. `additionalProperties: false` in an `allOf` member closes the whole object. `multipleOf` is compared by exact decimal divisibility: `10 -> 5` is looser (every multiple of 10 is a multiple of 5), `5 -> 10` stricter, and `4 -> 6` both, so it is breaking in a request and `unknown` in a response. Values too large or too precise to compare exactly are `constraint-changed`.

A removed response status that the new document still covers is not `response-status-removed`: OpenAPI lets a range (`1XX` to `5XX`) stand for every code of its class that is not declared, and `default` for every status that is not declared otherwise. So when `200` gives way to `2XX` (or, without `2XX`, to `default`), and when a range gives way to `default`, the old response is compared with the one that now covers it, as `response 200 (now 2XX)`.

Each difference carries a JSON pointer into each document. When an element exists in one document only, the pointer into the other is the closest enclosing element (for a removed operation, its path item or `/paths`).

## Breaking

| Rule | Description | Example |
|------|-------------|---------|
| `operation-removed` | An operation is removed. | `DELETE /users/{id}` is in the old document only |
| `response-status-removed` | A response status is removed from an operation. | `GET /users/{id}` no longer declares `404` |
| `response-media-type-removed` | A response media type is removed. | the `200` response drops `application/xml` |
| `response-field-removed` | A response field is removed or renamed. | `$.email` disappears from the `200` body |
| `response-field-became-optional` | A response field is no longer required. | `email` leaves the `required` list of the `200` body |
| `response-field-type-changed` | A response field's type changes or accepts more types. | `$.id` goes from `integer` to `string` |
| `response-header-removed` | A response header is removed. | the `200` response no longer declares `X-Rate-Limit` |
| `response-header-became-optional` | A response header is no longer required. | `X-Rate-Limit` changes to `required: false` in the `200` response |
| `response-enum-narrowed` | Values are removed from a response enum. | `$.status` goes from `[active, suspended, deleted]` to `[active, suspended]` |
| `request-required-field-added` | A request body field becomes required, or a new required field appears. | `email` is added to the `required` list of the `POST /users` body |
| `request-field-removed-from-closed-schema` | A request body field is removed from a schema with `additionalProperties: false`; clients that still send it are rejected. | `POST /users` no longer declares `nickname` and allows no other properties |
| `request-required-parameter-added` | A new required parameter appears. | `GET /users` gains a required query parameter `tenant` |
| `parameter-became-required` | An optional parameter becomes required. | query parameter `limit` changes to `required: true` |
| `request-body-became-required` | A request body becomes required, or a new required body appears. | `PATCH /users/{id}` changes its body to `required: true` |
| `request-media-type-removed` | A request body media type is removed. | `POST /users` no longer accepts `application/x-www-form-urlencoded` |
| `request-type-changed` | A request field's type changes to one that rejects old values. | `$.age` goes from `[integer, string]` to `integer` |
| `request-enum-narrowed` | Values are removed from a request enum. | query parameter `sort` goes from `[asc, desc]` to `[asc]` |
| `request-constraint-tightened` | A request constraint gets stricter (`maxLength`, `maximum`, `pattern`, `minItems`, `additionalProperties`, `items`, ...). | `$.name` goes from `maxLength: 100` to `maxLength: 50` |
| `parameter-location-changed` | A parameter (or an API key) moves to another location. | `api_key` moves from a query parameter to a header |
| `security-requirement-removed` | Every security alternative an operation accepted is removed. | `GET /users` accepted `basicAuth` and now accepts only `bearerAuth` |

## Non-breaking

| Rule | Description | Example |
|------|-------------|---------|
| `operation-added` | An operation is added. | `GET /health` is in the new document only |
| `operation-deprecated` | An operation is marked deprecated. | `GET /users/{id}` gains `deprecated: true` |
| `path-parameter-renamed` | A path parameter is renamed; the path template is the same. | `/users/{id}` becomes `/users/{userId}` |
| `response-status-added` | A response status is added to an operation. | `GET /users/{id}` declares a new `429` |
| `response-media-type-added` | A response media type is added. | the `200` response adds `application/xml` |
| `response-field-added` | A response field is added to a schema that allows other properties. | `$.nickname` appears in the `200` body |
| `response-field-became-required` | A response field becomes required. | `nickname` joins the `required` list of the `200` body |
| `response-header-added` | A response header is added. | the `200` response declares a new `X-Request-Id` |
| `response-header-became-required` | A response header becomes required. | `X-Request-Id` changes to `required: true` in the `200` response |
| `response-type-narrowed` | A response field accepts fewer types. | `$.nickname` goes from `[string, null]` to `string` |
| `response-constraint-tightened` | A response constraint gets stricter. | `$.name` goes from `maxLength: 100` to `maxLength: 50` |
| `request-optional-field-added` | An optional request body field is added. | `POST /users` accepts an optional `nickname` |
| `request-optional-parameter-added` | An optional parameter is added. | `GET /users` gains an optional query parameter `page` |
| `request-field-became-optional` | A request body field is no longer required. | `email` leaves the `required` list of the `POST /users` body |
| `parameter-became-optional` | A required parameter becomes optional. | header `X-Tenant` changes to `required: false` |
| `request-body-added` | An optional request body is added. | `DELETE /users/{id}` accepts an optional JSON body |
| `request-body-became-optional` | A required request body becomes optional. | `PATCH /users/{id}` changes its body to `required: false` |
| `request-media-type-added` | A request body media type is added. | `POST /users` also accepts `application/x-www-form-urlencoded` |
| `request-type-widened` | A request field accepts more types. | `$.age` goes from `integer` to `[integer, string]` |
| `request-enum-widened` | Values are added to a request enum. | query parameter `sort` goes from `[asc]` to `[asc, desc]` |
| `request-constraint-loosened` | A request constraint gets looser. | `$.name` goes from `maxLength: 50` to `maxLength: 100` |
| `security-alternative-added` | A security alternative is added to an operation. | `GET /users` also accepts `apiKey` |
| `docs-changed` | Only documentation changes (`summary`, `description`, `title`, examples). | the `GET /users` description is reworded |

## Unknown

| Rule | Description | Example |
|------|-------------|---------|
| `response-enum-widened` | Values are added to a response enum; clients may not handle them. | `$.status` goes from `[active]` to `[active, archived]` |
| `response-constraint-loosened` | A response constraint gets looser; clients may not handle the new values. | `$.name` goes from `maxLength: 50` to `maxLength: 100` |
| `constraint-changed` | A constraint changes in a way that cannot be ordered exactly as stricter or looser. | `$.amount` goes from `multipleOf: 3` to `multipleOf: 1e300` |
| `response-field-added-to-closed-schema` | A response field is added to a schema with `additionalProperties: false`. | `$.nickname` appears in a `200` body that allowed no other properties |
| `request-field-removed` | A request body field is removed; clients may still send it. | `POST /users` no longer declares `nickname` |
| `parameter-removed` | A parameter is removed; clients may still send it. | `GET /users` no longer declares the query parameter `page` |
| `request-body-removed` | A request body is removed; clients may still send one. | `DELETE /users/{id}` no longer declares a body |
| `security-alternative-removed` | Some, but not all, security alternatives of an operation are removed. | `GET /users` accepted `basicAuth` or `bearerAuth` and now only `bearerAuth` |
| `composition-changed` | A `oneOf`, `anyOf`, `allOf` or `not` composition changes. | a `200` body goes from `oneOf: [A, B]` to `oneOf: [A, B, C]` |
| `vendor-extension-changed` | An `x-` vendor extension changes. | `x-rate-limit` goes from `100` to `50` |
