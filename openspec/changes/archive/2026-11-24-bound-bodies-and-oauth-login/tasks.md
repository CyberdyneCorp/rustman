# Tasks

## 1. Response bodies

- [x] 1.1 Spill outside a workspace to a per-user temp folder, refuse symbolic links and folders of other users, and create spill files new (#37). Verify: `ensure_private_dir` unit tests in `rustman-core` (symbolic link, other user's folder, tightened mode) and the `body.rs` tests `a_symlinked_spill_dir_outside_a_workspace_is_refused`, `an_existing_spill_file_is_never_opened` and `the_default_spill_dir_is_private_to_the_user`.
- [x] 1.2 Limit bodies that are not decoded with `[http] max_download_bytes` (#38). Verify: `an_endless_body_stops_at_the_download_limit` and `the_download_limit_applies_to_bodies_left_encoded` in `crates/rustman-http/tests/bodies.rs`, and the documented message in `docs_http.rs`.

## 2. OAuth2 login

- [x] 2.1 Cap concurrent loopback connections and back off on `accept` errors (#39). Verify: `failing_accepts_back_off_instead_of_spinning` and `concurrent_connections_are_capped` in `oauth2/loopback.rs`.
- [x] 2.2 Warn once at `rustman auth login` when the token goes to `.rustman/secrets.yaml` (#41). Verify: `login_reports_where_the_token_was_stored` in `crates/rustman-http/tests/oauth2.rs`, the stderr assertion in `auth_login_stores_the_token_in_the_secret_store` and `the_token_file_warning_is_documented` in `crates/rustman-cli/tests/cookies_auth.rs`.

## 3. Tests and docs

- [x] 3.1 Give the response-timeout test a margin that holds on loaded runners (#45). Verify: `a_slow_server_times_out_in_the_response_phase` warms the pool and asserts 500 ms to 2.5 s against a 5 s server delay.
- [x] 3.2 Document the limits, the spill folder, the loopback listener and the token notice. Verify: `docs/http.md`, `docs/environments.md` and `docs/request-files.md` updated; the docs tests pass.
