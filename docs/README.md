# Rustman documentation

User and developer documentation for Rustman. Pages are added as the features they describe are implemented; the behavior itself is specified in [`openspec/`](../openspec/).

## Available

- [Install and quick start](../README.md#install-and-quick-start-current-state)

## Planned

These pages arrive with the `bootstrap-core-engine` change ([tasks](../openspec/changes/bootstrap-core-engine/tasks.md)):

| Page | Covers |
|------|--------|
| `request-files.md` | The `*.rman.yaml` request-file format, with one example per body type |
| `environments.md` | Environments, variables, secrets and the CI pattern |
| `http.md` | HTTP settings, auth helpers and curl import/export |
| `assertions.md` | Declarative assertions and captures |
| `cli.md` | CLI reference and exit codes |
| `history.md` | Request history, retention and replay |
| `schemas/` | Published JSON Schemas, starting with `report.schema.json` (`rustman.report/v1`) |
