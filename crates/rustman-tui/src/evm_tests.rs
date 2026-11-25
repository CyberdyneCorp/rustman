//! The EVM pane (add-evm-toolkit task 4.3) drawn on a `TestBackend`: the
//! function picker (view and pure functions by default, all on toggle),
//! typed argument fields, the block selector, the decoded result with unit
//! toggles, a revert, the trace and the event timeline, and an `evm-tx`
//! opened read-only (signed only by `evm.sign`, see `evm_sign_tests`).

use std::path::Path;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rustman_core::model::{KindRegistry, Request};
use serde_json::json;

use crate::config::UiOptions;
use crate::evm_pane::{EvmEditor, EvmTab, set_key};
use crate::focus::Focus;
use crate::keymap::chord::Code;
use crate::keymap::{Chord, Keymap};
use crate::pane::{PaneEditor, Registry, RenderCtx};
use crate::snapshot::buffer_text;
use crate::vars_view::VarScope;

const CALL: &str = "version: 1\nkind: evm-call\nname: Balance\n# the token\nrpc: \"{{rpc}}\"\nto: \"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48\"\nabi: erc20\nfunction: balanceOf\nargs:\n  - \"0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045\"\n";

fn load(text: &str) -> Request {
    KindRegistry::default()
        .load(Path::new("balance.rman.yaml"), text)
        .unwrap()
}

fn editor(text: &str) -> Box<dyn PaneEditor> {
    Registry::builtin().open(&load(text))
}

fn draw(editor: &dyn PaneEditor, focus: Focus) -> String {
    let ui = UiOptions::default();
    let vars = VarScope::empty();
    let keymap = Keymap::default();
    let ctx = RenderCtx {
        ui: &ui,
        focus,
        vars: &vars,
        keymap: &keymap,
    };
    let mut terminal = Terminal::new(TestBackend::new(90, 14)).unwrap();
    terminal
        .draw(|frame| editor.render(frame, frame.area(), &ctx))
        .unwrap();
    buffer_text(terminal.backend().buffer())
}

fn press(editor: &mut dyn PaneEditor, code: Code) -> bool {
    editor.key(Focus::EditorBody, Chord::plain(code), None)
}

#[test]
fn the_picker_lists_view_and_pure_functions_by_default() {
    // Scenario "Function picker".
    let mut editor = editor(CALL);
    assert_eq!(editor.kind(), "evm-call");
    assert_eq!(
        editor.tabs(),
        ["Function", "Args", "Block", "Result", "Trace", "Events"]
    );
    insta::assert_snapshot!(
        "evm_picker_read_only",
        draw(editor.as_ref(), Focus::EditorBody)
    );
    assert!(press(editor.as_mut(), Code::Char('a')));
    insta::assert_snapshot!("evm_picker_all", draw(editor.as_ref(), Focus::EditorBody));
    let evm = editor.evm().unwrap();
    assert!(evm.functions().iter().any(|f| f.name == "transfer"));
}

#[test]
fn picking_a_function_types_its_arguments_into_the_file() {
    let mut editor = editor(CALL);
    let names: Vec<String> = editor
        .evm()
        .unwrap()
        .functions()
        .iter()
        .map(|f| f.name.clone())
        .collect();
    let allowance = names.iter().position(|n| n == "allowance").unwrap();
    for _ in 0..allowance {
        press(editor.as_mut(), Code::Down);
    }
    assert!(press(editor.as_mut(), Code::Enter));
    assert_eq!(editor.evm().unwrap().tab, EvmTab::Args);
    for c in "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045".chars() {
        press(editor.as_mut(), Code::Char(c));
    }
    press(editor.as_mut(), Code::Enter);
    for c in "{{spender}}".chars() {
        press(editor.as_mut(), Code::Char(c));
    }
    insta::assert_snapshot!("evm_args", draw(editor.as_ref(), Focus::EditorBody));
    assert_eq!(
        editor.variable_at_cursor(Focus::EditorBody).as_deref(),
        Some("spender")
    );
    let Request::Evm(evm) = editor.request().unwrap() else {
        panic!("not evm");
    };
    let common = evm.common();
    assert_eq!(common.function.unwrap().get(), "allowance");
    assert_eq!(common.args.len(), 2);
    assert!(common.source.contains("# the token"), "comments are kept");
}

#[test]
fn the_block_selector_cycles_tags() {
    let mut editor = editor(CALL);
    editor.set_tab(2);
    assert!(press(editor.as_mut(), Code::Right));
    insta::assert_snapshot!("evm_block", draw(editor.as_ref(), Focus::EditorBody));
    let Request::Evm(evm) = editor.request().unwrap() else {
        panic!("not evm");
    };
    let EvmRequest::Call(call) = evm.as_ref() else {
        panic!("not a call");
    };
    assert_eq!(call.block.as_ref().unwrap().as_str(), "finalized");
}

use rustman_core::model::EvmRequest;

fn result_record() -> serde_json::Value {
    json!({
        "status": "Ethereum Mainnet (1) · geth 1.14.11",
        "result": {
            "function": "balanceOf(address)",
            "outputs": {"balance": "1500000000000000000"},
            "reverted": null,
            "gas_estimate": "29000",
            "token": null,
        },
        "trace": {"tree": "CALL 0xA0b8…eB48 balanceOf(account=0xd8dA…6045) gas=29000/24000\n  STATICCALL 0x43…01 implementation()"},
    })
}

