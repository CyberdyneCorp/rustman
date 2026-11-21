//! Blockchain GraphQL presets in the TUI (add-evm-toolkit tasks 7.1-7.4):
//! the preset on the schema line, formatted data with the units toggle and
//! the subgraph's lag, the pagination action, and result links into the
//! EVM toolkit, against a mock subgraph and a stub node replaying a
//! recorded mainnet USDC transfer. No live network.

use std::fs;
use std::path::{Path, PathBuf};

use rustman_core::{Secret, SecretBackend, SecretStore};
use rustman_test_support::jsonrpc::{JsonrpcHttpServer, RpcReply};
use rustman_test_support::{GraphqlHttpServer, Reply};
use serde_json::{Value, json};

use crate::app::{Action, Effect, Overlay, State, update};
use crate::chain_links::{LookupKind, NO_RPC};
use crate::config::Overrides;
use crate::effects::execute;
use crate::focus::Focus;
use crate::snapshot::{buffer_text, render};
use crate::theme::ColorMode;
use crate::workbench_tests::{event, press, run};

const HASH: &str = "0xdeed52ec1e92d0ca8834284f39ed5e37ff6106b6bb09a9dc37fb6d1c01b0a8f6";
const OWNER: &str = "0x7e4c83cb3b38c128e42286db909e410b1bc60038";
const OWNER_CHECKSUM: &str = "0x7e4c83cb3b38C128E42286dB909E410B1bC60038";
const CANARY: &str = "canary-subgraph-key-91c2";

const SDL: &str = "scalar BigInt\nscalar Bytes\n\
type _Block_ { number: Int! }\ntype _Meta_ { block: _Block_! hasIndexingErrors: Boolean! }\n\
type Token { id: ID! decimals: Int! totalSupply: BigInt! }\n\
type Transfer { id: ID! from: Bytes! value: BigInt! transactionHash: Bytes! token: Token! }\n\
input Transfer_filter { id_gt: ID }\n\
type Query { _meta: _Meta_ transfers(first: Int, where: Transfer_filter): [Transfer!]! }\n";

/// A workspace with the subgraph request `transfers` (its key the secret
/// `key`, [`CANARY`]) and `forced` (`preset: geth`), and the environment
/// `local`, pinned to mainnet, whose `rpc` is `rpc` when given.
fn workspace(subgraph: &str, rpc: Option<&str>) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    let write = |rel: &str, text: &str| {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    write(
        "rustman.toml",
        "[workspace]\nname = \"demo\"\nversion = 1\n\n[defaults]\ndefault_environment = \"local\"\n",
    );
    let rpc = rpc.map(|url| format!("  rpc: {url}\n")).unwrap_or_default();
    write(
        "environments/local.yaml",
        &format!("chain_id: 1\nvariables:\n  subgraph: {subgraph}\n{rpc}secrets: [key]\n"),
    );
    write("graph/schema.graphql", SDL);
    let document = "query T($last: ID) { transfers(first: 2, where: {id_gt: $last}) \
                    { id from value transactionHash token { decimals totalSupply } } }";
    write(
        "graph/transfers.rman.yaml",
        &format!(
            "version: 1\nkind: graphql\nname: Transfers\nendpoint: '{{{{subgraph}}}}'\n\
             schema_file: schema.graphql\ndocument: '{document}'\n\
             headers:\n  - {{name: Authorization, value: 'Bearer {{{{key}}}}'}}\n"
        ),
    );
    write(
        "graph/forced.rman.yaml",
        "version: 1\nkind: graphql\nname: Forced\nendpoint: '{{subgraph}}'\n\
         schema_file: schema.graphql\npreset: geth\ndocument: '{ _meta { hasIndexingErrors } }'\n",
    );
    SecretStore::open(&root, SecretBackend::File)
        .set("local", &Secret::new("key", CANARY.to_owned()))
        .unwrap();
    tmp
}

fn shell(tmp: &tempfile::TempDir, width: u16, height: u16) -> State {
    let root = tmp.path().join("ws");
    let config = tmp.path().join("config");
    fs::create_dir_all(&config).unwrap();
    let (mut state, effects) = crate::initial_state_with(
        &root,
        Overrides::default(),
        Some(&config),
        ColorMode::TrueColor,
    );
    run(&mut state, effects);
    state.secret_backend = Some(SecretBackend::File);
    update(&mut state, Action::Resize { width, height });
    press(&mut state, "Ctrl+B");
    state
}

