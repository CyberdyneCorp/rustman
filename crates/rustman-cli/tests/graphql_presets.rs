//! Blockchain GraphQL presets in `rustman graphql query` and `rustman run`
//! (add-evm-toolkit tasks 7.1-7.4): the preset and lag on the status line,
//! formatted scalars with `--raw` and `--units`, `--paginate` with the page
//! cap, `subgraph` assertions, and secrets kept redacted in formatted
//! output. Against a mock subgraph and a stub EVM node.

mod common;

use common::{rustman, write};
use rustman_test_support::jsonrpc::{JsonrpcHttpServer, RpcReply};
use rustman_test_support::{GraphqlHttpServer, RecordedRequest, Reply};
use serde_json::{Value, json};

const OWNER: &str = "0xd8da6bf26964af9d7eed9e03e53415d37aa96045";
const OWNER_CHECKSUM: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";

/// The servers and the runtime they are served from (dropped after them).
struct Served {
    graph: GraphqlHttpServer,
    node: JsonrpcHttpServer,
    _runtime: tokio::runtime::Runtime,
}

/// A subgraph indexed up to block 1000 with one token, many transfers
/// (`id_gt` pages of `first`), and a node whose head is 1120.
fn serve() -> Served {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let graph = runtime.block_on(GraphqlHttpServer::start(subgraph));
    let node = runtime.block_on(JsonrpcHttpServer::start(|hit, _| {
        let body = hit.json();
        assert_eq!(body["method"], "eth_blockNumber");
        RpcReply::json(&json!({"jsonrpc": "2.0", "id": body["id"], "result": "0x460"}))
    }));
    Served {
        graph,
        node,
        _runtime: runtime,
    }
}

fn subgraph(hit: &RecordedRequest) -> Reply {
    let body = hit.json();
    let document = body["query"].as_str().unwrap_or_default();
    if document.contains("_meta") {
        return Reply::graphql(json!({"data": {"_meta": {
            "block": {"number": 1000}, "hasIndexingErrors": false
        }}}));
    }
    if document.contains("transfers") {
        let after = body["variables"]["last"].as_str().unwrap_or("").to_owned();
        // `failing:` lists answer the third page with a gateway error.
        let key = match document.contains("failing:") {
            true if after == "t01999" => return Reply::raw(502, "text/plain", "bad gateway"),
            true => "failing",
            false => "transfers",
        };
        let page: Vec<Value> = (0..2500)
            .map(|i| format!("t{i:05}"))
            .filter(|id| *id > after)
            .take(1000)
            .map(|id| json!({"id": id}))
            .collect();
        return Reply::graphql(json!({"data": {key: page}}));
    }
    Reply::graphql(json!({"data": {"token": {
        "id": "x", "owner": OWNER, "decimals": 18,
        "totalSupply": "1000000000000000000000"
    }}}))
}

const SCHEMA: &str = r#"
scalar BigInt
scalar Bytes
type _Block_ { number: Int! }
type _Meta_ { block: _Block_! hasIndexingErrors: Boolean! }
type Token { id: ID! owner: Bytes! decimals: Int! totalSupply: BigInt! }
type Transfer { id: ID! }
input Transfer_filter { id_gt: ID }
type Query {
  _meta: _Meta_
  token(id: ID!): Token
  transfers(first: Int, where: Transfer_filter): [Transfer!]!
}
"#;

/// A workspace with the `token` and `transfers` requests; its `dev`
/// environment has `rpc` when `with_rpc`, and the secret `owner` holding
/// the checksummed owner address, which `token` sends as a header.
fn workspace(served: &Served, with_rpc: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    let rpc = match with_rpc {
        true => format!("  rpc: {}\n", served.node.url("/")),
        false => String::new(),
    };
    let env = format!(
        "variables:\n  subgraph: {}\n{rpc}secrets: [owner]\n",
        served.graph.url("/")
    );
    write(root, "environments/dev.yaml", &env);
    write(root, "schema.graphql", SCHEMA);
    let request = |name: &str, document: &str, extra: &str| {
        format!(
            "version: 1\nkind: graphql\nname: {name}\nendpoint: '{{{{subgraph}}}}'\n\
             schema_file: schema.graphql\ndocument: '{document}'\n{extra}"
        )
    };
    let token = "query { token(id: \"x\") { id owner decimals totalSupply } }";
    // The request sends the secret, so the run knows to redact it.
    let header = "headers:\n  - {name: X-Owner, value: '{{owner}}'}\n";
    write(root, "token.rman.yaml", &request("token", token, header));
    let stale = "assertions:\n  - subgraph: {max_lag_blocks: 50}\n  - data: {path: $.token.owner, equals: '0xd8da6bf26964af9d7eed9e03e53415d37aa96045'}\n";
    write(root, "stale.rman.yaml", &request("stale", token, stale));
    let transfers = "query($last: ID) { transfers(first: 1000, where: {id_gt: $last}) { id } }";
    write(
        root,
        "transfers.rman.yaml",
        &request("transfers", transfers, ""),
    );
    let failing =
        "query($last: ID) { failing: transfers(first: 1000, where: {id_gt: $last}) { id } }";
    write(root, "failing.rman.yaml", &request("failing", failing, ""));
    write(
        root,
        "forced.rman.yaml",
        &request("forced", token, "preset: geth\n"),
    );
    rustman(root)
        .args(["secret", "set", "owner", "--env", "dev"])
        .write_stdin(OWNER_CHECKSUM)
        .assert()
        .success();
    dir
}