#[test]
fn the_result_shows_outputs_in_unit_forms() {
    let mut editor = editor(CALL);
    editor.evm_mut().unwrap().set_result(&result_record());
    assert_eq!(editor.tab(), 3);
    insta::assert_snapshot!(
        "evm_result_native",
        draw(editor.as_ref(), Focus::EditorBody)
    );
    assert!(press(editor.as_mut(), Code::Char('u')));
    let wei = draw(editor.as_ref(), Focus::EditorBody);
    assert!(wei.contains("balance = 1500000000000000000 wei"), "{wei}");
    editor.cycle_mode();
    editor.cycle_mode();
    let hex = draw(editor.as_ref(), Focus::EditorBody);
    assert!(hex.contains("balance = 0x14d1120d7b160000"), "{hex}");
    editor.set_tab(4);
    insta::assert_snapshot!("evm_trace", draw(editor.as_ref(), Focus::EditorBody));
}

#[test]
fn token_amounts_and_reverts() {
    let mut editor = editor(CALL);
    let mut record = result_record();
    record["result"]["outputs"] = json!({"0": "250000000"});
    record["result"]["token"] = json!({"decimals": 6, "symbol": "USDC"});
    editor.evm_mut().unwrap().set_result(&record);
    let shown = draw(editor.as_ref(), Focus::EditorBody);
    assert!(shown.contains("0 = 250 USDC"), "{shown}");
    let reverted = json!({
        "status": "Ethereum Mainnet (1) · geth 1.14.11",
        "result": {"outputs": null, "reverted": {"message": "reverted: Panic(0x12) division or modulo by zero", "name": "Panic"}},
    });
    editor.evm_mut().unwrap().set_result(&reverted);
    insta::assert_snapshot!("evm_revert", draw(editor.as_ref(), Focus::EditorBody));
}

#[test]
fn the_event_timeline_marks_reorgs() {
    let mut editor = editor(CALL);
    let evm = editor.evm_mut().unwrap();
    evm.push_event("[1] 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48 Transfer(from=0xd8dA…, to=0x…ab, value=250.00 USDC)");
    evm.push_event("REORGED [1] 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48 Transfer(from=0xd8dA…, to=0x…ab, value=250.00 USDC)");
    editor.set_tab(5);
    insta::assert_snapshot!("evm_events", draw(editor.as_ref(), Focus::EditorBody));
}

#[test]
fn results_feed_the_event_timeline_which_stays_bounded() {
    // Regression: nothing but tests reached the timeline, and it grew
    // without bound.
    let mut editor = editor(CALL);
    let evm = editor.evm_mut().unwrap();
    let mut record = result_record();
    record["events"] = json!([
        "Transfer(from=0xd8dA…, to=0xd8dA…, value=1)",
        "Odd\u{202e}x"
    ]);
    evm.set_result(&record);
    assert_eq!(evm.events.len(), 2);
    assert_eq!(evm.events[1], "Odd\\u{202e}x");
    for i in 0..crate::evm_pane::MAX_EVENTS + 5 {
        evm.push_event(format!("event {i}"));
    }
    assert_eq!(evm.events.len(), crate::evm_pane::MAX_EVENTS);
    assert_eq!(evm.events[0], "event 5");
}

#[test]
fn an_evm_tx_opens_read_only() {
    let tx = CALL
        .replace("kind: evm-call", "kind: evm-tx")
        .replace("function: balanceOf", "function: transfer")
        .replace(
            "args:\n  - \"0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045\"\n",
            "args: [\"0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045\", \"1\"]\n",
        );
    let mut editor = editor(&tx);
    assert_eq!(editor.kind(), "evm-tx");
    assert_eq!(editor.notice().as_deref(), Some(crate::evm_sign::TX_NOTICE));
    assert!(!press(editor.as_mut(), Code::Down));
}

#[test]
fn a_file_abi_loads_from_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("vault.json"),
        r#"[{"type":"function","name":"owner","stateMutability":"view","inputs":[],"outputs":[{"name":"","type":"address"}]}]"#,
    )
    .unwrap();
    let text = CALL
        .replace("abi: erc20", "abi: vault.json")
        .replace("function: balanceOf", "function: owner")
        .replace(
            "args:\n  - \"0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045\"\n",
            "",
        );
    let mut editor = editor(&text);
    assert!(editor.notice().unwrap().contains("vault.json"));
    editor
        .evm_mut()
        .unwrap()
        .load_abi(dir.path(), Path::new("balance.rman.yaml"));
    assert_eq!(editor.notice(), None);
    let names: Vec<String> = editor
        .evm()
        .unwrap()
        .functions()
        .iter()
        .map(|f| f.name.clone())
        .collect();
    assert_eq!(names, ["owner"]);
}

#[test]
fn top_level_keys_are_replaced_with_their_blocks() {
    let text = "a: 1\nargs:\n  - x\n  - y\nb: 2\n";
    assert_eq!(
        set_key(text, "args", "[\"z\"]"),
        "a: 1\nargs: [\"z\"]\nb: 2\n"
    );
    assert_eq!(set_key("a: 1\n", "block", "safe"), "a: 1\nblock: safe\n");
    let _ = EvmEditor::new;
}
