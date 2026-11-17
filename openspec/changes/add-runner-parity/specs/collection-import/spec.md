## ADDED Requirements

### Requirement: Extended script-to-assertion mapping
On top of the existing mappings, the importer SHALL map:
- `pm.response.to.be.ok` → `status: "2xx"`;
- `pm.expect(pm.response.responseTime).to.be.below(N)` → `latency: {total_lt: N}`;
- `pm.expect(pm.response.text()).to.include("s")` → `body: {contains: "s"}`;
- `pm.response.to.have.header("H", "v")` → `header: {name: H, equals: v}`.

Statements outside the mapping table SHALL remain comment-only.

#### Scenario: Response time check
- **GIVEN** `pm.test("fast", () => pm.expect(pm.response.responseTime).to.be.below(300));`
- **WHEN** it is imported
- **THEN** the request file SHALL contain `latency: {total_lt: 300}`

#### Scenario: Header value check
- **GIVEN** `pm.response.to.have.header("Content-Type", "application/json");`
- **WHEN** it is imported
- **THEN** the request file SHALL contain `header: {name: Content-Type, equals: application/json}`

### Requirement: JSON body field mapping
The importer SHALL track `const|let|var <name> = pm.response.json()` and map `pm.expect(<name>.a.b).to.eql(v)` and `.to.equal(v)` to `json: {path: "$.a.b", equals: v}`. It SHALL also map `.to.exist` to `exists: true`, `.to.have.lengthOf(n)` to `length: n`, `.to.be.a("t")` to `type: t`, and `.to.be.above(n)` and `.to.be.below(n)` to `gt` and `lt`.

#### Scenario: Field equality
- **GIVEN** `const body = pm.response.json(); pm.expect(body.user.id).to.eql(42);`
- **WHEN** it is imported
- **THEN** the request file SHALL contain `json: {path: "$.user.id", equals: 42}`

#### Scenario: Array index and length
- **GIVEN** `const d = pm.response.json(); pm.expect(d.items[0].tags).to.have.lengthOf(2);`
- **WHEN** it is imported
- **THEN** the request file SHALL contain `json: {path: "$.items[0].tags", length: 2}`

### Requirement: Variable setter mapping
`pm.environment.set`, `pm.collectionVariables.set`, `pm.globals.set` and `pm.variables.set` SHALL map to captures when the value is `pm.response.json()<path>`, a tracked JSON variable path, `pm.response.headers.get("H")` or `pm.cookies.get("c")`. Environment and global setters SHALL be marked `persist: true`.

#### Scenario: Header captured
- **GIVEN** `pm.environment.set("etag", pm.response.headers.get("ETag"));`
- **WHEN** it is imported
- **THEN** the request file SHALL contain the capture `{var: etag, from: header, name: ETag, persist: true}`

#### Scenario: Collection variable from tracked body
- **GIVEN** `const b = pm.response.json(); pm.collectionVariables.set("orderId", b.order.id);`
- **WHEN** it is imported
- **THEN** the request file SHALL contain `{var: orderId, from: json, path: "$.order.id"}` without `persist`

### Requirement: Migration summary
The import report SHALL include, per request and in total, the number of script statements found, mapped and left unconverted, plus the overall percentage converted. The pretty summary SHALL print `scripts: <mapped>/<found> statements converted (<pct>%)`.

#### Scenario: Summary line
- **GIVEN** a collection with 40 script statements, 31 of them mappable
- **WHEN** it is imported
- **THEN** the summary SHALL print `scripts: 31/40 statements converted (77%)`

### Requirement: Migration report
`rustman import <format> <file> --migration-report <file.md>` SHALL write a Markdown report with one section per request holding unconverted statements. Each entry SHALL show the original statement, the request file path, and either a suggested manual equivalent or `no declarative equivalent; consider a plugin`. Secret values SHALL be redacted.

#### Scenario: Suggested equivalent
- **GIVEN** an unconverted statement `pm.expect(pm.response.json().items.every(i => i.price > 0)).to.be.true;`
- **WHEN** the migration report is written
- **THEN** its entry SHALL suggest a `schema` assertion with `items.price` constrained by `exclusiveMinimum: 0`

#### Scenario: No equivalent
- **GIVEN** an unconverted statement `postman.setNextRequest("checkout");`
- **WHEN** the migration report is written
- **THEN** its entry SHALL read `no declarative equivalent; consider a plugin`, and also point to collection ordering in `_collection.yaml`
