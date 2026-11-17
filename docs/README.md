# Rustman documentation

User and developer documentation for Rustman. Pages are added as the features they describe are implemented; the behavior itself is specified in [`openspec/`](../openspec/).

## Available

- [Install and quick start](../README.md#install-and-quick-start-current-state)
- [Request files](request-files.md): the workspace layout, `rustman.toml`, the `*.rman.yaml` format with one example per body type, collections and `rustman init`
- [Environments, variables and secrets](environments.md): environment files, the active environment, variable precedence, dynamic variables, `rustman secret`, secret storage and redaction, and the CI pattern
- [HTTP requests](http.md): transport, timeouts, redirects, TLS and mTLS, proxies, cookie jars, default headers, auth helpers (including OAuth2 and `rustman auth login`), timings, response bodies, and `rustman import curl` / `rustman export curl`

## Planned

These pages arrive with the `bootstrap-core-engine` change ([tasks](../openspec/changes/bootstrap-core-engine/tasks.md)):

| Page | Covers |
|------|--------|
| `assertions.md` | Declarative assertions and captures |
| `cli.md` | CLI reference and exit codes |
| `history.md` | Request history, retention and replay |
| `schemas/` | Published JSON Schemas, starting with `report.schema.json` (`rustman.report/v1`) |
