## MODIFIED Requirements

### Requirement: Redirect credential stripping
Every credential the request carries (`Authorization`, `Proxy-Authorization`, an explicit `Cookie` header, and the `api_key` header or query parameter) SHALL be dropped when a redirect hop changes origin (scheme, host or port). On such a hop, every other header whose value carries one of the request's secrets, and a `307`/`308` body that carries one, SHALL be dropped too; the hop SHALL still be sent without them, and the exchange SHALL get the warning `cross-origin redirect to <origin>: dropped <parts>, which carry a secret`. A same-origin hop SHALL keep them.

#### Scenario: Cross-host auth stripping
- **GIVEN** `https://api.example.com/x` redirects to `https://cdn.other.com/x`
- **WHEN** the request carries bearer auth
- **THEN** the second hop SHALL NOT include `Authorization`

#### Scenario: Cross-origin API key stripping
- **GIVEN** `http://localhost:8080/x` redirects to `http://127.0.0.1:8080/x`
- **WHEN** the request carries an `api_key` query parameter
- **THEN** the second hop SHALL NOT include that parameter

#### Scenario: Secret in a custom header and a 307 body
- **GIVEN** `http://localhost:8080/x` answers `307` to `http://127.0.0.1:8080/y`
- **WHEN** the request is a `POST` with `X-Api-Key: '{{token}}'` and a form body with `client_secret={{token}}`, where `token` is a secret
- **THEN** the second hop SHALL be a `POST` without `X-Api-Key` and with an empty body
- **AND** the exchange SHALL warn `cross-origin redirect to http://127.0.0.1:8080: dropped header x-api-key, the body, which carry a secret`

## ADDED Requirements

### Requirement: Token endpoint redirects
Calls to an OAuth2 `token_url` SHALL NOT follow redirects, so the form with the client secret, code or refresh token is never sent to another URL.

#### Scenario: Token URL redirects
- **GIVEN** `token_url` answers `307` to another URL
- **WHEN** a token is requested
- **THEN** the request SHALL fail with an `oauth2:` error
- **AND** the other URL SHALL receive no request
