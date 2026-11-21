//! Blockchain GraphQL presets (add-evm-toolkit section 7) against a mock
//! subgraph and a stub EVM node: detection from introspection fixtures,
//! display-only scalar formatting, `id_gt` and `skip` pagination with the
//! page cap, and subgraph freshness under the runner. No live network.

mod common;

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rustman_core::assertions::Outcome;
use rustman_core::model::GraphqlPreset;
use rustman_core::run::{RequestRecord, RequestStatus, plan};
use rustman_core::{SecretStore, Workspace};
use rustman_evm::gql::{Display, RpcHead, find_links, format_data};
use rustman_graphql::preset::{NOT_GETH, NOT_SUBGRAPH, choose, detect};
use rustman_graphql::{GraphqlRunner, GraphqlSchema, PageMode, Paginate};
use rustman_http::HttpEngine;
use rustman_http::runner::{RunContext, RunOptions, Runner};
use rustman_test_support::jsonrpc::JsonrpcHttpServer;
use rustman_test_support::{GraphqlHttpServer, RecordedRequest, Reply};
use serde_json::{Value, json};

const OWNER: &str = "0xd8da6bf26964af9d7eed9e03e53415d37aa96045";
const OWNER_CHECKSUM: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";
const API_KEY: &str = "subgraph-key-canary-7f3a";

fn schema(name: &str) -> GraphqlSchema {
    let path = common::fixture(&format!("graphql/{name}.introspection.json"));
    GraphqlSchema::from_file(&path, name).unwrap()
}

#[test]
fn presets_are_detected_from_geth_and_subgraph_introspection() {
    let (geth, subgraph) = (schema("geth"), schema("subgraph"));
    assert_eq!(detect(&geth), Some(GraphqlPreset::Geth));
    assert_eq!(detect(&subgraph), Some(GraphqlPreset::Subgraph));
    let shown = choose(None, Some(&subgraph)).unwrap();
    assert_eq!((shown.name.as_str(), shown.explicit), ("subgraph", false));
    let forced = choose(Some(GraphqlPreset::Geth), Some(&subgraph)).unwrap();
    assert_eq!(forced.name, "geth");
    assert_eq!(forced.warning.as_deref(), Some(NOT_GETH));
    let forced = choose(Some(GraphqlPreset::Subgraph), Some(&geth)).unwrap();
    assert_eq!(forced.warning.as_deref(), Some(NOT_SUBGRAPH));
}

#[test]
fn geth_scalars_display_as_decimals_and_checksums() {
    let document = "{ block { number gasUsed hash miner { address balance } } }";
    let scalars = rustman_graphql::preset::scalar_paths(&schema("geth"), document, None);
    let data = json!({"block": {
        "number": "0x1312d00", "gasUsed": "0x5208",
        "hash": "0x88e96d4537bea4d9c05d12549907b32561d3bf31f45aae734cdc119f13406cb6",
        "miner": {"address": OWNER, "balance": "0xde0b6b3a7640000"}
    }});
    let shown = format_data(&data, &scalars, Display::Formatted);
    insta::assert_json_snapshot!("geth_formatted", shown);
    assert_eq!(format_data(&data, &scalars, Display::Raw), data);
}

/// A workspace whose `prod` environment has `variables`, with `files`.
fn workspace(variables: &str, files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let write = |rel: &str, text: &str| {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    write("rustman.toml", "[workspace]\nname = \"t\"\nversion = 1\n");
    write(
        "environments/prod.yaml",
        &format!("variables:\n{variables}"),
    );
    let fixture = common::fixture("graphql/subgraph.introspection.json");
    fs::copy(fixture, root.join("subgraph.json")).unwrap();
    for (rel, text) in files {
        write(rel, text);
    }
    dir
}

async fn run(root: &Path, graphql: &GraphqlRunner) -> Vec<RequestRecord> {
    let ws = Workspace::load(root).unwrap_or_else(|e| panic!("{e}"));
    let env = ws.active_environment(Some("prod"), None).unwrap();
    let store = SecretStore::with_keychain(root, None);
    let options = RunOptions {
        graphql: Some(graphql),
        ..RunOptions::default()
    };
    let ctx = RunContext {
        root: &ws.root,
        settings: &ws.settings,
        environment: env,
        secrets: &store,
        in_workspace: true,
    };
    let runner = Runner::new(ctx, options).unwrap();
    runner.run(&plan(&ws, &[], &[])).await.requests
}

fn runner() -> GraphqlRunner {
    GraphqlRunner {
        chain_head: Some(Arc::new(RpcHead::new(Arc::new(HttpEngine::new())))),
        ..GraphqlRunner::default()
    }
}

/// A subgraph indexed up to `indexed`, holding one token owned by
/// [`OWNER`].
fn subgraph(indexed: u64) -> impl Fn(&RecordedRequest) -> Reply + Send + Sync + 'static {
    move |hit| {
        if hit.body_text().contains("_meta") {
            return Reply::graphql(json!({"data": {"_meta": {
                "block": {"number": indexed}, "hasIndexingErrors": false
            }}}));
        }
        Reply::graphql(json!({"data": {"token": {
            "id": OWNER, "decimals": 18,
            "totalSupply": "1000000000000000000000"
        }, "transfers": [{"id": "1", "from": OWNER, "to": OWNER, "value": "5",
            "blockNumber": "7", "transactionHash":
            "0x5c504ed432cb51138bcf09aa5e8a410dd4a1e204ef84bfed1be16dfba1b22060",
            "token": {"id": OWNER, "decimals": 18, "totalSupply": "1"}}]}}))
    }
}

