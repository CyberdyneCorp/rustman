# Tasks

## 1. Redirects

- [x] 1.1 Drop secret-bearing headers and `307`/`308` bodies on cross-origin redirects, with a warning (#40). Verify: `secret_headers_and_bodies_are_dropped_when_the_origin_changes`, `secrets_stay_on_a_same_origin_redirect` and `requests_without_secrets_cross_origins_unchanged` in `redirect.rs`; `secret_headers_and_bodies_stay_behind_on_a_cross_origin_redirect` (canary) and `secret_headers_and_bodies_follow_a_same_origin_redirect` in `crates/rustman-http/tests/redirects.rs`.
- [x] 1.2 Keep the OAuth2 token endpoint from following redirects (#40). Verify: `the_token_call_does_not_follow_redirects` in `crates/rustman-http/tests/oauth2.rs`.

## 2. TLS

- [x] 2.1 Guard the `ca_bundle` trust source: the bundle plus the platform roots from `rustls-native-certs`, as design D3 and `docs/http.md` say (#43). Verify: `a_bundle_is_trusted_next_to_the_platform_roots` in `tls.rs`.
- [x] 2.2 Confirm workspace-level mutual TLS in `rustman.toml [http]` (#42). Verify: `mtls_from_the_workspace_settings` in `crates/rustman-http/tests/tls.rs` and `the_workspace_client_certificate_applies_without_one_of_its_own` in `crates/rustman-core/tests/variables.rs`.

## 3. Docs

- [x] 3.1 Document the stripping and its warning. Verify: `docs/http.md` Redirects and "Errors and warnings" updated; `documented_messages_match_the_engine` passes.
