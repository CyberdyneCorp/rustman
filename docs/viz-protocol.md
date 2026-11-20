# Viz protocol (`rustman-viz/1`)

The viz protocol connects `rustman` (the producer: the CLI or the TUI) to the optional graphical companion `rustman-viz`. It is specified by the `visual-dashboards` capability in [`openspec/changes/add-raven-visualizer`](../openspec/changes/add-raven-visualizer/). The message types live in the `rustman-viz-protocol` crate (serde types only, no I/O), and the JSON Schema of every message is [`crates/rustman-viz-protocol/schema/rustman-viz-1.schema.json`](../crates/rustman-viz-protocol/schema/rustman-viz-1.schema.json).

## Transport

- The producer spawns the companion and writes to its **stdin**: UTF-8 JSON, **one message per line**, each an object with a `type` field.
- A line is at most **16 MiB** (16,777,216 bytes, newline excluded). A body that does not fit is written to a temp file and sent as a `blob_ref` instead of inline text. The producer owns that file: it keeps it while the companion runs and removes it once the companion has exited, so the companion must not delete it. (A producer that exits first leaves the file to the companion.)
- The companion's **stdout** carries only the handshake reply and `error` lines. Its stderr goes to a log file in the Rustman state directory.
- End of stdin means the producer is gone. Open windows stay usable and show `Producer disconnected`.

## Versioning

- The protocol is named `rustman-viz/<major>`. The major version changes only on breaking changes.
- Minor additions are new optional fields and new message types. Readers **ignore unknown fields and unknown message types** (the crate decodes an unknown `type` as `Message::Unknown`), and unknown values of open enumerations (views, outcomes, severities, states) decode as `unknown`.
- Writers never send a field or type the peer has not agreed to by version.

## Handshake

The producer opens every session with `hello`. The companion answers with `hello_ack` (and its views) or `hello_reject` (and the versions it speaks). After a rejection the producer sends nothing more and reports both versions, e.g. `Viz companion speaks rustman-viz/1, rustman needs rustman-viz/2`.

```json
{"type":"hello","protocol":"rustman-viz/1","producer":"rustman 0.2.0"}
{"type":"hello_ack","protocol":"rustman-viz/1","viewer":"rustman-viz 0.2.0","views":["run_dashboard","exploration_dashboard","api_graph","response_inspector"]}
{"type":"hello_reject","supported":["rustman-viz/1"]}
```

## Messages

Opening a view, and loading a saved report (`kind` is `run`, `exploration` or `bench`; `view` is optional, e.g. `api_graph` for `--view graph`):

```json
{"type":"open_view","view":"run_dashboard","title":"smoke/"}
{"type":"load_report","path":"reports/run-2026-11-17.json","kind":"run"}
```

A run as it happens. `outcome` is `passed`, `failed`, `errored` (a transport error, with its category in `error` and no `status`), `cancelled` or `skipped`. A run ends `completed` or `aborted`:

```json
{"type":"run_started","started_at":"2026-11-17T10:00:00Z","environment":"staging","total":200}
{"type":"exchange_completed","request":"users/get-user","method":"GET","url":"https://api.test/users/42","outcome":"passed","status":200,"duration_ms":84,"assertions":{"passed":3,"failed":0},"iteration":1}
{"type":"exchange_completed","request":"users/get-user","method":"GET","url":"https://nope.invalid/users/42","outcome":"errored","duration_ms":3,"assertions":{"passed":0,"failed":0},"error":"dns"}
{"type":"run_finished","state":"completed","summary":{"total":200,"passed":180,"failed":15,"errored":5,"skipped":0,"duration_ms":9000}}
```

An exploration. Severities are `critical`, `high`, `medium`, `low` and `info`:

```json
{"type":"exploration_started","started_at":"2026-11-17T10:00:00Z","operations":24,"base_url":"https://api.test"}
{"type":"probe_completed","operation":"POST /users","outcome":"passed","status":201,"duration_ms":12}
{"type":"finding","severity":"high","operation":"POST /users","title":"500 on empty body","detail":"POST /users with {} returned 500"}
{"type":"exploration_finished","state":"completed","operations_total":24,"operations_exercised":18,"status_codes_documented":40,"status_codes_seen":31}
```

A bench (`rustman bench --viz`, see [Load testing](bench.md)) opens the `bench` view, then sends one `bench_tick` per second with that second's requests, errors, rate, latency percentiles (milliseconds) and active VUs, and a final `bench_finished` with the totals and thresholds. `remaining_ms` is left out when the bench has no fixed duration, and `aborted` (`threshold` or `interrupted`) when it ran to its end. Bench events hold counts, timings and threshold expressions only, never request or response content:

```json
{"type":"open_view","view":"bench","title":"rustman bench https://staging.example.com"}
{"type":"bench_tick","second":3,"elapsed_ms":3000,"remaining_ms":27000,"active_vus":10,"requests":412,"errors":2,"rps":412.0,"p50_ms":18.2,"p95_ms":61.0,"p99_ms":120.4}
{"type":"bench_finished","state":"completed","duration_ms":30000,"iterations":4100,"requests":12300,"errors":41,"rps":410.0,"p50_ms":18.0,"p95_ms":64.3,"p99_ms":130.1,"thresholds":[{"expression":"p95<300ms","passed":true,"actual":"64.3ms"}]}
```

A response for the inspector, inline or by reference. Header values and bodies are redacted by the producer before they are written (secrets appear as `«redacted:name»`); the companion has no reveal function:

```json
{"type":"inspect_response","request":"users/list","status":200,"content_type":"application/json","headers":[{"name":"Authorization","value":"Bearer «redacted:token»"}],"selector":"$.items","body":{"type":"inline","text":"{\"items\":[{\"id\":1,\"name\":\"a\"},{\"id\":2,\"email\":\"b@x\"}]}"}}
{"type":"inspect_response","request":"exports/all","status":200,"body":{"type":"blob_ref","path":"/tmp/rustman-viz/body-01.json","bytes":209715200,"media_type":"application/json"}}
{"type":"blob_ref","path":"/tmp/rustman-viz/report-01.json","bytes":20971520,"media_type":"application/json"}
```

Errors and the end of a session:

```json
{"type":"error","code":"vulkan_unavailable","message":"Graphics unavailable (Vulkan)"}
{"type":"close"}
```

## Priorities

A companion whose queue is full may coalesce progress messages (`exchange_completed`, `probe_completed`), but never drops `finding`, `run_finished`, `exploration_finished` or `bench_finished`. Producers apply the same rule to their own queue: `rustman bench --viz` drops a `bench_tick` rather than wait for a slow companion, never the `bench_finished`.

## Exit codes of the companion

| Code | Meaning |
|------|---------|
| 0 | The user closed the windows (or `--protocol-check` succeeded) |
| 2 | Bad arguments, or a report file that is not a Rustman report |
| 3 | Graphics unavailable: an `error` line `vulkan_unavailable` is written first |
| other | A crash; the producer shows `Viz companion exited` and keeps running |

`rustman-viz --protocol-check` prints the protocol names it speaks, one per line, and exits 0.