const TOKEN: &str = "\
version: 1
kind: graphql
name: Token
endpoint: '{{subgraph}}'
schema_file: subgraph.json
document: 'query { token(id: \"x\") { id decimals totalSupply } }'
headers:
  - name: Authorization
    value: 'Bearer subgraph-key-canary-7f3a'
assertions:
  - data: {path: $.token.id, equals: '0xd8da6bf26964af9d7eed9e03e53415d37aa96045'}
  - data: {path: $.token.totalSupply, equals: '1000000000000000000000'}
  - subgraph: {max_lag_blocks: 50}
";

async fn head_node(head: u64, seen: Arc<Mutex<Vec<RecordedRequest>>>) -> JsonrpcHttpServer {
    JsonrpcHttpServer::start(move |hit, _| {
        seen.lock().unwrap().push(hit.clone());
        let request: Value = serde_json::from_slice(&hit.body).unwrap();
        assert_eq!(request["method"], "eth_blockNumber", "read-only head only");
        common::result(&request, json!(format!("{head:#x}")))
    })
    .await
}

#[tokio::test]
async fn a_stale_subgraph_fails_its_lag_assertion_and_raw_values_compare() {
    let graph = GraphqlHttpServer::start(subgraph(1000)).await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let node = head_node(1120, seen.clone()).await;
    let variables = format!(
        "  subgraph: {}\n  rpc: {}\n",
        graph.url("/subgraphs/name/t"),
        node.url("/")
    );
    let dir = workspace(&variables, &[("token.rman.yaml", TOKEN)]);
    let records = run(dir.path(), &runner()).await;
    let record = &records[0];
    assert_eq!(
        record.status,
        RequestStatus::Failed,
        "{:?}",
        record.assertions
    );
    let outcomes: Vec<_> = record.assertions.iter().map(|a| a.outcome).collect();
    assert_eq!(outcomes, [Outcome::Pass, Outcome::Pass, Outcome::Fail]);
    assert_eq!(
        record.assertions[2].message,
        "subgraph lag 120 blocks exceeds 50"
    );
    let preset = record.graphql.as_ref().unwrap().preset.as_ref().unwrap();
    assert_eq!((preset.name.as_str(), preset.explicit), ("subgraph", false));
    let facts = preset.freshness.as_ref().unwrap();
    assert_eq!(facts.lag_text(), "lag 120 blocks");
    assert_eq!(preset.scalars["token.totalSupply"], "BigInt");
    let node_hits = seen.lock().unwrap();
    assert_eq!(node_hits.len(), 1);
    assert!(
        node_hits[0].header("authorization").is_none(),
        "the subgraph's key never goes to the node"
    );
    assert!(!node_hits[0].body_text().contains(API_KEY));
}

#[tokio::test]
async fn a_fresh_subgraph_passes() {
    let graph = GraphqlHttpServer::start(subgraph(1100)).await;
    let node = head_node(1120, Arc::default()).await;
    let variables = format!("  subgraph: {}\n  rpc: {}\n", graph.url("/"), node.url("/"));
    let dir = workspace(&variables, &[("token.rman.yaml", TOKEN)]);
    let records = run(dir.path(), &runner()).await;
    assert_eq!(
        records[0].status,
        RequestStatus::Passed,
        "{:?}",
        records[0].assertions
    );
}

