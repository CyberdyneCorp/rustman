# Rustman documentation

User and developer documentation for Rustman. Pages are added as the features they describe are implemented; the behavior itself is specified in [`openspec/`](../openspec/).

## Available

- [Install and quick start](../README.md#install-and-quick-start-current-state)
- [Request files](request-files.md): the workspace layout, `rustman.toml`, the `*.rman.yaml` format with one example per body type, collections and `rustman init`
- [Environments, variables and secrets](environments.md): environment files, the active environment, variable precedence, dynamic variables, `rustman secret`, secret storage and redaction, and the CI pattern
- [Safety policy](safety.md): the rules automated senders (exploration, agents) pass before a request is sent: method gating, the origin allowlist, the production guard, rate, concurrency and budgets, `429` backoff, the `[safety]` table in `rustman.toml`, and `GovernedSender`
- [HTTP requests](http.md): transport, timeouts, redirects, TLS and mTLS, proxies, cookie jars, default headers, auth helpers (including OAuth2 and `rustman auth login`), timings, response bodies, and `rustman import curl` / `rustman export curl`
- [Assertions and captures](assertions.md): status, header, JSONPath, JSON Schema, body and latency assertions, interpolated expected values, captures, and where captured values are kept
- [Request history](history.md): what is recorded and where, retention, redaction at rest, `rustman history list` filters, `show`, `replay` with secret re-resolution, and `clear`
- [CLI reference](cli.md): every command, global flags, `rustman send` (including ad-hoc requests), `rustman run` (filters, iterations, parallel folders, bail), the pretty, json, junit and tap reporters, terminal output and logs, completions and exit codes
- [Report schema](schemas/report.schema.json): the JSON Schema of `rustman.report/v1` (`kind: run`)
- [Terminal UI](tui.md): starting `rustman` / `rustman tui`, workspace discovery, the create-or-scratch prompt, the responsive layout, focus, status bar, toasts, mouse, themes and colour detection, `--ascii`, `tui.toml`, the command palette, help, vim mode, `keybindings.toml` and the key reference, quitting and terminal restore, and rendering screenshots

