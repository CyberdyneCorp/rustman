# Spec Delta

## Purpose

Declarative checks on responses, and captures that extract values into variables for later requests. Together they make collections into repeatable tests without a scripting language.

## ADDED Requirements

### Requirement: Assertion list
A request file's `assertions` SHALL be an ordered list. Every assertion SHALL be evaluated, even after one fails, and each SHALL produce `pass`, `fail` (with expected vs. actual) or `error` (the assertion couldn't be evaluated). A request passes only if all its assertions pass. A request with no assertions passes on any completed response.

#### Scenario: All evaluated
- **GIVEN** three assertions where the first fails
- **WHEN** the request runs
- **THEN** all three results SHALL be reported

#### Scenario: No assertions
- **GIVEN** a request without `assertions` that gets a `500`
- **WHEN** it runs
- **THEN** the request SHALL pass, and the reporter SHALL show the status

### Requirement: Status assertions
The system SHALL support `status: <code>`, `status: [<codes>]` and `status: "2xx"`-style class patterns.

#### Scenario: Class pattern
- **GIVEN** `- status: "2xx"`
- **WHEN** the response is `201`
- **THEN** the assertion SHALL pass

#### Scenario: Mismatch message
- **GIVEN** `- status: 200`
- **WHEN** the response is `404`
- **THEN** the failure SHALL read `status: expected 200, got 404`

### Requirement: Header assertions
The system SHALL support `header: {name, equals | contains | matches | exists}`. Header names SHALL match case-insensitively. When a header repeats, the assertion SHALL pass if any value satisfies it.

#### Scenario: Header exists
- **GIVEN** `- header: {name: x-request-id, exists: true}`
- **WHEN** the response carries `X-Request-Id: 7`
- **THEN** the assertion SHALL pass

### Requirement: JSONPath assertions
The system SHALL support `json: {path: <RFC 9535 JSONPath>, <op>: value}` with ops `equals`, `not_equals`, `contains`, `matches`, `gt`, `gte`, `lt`, `lte`, `exists`, `length` and `type`. A path matching several nodes SHALL compare against the node list. A non-JSON body SHALL yield `error: response body is not JSON`.

#### Scenario: Equality on a field
- **GIVEN** `- json: {path: "$.user.id", equals: 42}` and body `{"user":{"id":42}}`
- **WHEN** evaluated
- **THEN** the assertion SHALL pass

#### Scenario: Array length
- **GIVEN** `- json: {path: "$.items", length: 3}` and an `items` array of 2
- **WHEN** evaluated
- **THEN** the failure SHALL read `json $.items: expected length 3, got 2`

#### Scenario: Missing path
- **GIVEN** `- json: {path: "$.nope", exists: true}`
- **WHEN** the path matches nothing
- **THEN** the assertion SHALL fail, not error

### Requirement: JSON Schema assertions
The system SHALL support `schema: {file: <path>}` or an inline `schema: {inline: {...}}` (JSON Schema draft 2020-12, with draft-07 auto-detected via `$schema`). On failure it SHALL report up to 10 violations, each with its instance path and message.

#### Scenario: Schema violation
- **GIVEN** a schema requiring `email` to be a string, and a body with `"email": 5`
- **WHEN** evaluated
- **THEN** the failure SHALL include `/email: 5 is not of type "string"`

### Requirement: Body assertions
The system SHALL support `body: {contains: <text>}`, `body: {not_contains}`, `body: {matches: <regex>}` and `body: {equals}`, evaluated against the decoded text body. A binary body SHALL yield `error: body is binary`.

#### Scenario: Regex match
- **GIVEN** `- body: {matches: "order-[0-9]{6}"}` and body `created order-123456`
- **WHEN** evaluated
- **THEN** the assertion SHALL pass

### Requirement: Latency assertions
The system SHALL support `latency: {total_lt: <ms>}` and `latency: {ttfb_lt: <ms>}`, evaluated against the exchange timings.

#### Scenario: Too slow
- **GIVEN** `- latency: {total_lt: 200}` and a total of 350 ms
- **WHEN** evaluated
- **THEN** the failure SHALL read `latency total: expected < 200 ms, got 350 ms`

### Requirement: Variable interpolation in assertions
Expected values in assertions SHALL support `{{variable}}` interpolation, using the same resolution rules as requests.

#### Scenario: Compare to captured id
- **GIVEN** an earlier capture `user_id=42`, and `- json: {path: "$.id", equals: "{{user_id}}"}`
- **WHEN** the response has `"id": 42`
- **THEN** the assertion SHALL pass, coercing the number to the variable's string form for comparison

### Requirement: Captures
`captures` SHALL be a list of `{var, from: json|header|status|body_regex|cookie, ...}` entries, evaluated after assertions. A captured value SHALL be available to every later request in the same run. A capture that matches nothing SHALL leave the variable unset and record a warning, unless `required: true`, which makes the request fail.

#### Scenario: Token chaining
- **GIVEN** `login` captures `{var: token, from: json, path: "$.access_token"}`, and `me` uses `Authorization: Bearer {{token}}`
- **WHEN** the collection runs in order
- **THEN** `me` SHALL be sent with the captured token

#### Scenario: Required capture missing
- **GIVEN** `{var: id, from: json, path: "$.id", required: true}` and a body without `id`
- **WHEN** the request runs
- **THEN** the request SHALL fail with `capture 'id': no match for $.id`

### Requirement: Capture persistence scope
Captures SHALL live only for the current run by default. A capture with `persist: true` SHALL write the value into the active environment's runtime state file `.rustman/state/<env>.yaml` (git-ignored), never into `environments/*.yaml`. A captured variable listed as a secret SHALL be stored via the secret store.

#### Scenario: Persisted capture survives runs
- **GIVEN** `{var: session, from: cookie, name: sid, persist: true}`
- **WHEN** a later, separate `rustman run` uses `{{session}}`
- **THEN** it SHALL resolve to the persisted value

#### Scenario: Environment file untouched
- **WHEN** any capture is persisted
- **THEN** no file under `environments/` SHALL be modified