#[tokio::test]
async fn without_an_rpc_the_lag_is_unknown_and_the_assertion_fails() {
    let graph = GraphqlHttpServer::start(subgraph(1000)).await;
    let variables = format!("  subgraph: {}\n", graph.url("/"));
    let dir = workspace(&variables, &[("token.rman.yaml", TOKEN)]);
    let records = run(dir.path(), &runner()).await;
    let failed = &records[0].assertions[2];
    assert_eq!(failed.outcome, Outcome::Fail);
    assert_eq!(failed.message, "cannot compute lag: no rpc configured");
    let preset = records[0].graphql.as_ref().unwrap().preset.clone().unwrap();
    assert_eq!(preset.freshness.unwrap().lag_text(), "unknown (no rpc)");
}

const NOT_GETH_FILE: &str = "\
version: 1
kind: graphql
name: Forced
endpoint: '{{subgraph}}'
schema_file: subgraph.json
preset: geth
document: 'query { token(id: \"x\") { id } }'
";

#[tokio::test]
async fn an_explicit_preset_wins_with_a_warning() {
    let graph = GraphqlHttpServer::start(subgraph(1000)).await;
    let variables = format!("  subgraph: {}\n", graph.url("/"));
    let dir = workspace(&variables, &[("forced.rman.yaml", NOT_GETH_FILE)]);
    let records = run(dir.path(), &runner()).await;
    let record = &records[0];
    assert!(
        record.warnings.iter().any(|w| w == NOT_GETH),
        "{:?}",
        record.warnings
    );
    let preset = record.graphql.as_ref().unwrap().preset.as_ref().unwrap();
    assert_eq!((preset.name.as_str(), preset.explicit), ("geth", true));
    assert!(preset.freshness.is_none(), "geth has no _meta");
    assert_eq!(graph.hits().len(), 1, "no _meta query for geth");
}

#[test]
fn subgraph_results_display_formatted_with_a_units_toggle_and_links() {
    let document = "{ token(id: \"x\") { id decimals totalSupply } \
                    transfers { from value transactionHash } }";
    let scalars = rustman_graphql::preset::scalar_paths(&schema("subgraph"), document, None);
    let reply = subgraph(1)(&RecordedRequest {
        method: "POST".into(),
        uri: "/".into(),
        headers: Vec::new(),
        body: "{}".into(),
    });
    let data = serde_json::from_str::<Value>(&reply.body).unwrap()["data"].clone();
    let shown = format_data(&data, &scalars, Display::Formatted);
    assert_eq!(shown["token"]["totalSupply"], "1000000000000000000000");
    assert_eq!(shown["transfers"][0]["from"], OWNER_CHECKSUM);
    let units = format_data(&data, &scalars, Display::Units);
    assert_eq!(units["token"]["totalSupply"], "1000 (18 decimals)");
    let links = find_links(&data);
    let values: Vec<_> = links.iter().map(|l| l.value.as_str()).collect();
    assert_eq!(values.len(), 2, "{values:?}");
    assert!(values.contains(&OWNER_CHECKSUM));
}

const TRANSFERS: &str = "\
version: 1
kind: graphql
name: Transfers
endpoint: '{{subgraph}}'
schema_file: subgraph.json
document: 'query T($last: ID) { transfers(first: 1000, where: {id_gt: $last}) { id value } }'
";

/// A subgraph holding `total` transfers with ids `t00000`..., served in
/// `id_gt` order.
fn paged_subgraph(total: usize) -> impl Fn(&RecordedRequest) -> Reply + Send + Sync + 'static {
    move |hit| {
        let body = hit.json();
        let after = body["variables"]["last"].as_str().unwrap_or("").to_owned();
        let page: Vec<Value> = (0..total)
            .map(|i| format!("t{i:05}"))
            .filter(|id| *id > after)
            .take(1000)
            .map(|id| json!({"id": id, "value": "1"}))
            .collect();
        Reply::graphql(json!({"data": {"transfers": page}}))
    }
}

#[tokio::test]
async fn id_gt_pagination_concatenates_pages_until_a_short_one() {
    let graph = GraphqlHttpServer::start(paged_subgraph(2500)).await;
    let variables = format!("  subgraph: {}\n", graph.url("/"));
    let dir = workspace(&variables, &[("transfers.rman.yaml", TRANSFERS)]);
    let paged = GraphqlRunner {
        paginate: Some(Paginate::default()),
        ..runner()
    };
    let records = run(dir.path(), &paged).await;
    assert_eq!(records[0].status, RequestStatus::Passed, "{:?}", records[0]);
    let cursors: Vec<Value> = graph
        .hits()
        .iter()
        .map(|h| h.json()["variables"]["last"].clone())
        .collect();
    assert_eq!(cursors, [json!(""), json!("t00999"), json!("t01999")]);
    let exchange = records[0].exchange.as_ref().unwrap();
    let body: Value = serde_json::from_str(exchange.response.body.as_deref().unwrap()).unwrap();
    let items = body["data"]["transfers"].as_array().unwrap();
    assert_eq!(items.len(), 2500);
    assert_eq!(items[2499]["id"], "t02499");
}

