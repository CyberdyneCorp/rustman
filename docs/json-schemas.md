# JSON schemas

Machine-readable outputs carry a `schema` field naming their format and major version, and are described by JSON Schemas kept in this repository:

| Schema | Describes | Produced by |
|--------|-----------|-------------|
| [`rustman.report/v1`](schemas/report.schema.json) | The canonical report of runs, explorations, contract checks, load and stress tests (`kind: run`, `exploration`, `contract`, `bench`, `stress`) | `rustman run --reporter json`, `--report json`, `rustman explore run`; `rustman report schema` prints the schema; see [Reports](reports.md) |
| [`rustman.import-report/v1`](schemas/import-report.schema.json) | The report of an import: files, conflicts, moved secrets, notes and script migration counts | `rustman import --report <file>`; see [Importing collections](import.md#the-report) |
| [`rustman-viz/1`](https://github.com/CyberdyneCorp/rustman/blob/main/crates/rustman-viz-protocol/schema/rustman-viz-1.schema.json) | The JSON-lines protocol between `rustman` and the viz companion | `rustman run --viz`, `rustman viz`; see [Viz protocol](viz-protocol.md) |

Exit codes are listed in the [CLI reference](cli.md#exit-codes), with the import-specific ones in [Importing collections](import.md#exit-codes).
