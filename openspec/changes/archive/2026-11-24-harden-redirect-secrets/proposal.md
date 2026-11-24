# Proposal

## Why

The HTTP engine review (issue #40) found that a cross-origin redirect drops only the standard credentials (`Authorization`, `Proxy-Authorization`, an explicit `Cookie` and the `api_key`). A secret in a custom header, such as `X-Api-Key: '{{token}}'`, or in a `307`/`308` body, such as a token request's `client_secret`, was sent on to the new host. That matches curl, but contradicts the rule that secrets never leave for another server.

## What Changes

- When a redirect hop changes origin, every header whose value carries one of the request's secrets, and a kept `307`/`308` body that carries one, is dropped as well. Secrets are found with the request's `Redactor` (secret variable values and literal credentials, in every encoded form), so detection matches redaction.
- The hop is still sent, without what was dropped, and the exchange gets the warning `cross-origin redirect to <origin>: dropped <parts>, which carry a secret`.
- The OAuth2 token endpoint never follows redirects (already the behavior; now in the spec).
- The `ca_bundle` trust source (bundle plus platform roots loaded with `rustls-native-certs`, design D3) gets a regression test (issue #43). Workspace-level mutual TLS (issue #42) is already in `rustman.toml [http]` and covered by `mtls_from_the_workspace_settings`.

Out of scope: secrets in the redirect target URL chosen by the server, refusing cross-origin redirects altogether (a request can set `follow_redirects: false`), and the other review follow-ups (#37, #38, #39, #41, #44, #45).

## Capabilities

### New Capabilities
None.

### Modified Capabilities
- `http-requests`: redirect credential stripping covers any secret-bearing header and body; token endpoint redirects.

## Impact

- `rustman-http`: `redirect::next_request` (now async, takes the request's `Redactor` and returns the warning), `Hops` in `engine.rs`, `tls::bundle_roots`.
- Docs: `docs/http.md` (Redirects, Errors and warnings).
- No new dependencies.
- Archive order: the `http-requests` capability is introduced by `bootstrap-core-engine`, so this change is archived after it.