#[tokio::test]
async fn pagination_stops_at_the_page_cap_with_a_warning() {
    let graph = GraphqlHttpServer::start(paged_subgraph(30_000)).await;
    let variables = format!("  subgraph: {}\n", graph.url("/"));
    let dir = workspace(&variables, &[("transfers.rman.yaml", TRANSFERS)]);
    let paged = GraphqlRunner {
        paginate: Some(Paginate::default()),
        ..runner()
    };
    let records = run(dir.path(), &paged).await;
    assert_eq!(graph.hits().len(), 20);
    let warnings = &records[0].warnings;
    assert!(
        warnings.iter().any(|w| w == "stopped at --max-pages 20"),
        "{warnings:?}"
    );
}

const SKIPPED: &str = "\
version: 1
kind: graphql
name: Skipped
endpoint: '{{subgraph}}'
schema_file: subgraph.json
document: 'query S($skip: Int) { tokens(first: 2, skip: $skip) { id } }'
";

#[tokio::test]
async fn skip_pagination_moves_by_first() {
    let graph = GraphqlHttpServer::start(|hit| {
        let skip = hit.json()["variables"]["skip"].as_u64().unwrap() as usize;
        let tokens: Vec<Value> = (skip..5.min(skip + 2))
            .map(|i| json!({"id": i.to_string()}))
            .collect();
        Reply::graphql(json!({"data": {"tokens": tokens}}))
    })
    .await;
    let variables = format!("  subgraph: {}\n", graph.url("/"));
    let dir = workspace(&variables, &[("skipped.rman.yaml", SKIPPED)]);
    let paged = GraphqlRunner {
        paginate: Some(Paginate {
            mode: PageMode::Skip,
            max_pages: 20,
        }),
        ..runner()
    };
    let records = run(dir.path(), &paged).await;
    assert_eq!(records[0].status, RequestStatus::Passed, "{:?}", records[0]);
    let skips: Vec<Value> = graph
        .hits()
        .iter()
        .map(|h| h.json()["variables"]["skip"].clone())
        .collect();
    assert_eq!(skips, [json!(0), json!(2), json!(4)]);
}

const FRESH_WITHIN_ONE: &str = "\
version: 1
kind: graphql
name: Fresh
endpoint: '{{subgraph}}'
schema_file: subgraph.json
document: 'query { token(id: \"x\") { id } }'
assertions:
  - subgraph: {max_lag_blocks: 1}
";

/// Task 7.4 against a real node: the head comes from Anvil (mining a block
/// a second), the subgraph is stuck at block 0. Opt-in like the other
/// Anvil tests (`RUSTMAN_ANVIL=1`, required in CI).
#[tokio::test]
async fn the_lag_is_measured_against_an_anvil_head() {
    use rustman_evm::fork::{Anvil, AnvilOptions};
    let Some(program) = common::foundry_tool("anvil") else {
        return;
    };
    let mut options = AnvilOptions::new(program);
    options.port = 0;
    options.extra = vec!["--block-time".into(), "1".into()];
    let anvil = Anvil::start(&options).await.unwrap();
    let rpc = common::transport(&anvil.url());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while rustman_evm::query::block_number(&rpc).await.unwrap() < 3 {
        assert!(
            std::time::Instant::now() < deadline,
            "Anvil mines no blocks"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let graph = GraphqlHttpServer::start(subgraph(0)).await;
    let variables = format!("  subgraph: {}\n  rpc: {}\n", graph.url("/"), anvil.url());
    let dir = workspace(&variables, &[("fresh.rman.yaml", FRESH_WITHIN_ONE)]);
    let records = run(dir.path(), &runner()).await;
    let failed = &records[0].assertions[0];
    assert_eq!(failed.outcome, Outcome::Fail);
    assert!(
        failed.message.starts_with("subgraph lag "),
        "{}",
        failed.message
    );
    assert!(
        failed.message.ends_with(" blocks exceeds 1"),
        "{}",
        failed.message
    );
    anvil.stop().await;
}