fn query(dir: &tempfile::TempDir, args: &[&str]) -> (u8, String, String) {
    let output = rustman(dir.path())
        .args(["graphql", "query", "--env", "dev"])
        .args(args)
        .output()
        .unwrap();
    let code = u8::try_from(output.status.code().unwrap()).unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (code, text(&output.stdout), text(&output.stderr))
}

#[test]
fn the_status_line_shows_the_preset_and_the_lag() {
    let served = serve();
    let dir = workspace(&served, true);
    let (code, out, err) = query(&dir, &["token"]);
    assert_eq!(code, 0, "{err}");
    let status = out.lines().next().unwrap();
    assert!(
        status.ends_with("preset subgraph · lag 120 blocks"),
        "{status}"
    );
    assert!(
        out.contains("\"totalSupply\": \"1000000000000000000000\""),
        "{out}"
    );
    let no_rpc = workspace(&served, false);
    let (_, out, _) = query(&no_rpc, &["token"]);
    let status = out.lines().next().unwrap();
    assert!(
        status.ends_with("preset subgraph · unknown (no rpc)"),
        "{status}"
    );
}

#[test]
fn formatted_addresses_stay_redacted_and_raw_shows_the_json_as_received() {
    let served = serve();
    let dir = workspace(&served, true);
    let (_, out, _) = query(&dir, &["token"]);
    // The checksummed form is the `owner` secret: formatting must not
    // reveal it.
    assert!(!out.contains(OWNER_CHECKSUM), "{out}");
    assert!(out.contains("«redacted:owner»"), "{out}");
    let (_, raw, _) = query(&dir, &["token", "--raw"]);
    assert!(raw.contains(OWNER), "{raw}");
    let (_, units, _) = query(&dir, &["token", "--units"]);
    assert!(units.contains("\"1000 (18 decimals)\""), "{units}");
    let (_, json, _) = query(&dir, &["token", "--json"]);
    let value: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["data"]["token"]["owner"], OWNER, "--json stays raw");
    assert_eq!(value["preset"]["name"], "subgraph");
    assert_eq!(value["preset"]["freshness"]["lag"], 120);
}

#[test]
fn an_explicit_preset_warns_when_the_schema_does_not_match() {
    let served = serve();
    let dir = workspace(&served, true);
    let (code, out, err) = query(&dir, &["forced"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains("warning: schema does not look like geth EIP-1767"),
        "{err}"
    );
    assert!(
        out.lines().next().unwrap().ends_with("preset geth"),
        "{out}"
    );
}

#[test]
fn paginate_concatenates_pages_and_stops_at_max_pages() {
    let served = serve();
    let dir = workspace(&served, true);
    let (code, json, err) = query(&dir, &["transfers", "--paginate", "--json"]);
    assert_eq!(code, 0, "{err}");
    let value: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["data"]["transfers"].as_array().unwrap().len(), 2500);
    let (code, json, err) = query(
        &dir,
        &[
            "transfers",
            "--paginate",
            "id_gt",
            "--max-pages",
            "2",
            "--json",
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("warning: stopped at --max-pages 2"), "{err}");
    let value: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["data"]["transfers"].as_array().unwrap().len(), 2000);
    let (code, _, err) = query(&dir, &["token", "--paginate"]);
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("no top-level field takes `first`"), "{err}");
}

#[test]
fn max_pages_is_bounded() {
    let served = serve();
    let dir = workspace(&served, true);
    for pages in ["0", "1001"] {
        let (code, _, err) = query(&dir, &["transfers", "--paginate", "--max-pages", pages]);
        assert_eq!(code, 2, "--max-pages {pages}: {err}");
        assert!(err.contains("--max-pages"), "{err}");
    }
    let (code, _, err) = query(&dir, &["transfers", "--paginate", "--max-pages", "1000"]);
    assert_eq!(code, 0, "{err}");
}

#[test]
fn a_failing_page_keeps_the_pages_read_before_it() {
    let served = serve();
    let dir = workspace(&served, true);
    let (code, json, err) = query(&dir, &["failing", "--paginate", "--json"]);
    assert_ne!(code, 0, "{json}{err}");
    let value: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["data"]["failing"].as_array().unwrap().len(), 2000);
    let message = value["errors"][0]["message"].as_str().unwrap_or_default();
    assert_eq!(
        message,
        "--paginate: page 3 failed (HTTP 502, not a GraphQL result); \
         keeping the 2 pages read before it",
        "{json}"
    );
}

#[test]
fn run_fails_a_stale_subgraph_and_compares_raw_values() {
    let served = serve();
    let dir = workspace(&served, true);
    let output = rustman(dir.path())
        .args(["run", "stale.rman.yaml", "--env", "dev"])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(1), "{out}");
    assert!(out.contains("subgraph lag 120 blocks exceeds 50"), "{out}");
    assert!(
        !out.contains("data $.token.owner"),
        "the raw comparison passes: {out}"
    );
}
