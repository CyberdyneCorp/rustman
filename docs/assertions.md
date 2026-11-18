# Assertions and captures

**Assertions** are declarative checks on a response: its status, headers, JSON fields, schema, body text and timing. **Captures** extract values from a response into variables that later requests use, such as a login token. Together they turn a collection into a repeatable test, without a scripting language.

The examples on this page are checked by an integration test (`crates/rustman-http/tests/docs_assertions.rs`): the files below make up one workspace, every request in it is sent to an in-process test server that answers as described, and every assertion and required capture must pass. The failure messages quoted on this page are the exact texts Rustman prints, checked by unit tests in `crates/rustman-core/src/assertions/tests.rs`.

## How assertions are evaluated

`assertions` is an ordered list in a request file. Each item has one key naming its type: `status`, `header`, `json`, `schema`, `body` or `latency`, or one of the GraphQL checks (`no_errors`, `errors_count`, `error_message_contains`, `error_code`, `data`) described in [GraphQL](graphql.md#assertions-and-captures).

- Every assertion is evaluated, even after one fails, so a report shows all of them.
- Each one ends as `pass`, `fail` (with the expected and actual values) or `error` (it could not be evaluated, for example a JSONPath on a body that is not JSON).
- A request passes only when all its assertions pass. A request with no assertions passes on any completed response, whatever its status (a `500` included); the reporter still shows the status.

```yaml
# users/get-user.rman.yaml
version: 1
kind: http
name: Get user
url: '{{base_url}}/users/42'
assertions:
  - status: 200
  - header:
      name: x-request-id
      exists: true
  - header:
      name: Content-Type
      contains: json
  - json:
      path: $.user_missing
      exists: false
  - json:
      path: $.id
      equals: 42
  - json:
      path: $.name
      matches: '^[A-Z][a-z]+$'
  - json:
      path: $.roles
      length: 2
  - json:
      path: $.roles
      contains: admin
  - json:
      path: $.roles[*]
      equals: [admin, dev]
  - json:
      path: $.address.city
      type: string
  - json:
      path: $.id
      gte: 1
  - body:
      contains: '"email"'
  - body:
      not_contains: password
  - latency:
      total_lt: 5000
```

## Status

| Form | Passes when |
|------|-------------|
| `status: 200` | the status is 200 |
| `status: [200, 201]` | the status is one of the list |
| `status: "2xx"` | the status is in that class (`1xx` to `5xx`) |

A mismatch reads `status: expected 200, got 404`, `status: expected one of [200, 204], got 201` or `status: expected 2xx, got 500`.

## Headers

`header: {name, <op>}` with exactly one operator:

| Operator | Passes when a value of the header... |
|----------|--------------------------------------|
| `equals: <text>` | is exactly the text |
| `contains: <text>` | contains the text |
| `matches: <regex>` | matches the regular expression |
| `exists: true` / `exists: false` | is present / is absent |

Header names match case-insensitively. When a header repeats (several `Vary` or `Set-Cookie` lines), the assertion passes if any value satisfies it. Failures read `header x-trace: expected to exist, got nothing` or `header vary: expected "Cookie", got "Accept", "Origin"`. Values of `Authorization`, `Proxy-Authorization`, `Cookie` and `Set-Cookie` are shown as `«redacted:<Header>»`.

## JSON fields

`json: {path, <op>: value}` selects nodes of a JSON body with an [RFC 9535](https://www.rfc-editor.org/rfc/rfc9535) JSONPath and compares them with exactly one operator:

| Operator | Passes when the selection... |
|----------|------------------------------|
| `equals: <value>` | equals the value (numbers compare by value, so `1` equals `1.0`) |
| `not_equals: <value>` | does not equal the value |
| `contains: <value>` | is a string containing the text, an array with an equal element, or an object with that key |
| `matches: <regex>` | is a string (or a number or boolean, in its JSON form) matching the regular expression |
| `gt`, `gte`, `lt`, `lte: <number>` | is a number `>`, `>=`, `<`, `<=` the bound |
| `exists: true` / `exists: false` | matches at least one node / matches nothing |
| `length: <n>` | is an array, object or string (in characters) of that length |
| `type: <name>` | is a `string`, `number`, `integer`, `boolean`, `array`, `object` or `null` |

- A path matching one node compares against that node. A path matching several nodes (`$.roles[*]`, `$..id`, a filter) compares against the list of nodes, so `equals: [admin, dev]` and `length: 2` apply to the list.
- A path that matches nothing fails, it does not error: `json $.nope: expected to exist, got nothing`, or `json $.nope: expected 1, got nothing` for other operators. Only `exists: false` passes.
- A body that is not JSON yields `error: response body is not JSON`. An invalid JSONPath, an invalid regular expression or an unknown `type` name is also an error.
- Failures quote compact JSON: `json $.user.id: expected 41, got 42`, `json $.items: expected length 3, got 2`, `json $.price: expected > 10, got 9.5`, `json $.name: expected type number, got string`.

## JSON Schema

`schema: {file: <path>}` validates the JSON body against a schema file, and `schema: {inline: {...}}` against a schema written in the request file. The file path is relative to the request file's folder and must stay inside the workspace; schema files are JSON.

The default dialect is JSON Schema draft 2020-12. A schema whose `$schema` names draft-07 (`http://json-schema.org/draft-07/schema#`) is validated as draft-07. Remote `$ref`s are not fetched.

File `schemas/user.json`:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["id", "email"],
  "properties": {
    "id": {"type": "integer"},
    "email": {"type": "string"},
    "roles": {"type": "array", "items": {"type": "string"}}
  }
}
```

```yaml
# users/user-schema.rman.yaml
version: 1
kind: http
name: User matches its schema
url: '{{base_url}}/users/42'
assertions:
  - schema:
      file: ../schemas/user.json
  - schema:
      inline:
        $schema: http://json-schema.org/draft-07/schema#
        type: object
        properties:
          address:
            type: object
            required: [city]
```

On failure the message lists up to 10 violations, each with its instance path (a JSON Pointer, `/` for the root) and message, and then how many more there are:

```text
schema: expected a valid document, got 2 violations: /: "id" is a required property; /email: 5 is not of type "string"
```

A schema that is itself invalid, an unreadable file or a body that is not JSON is an error.

## Body text

`body: {<op>: <text>}` checks the decoded body text with one of `contains`, `not_contains`, `matches` (a regular expression) or `equals`. A binary body (not valid UTF-8) yields `error: body is binary`. Failures quote at most 200 characters of the body: `body: expected not to contain "order", got "created order-123456"`.

Regular expressions everywhere (headers, JSON, body, captures) use the Rust `regex` syntax: matching takes linear time, and look-around and back-references are not supported. A pattern matches anywhere in the text unless anchored with `^` and `$`.

## Latency

`latency: {total_lt: <ms>}` and `latency: {ttfb_lt: <ms>}` (both may be given) compare the exchange timings: the total time and the time to first byte (see [timings](http.md#timing-breakdown)). A slow response fails with `latency total: expected < 200 ms, got 350 ms`.

## Variables in expected values

Expected values may use `{{variable}}`, resolved like the request itself: overrides, captures from earlier requests, request and collection variables, persisted captures, the environment and `rustman.toml` (see [precedence](environments.md#precedence)). An unresolved name is an error that lists every missing name: `error: json $.id: unresolved variable(s): user_id`.

A variable always resolves to text. When an expected value is exactly a placeholder string, such as `equals: '{{user_id}}'`, a number or boolean in the response compares through its text form, so a captured `42` equals the JSON number `42`. A literal string does not: `equals: 'true'` does not match the boolean `true`.

A secret used in an expected value is redacted from every message, like everywhere else.

## Captures

`captures` is a list evaluated after the assertions. Each capture sets one variable for every later request of the same run:

| `from` | Fields | Captures |
|--------|--------|----------|
| `json` | `path` | the selected node: a string as is, any other value as compact JSON (`42`, `{"a":1}`); several nodes as a JSON array |
| `header` | `name` | the header's first value (case-insensitive name) |
| `status` | none | the status code, such as `201` |
| `body_regex` | `pattern`, optional `group` | a group of the first match in the body text: group 1 by default, or the whole match when the pattern has no groups |
| `cookie` | `name` | the value of that cookie set by this response (`Set-Cookie`) |

Every capture also takes `required` and `persist` (both default `false`).

- A capture that matches nothing leaves the variable unset and records a warning, such as `capture 'id': no match for $.id`. With `required: true` it fails the request with that message instead.
- A capture of a later request replaces the value of an earlier one with the same name. Captured values outrank request, collection and environment variables, and only `--var` overrides them.

```yaml
# orders/create-order.rman.yaml
version: 1
kind: http
name: Create order
method: POST
url: '{{base_url}}/orders'
body:
  type: text
  content: one coffee
assertions:
  - status: [201, 202]
  - body:
      matches: order-[0-9]{6}
captures:
  - var: order_url
    from: header
    name: Location
    required: true
  - var: order_id
    from: body_regex
    pattern: order-([0-9]+)
  - var: created_status
    from: status
  - var: cart
    from: cookie
    name: cart
```

### Token chaining

A login request captures a token, and every later request sends it:

```yaml
# auth/1-login.rman.yaml
version: 1
kind: http
name: Log in
method: POST
url: '{{base_url}}/token'
assertions:
  - status: 200
captures:
  - var: token
    from: json
    path: $.access_token
    required: true
  - var: user_id
    from: json
    path: $.user.id
    persist: true
```

```yaml
# auth/2-me.rman.yaml
version: 1
kind: http
name: Who am I
url: '{{base_url}}/me'
auth:
  type: bearer
  token: '{{token}}'
assertions:
  - status: 200
  - json:
      path: $.id
      equals: '{{user_id}}'
```

## Where captured values live

Captures live only for the current run by default.

- `persist: true` also writes the value to the active environment's runtime state file, `.rustman/state/<env>.yaml` (`.rustman/state/@none.yaml` without an active environment). A later, separate `rustman run` resolves `{{user_id}}` from it, below request and collection variables but above the environment. The state file is per-user, git-ignored state, written atomically and readable by the current user only. Files under `environments/` are never modified. A value that contains a secret seen in the run (a server echoing it back) is not written; the run warns `capture 'user_id': not persisted: the value contains a secret` and the value is still available to later requests of the same run.
- A captured variable listed under the active environment's `secrets` is a secret: its value is stored in the secret store (the OS keychain, or `.rustman/secrets.yaml`), never in a state file, whether or not `persist` is set, and it is redacted from reports, history, logs and errors like any other secret. With the environment below, the login above refreshes the stored `token` on every run.
- A captured value that contains a secret seen earlier in the run (a server echoing a token back, say), or any piece of one at least 4 characters long, becomes a secret itself for the rest of the run: every later request that uses it redacts it as `«redacted:<var>»` in reports, history, logs and errors, and its capture result shows `"secret": true`. It is not stored anywhere, so an exchange that used it cannot be replayed from history.

```yaml
# environments/local.yaml
variables:
  base_url: http://localhost:8080
secrets:
  - token
```

To forget persisted captures, delete `.rustman/state/<env>.yaml`; to forget a captured secret, run `rustman secret rm <name> --env <env>`.