fn open(state: &mut State, path: &str) -> Vec<Effect> {
    crate::workbench::open_path(state, Path::new(path));
    update(state, Action::Tick(state.now))
}

fn text(state: &State) -> String {
    buffer_text(&render(state, state.area.width, state.area.height))
}

/// `text` with response times masked (`#.# ms`, the status line's runs of
/// spaces collapsed, as the time's width varies), for snapshots.
fn stable(text: &str) -> String {
    text.lines()
        .map(|line| match line.find(" ms · ") {
            Some(end) => {
                let start = line[..end]
                    .rfind(|c: char| !(c.is_ascii_digit() || c == '.'))
                    .map_or(0, |i| i + 1);
                let masked = format!("{}#.# ms{}", &line[..start], &line[end + 3..]);
                masked
                    .split(' ')
                    .filter(|w| !w.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
                    + "\n"
            }
            None => format!("{line}\n"),
        })
        .collect()
}

/// Run effects and their follow-ups on blocking threads.
async fn settle(state: &mut State, mut effects: Vec<Effect>) {
    while let Some(effect) = effects.pop() {
        let done = tokio::task::spawn_blocking(move || execute(effect))
            .await
            .unwrap();
        effects.extend(update(state, done));
    }
}

/// A subgraph indexed up to block 1000 with one transfer per `id_gt` page
/// of 2 (three transfers in all).
async fn subgraph() -> GraphqlHttpServer {
    GraphqlHttpServer::start(|hit| {
        let body = hit.json();
        let query = body["query"].as_str().unwrap_or_default();
        if query.contains("_meta {") && !query.contains("transfers") {
            return Reply::graphql(json!({"data": {"_meta": {
                "block": {"number": 1000}, "hasIndexingErrors": false
            }}}));
        }
        let after = body["variables"]["last"].as_str().unwrap_or("").to_owned();
        let page: Vec<Value> = ["a", "b", "c"]
            .into_iter()
            .filter(|id| *id > after.as_str())
            .take(2)
            .map(|id| {
                json!({"id": id, "from": OWNER, "value": "250000000",
                    "transactionHash": HASH,
                    "token": {"decimals": 6, "totalSupply": "1000000000"}})
            })
            .collect();
        Reply::graphql(json!({"data": {"transfers": page}}))
    })
    .await
}

fn fixture() -> Vec<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../rustman-evm/tests/fixtures/mainnet/usdc-transfer.json");
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

/// A mainnet node at head 1012 replaying the recorded USDC transfer;
/// anything else is an unknown method. It answers read methods only.
async fn node() -> JsonrpcHttpServer {
    let recorded = fixture();
    JsonrpcHttpServer::start(move |hit, _| {
        let body = hit.json();
        let method = body["method"].as_str().unwrap_or_default().to_owned();
        assert!(
            !method.contains("send"),
            "a write reached the node: {method}"
        );
        let params = &body["params"];
        let found = recorded.iter().find(|r| {
            r["method"] == method.as_str()
                && match method.as_str() {
                    "eth_call" => r["params"][0]["data"] == params[0]["data"],
                    _ => r["params"] == *params,
                }
        });
        let reply = match (found, method.as_str()) {
            (Some(r), _) => json!({"jsonrpc": "2.0", "id": body["id"], "result": r["result"]}),
            (None, "eth_blockNumber") => {
                json!({"jsonrpc": "2.0", "id": body["id"], "result": "0x3f4"})
            }
            (None, _) => json!({"jsonrpc": "2.0", "id": body["id"],
                "error": {"code": -32601, "message": "method not found"}}),
        };
        RpcReply::json(&reply)
    })
    .await
}

#[test]
fn the_schema_line_shows_the_detected_or_forced_preset() {
    // Scenarios "Subgraph detected" and "Explicit preset wins".
    let tmp = workspace("http://127.0.0.1:9", None);
    let mut state = shell(&tmp, 240, 24);
    let effects = open(&mut state, "graph/transfers.rman.yaml");
    run(&mut state, effects);
    let screen = text(&state);
    assert!(screen.contains("· preset subgraph"), "{screen}");
    insta::assert_snapshot!("graphql_preset_detected", screen);
    let effects = open(&mut state, "graph/forced.rman.yaml");
    run(&mut state, effects);
    let screen = text(&state);
    assert!(
        screen.contains("· preset geth · schema does not look like geth EIP-1767"),
        "{screen}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn results_are_formatted_with_the_lag_and_links_decode_a_transaction() {
    let (graph, node) = (subgraph().await, node().await);
    let tmp = workspace(&graph.url("/"), Some(&node.url("/")));
    let mut state = shell(&tmp, 220, 34);
    let effects = open(&mut state, "graph/transfers.rman.yaml");
    settle(&mut state, effects).await;
    let effects = update(&mut state, Action::Key(event("Ctrl+R")));
    settle(&mut state, effects).await;
    state.shell.focus = Focus::Response;
    let data = text(&state);
    assert!(
        data.contains("query T · ok · preset subgraph · lag 12 blocks · formatted"),
        "{data}"
    );
    assert!(data.contains(OWNER_CHECKSUM), "{data}");
    press(&mut state, "u");
    let units = text(&state);
    assert!(units.contains("\"1000 (6 decimals)\""), "{units}");
    insta::assert_snapshot!("graphql_preset_units", stable(&units));
    assert!(!units.contains(CANARY));

    // Scenario "Open transaction from result".
    press(&mut state, "L");
    let Some(Overlay::ChainLinks(view)) = &state.overlay else {
        panic!("expected the links, got {:?}", state.overlay);
    };
    assert_eq!(view.explorer.as_deref(), Some("https://etherscan.io"));
    let links = text(&state);
    insta::assert_snapshot!("graphql_result_links", stable(&links));
    let tx = view.links.iter().position(|l| l.value == HASH).unwrap();
    for _ in 0..tx {
        press(&mut state, "Down");
    }
    press(&mut state, "Enter");
    let actions = text(&state);
    assert!(actions.contains("Decode transaction"), "{actions}");
    assert!(actions.contains("Open in explorer"), "{actions}");
    let effects = update(&mut state, Action::Key(event("Enter")));
    let [Effect::EvmLookup(job)] = effects.as_slice() else {
        panic!("Decode transaction looks the hash up: {effects:?}");
    };
    assert_eq!(
        (job.kind, job.value.as_str()),
        (LookupKind::Transaction, HASH)
    );
    settle(&mut state, effects).await;
    let decoded = text(&state);
    assert!(decoded.contains("EVM transaction 0xdeed…a8f6"), "{decoded}");
    assert!(decoded.contains("chain: Ethereum Mainnet (1)"), "{decoded}");
    assert!(decoded.contains("transfer"), "{decoded}");
    insta::assert_snapshot!("graphql_link_decoded_tx", stable(&decoded));
    for hit in node.hits() {
        assert!(hit.header("authorization").is_none(), "no subgraph key");
        assert!(!hit.body_text().contains(CANARY));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn without_an_rpc_the_lag_is_unknown_and_lookups_say_why() {
    let graph = subgraph().await;
    let tmp = workspace(&graph.url("/"), None);
    let mut state = shell(&tmp, 220, 34);
    let effects = open(&mut state, "graph/transfers.rman.yaml");
    settle(&mut state, effects).await;
    let effects = update(&mut state, Action::Key(event("Ctrl+R")));
    settle(&mut state, effects).await;
    state.shell.focus = Focus::Response;
    let data = text(&state);
    assert!(
        data.contains("preset subgraph · unknown (no rpc)"),
        "{data}"
    );
    press(&mut state, "L");
    press(&mut state, "Enter");
    let effects = update(&mut state, Action::Key(event("Enter")));
    settle(&mut state, effects).await;
    let shown = text(&state);
    assert!(shown.contains(NO_RPC), "{shown}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_paginate_action_concatenates_pages() {
    let graph = subgraph().await;
    let tmp = workspace(&graph.url("/"), None);
    let mut state = shell(&tmp, 130, 40);
    let effects = open(&mut state, "graph/transfers.rman.yaml");
    settle(&mut state, effects).await;
    let effects = update(&mut state, Action::Key(event("Alt+G")));
    let [Effect::Send(job)] = effects.as_slice() else {
        panic!("Alt+G sends: {effects:?}");
    };
    assert!(job.paginate);
    settle(&mut state, effects).await;
    let pages: Vec<Value> = graph
        .hits()
        .iter()
        .map(|h| h.json())
        .filter(|b| {
            b["query"]
                .as_str()
                .unwrap_or_default()
                .contains("transfers")
        })
        .map(|b| b["variables"]["last"].clone())
        .collect();
    assert_eq!(pages, [json!(""), json!("b")]);
    state.shell.focus = Focus::Response;
    let data = text(&state);
    assert!(data.contains("\"id\": \"c\""), "{data}");
}
