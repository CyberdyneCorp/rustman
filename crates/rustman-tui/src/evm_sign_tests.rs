//! Signing an `evm-tx` from the TUI (issue #112): the confirmation dialog
//! drawn on a `TestBackend` (Sepolia, mainnet with its typed name, a
//! forced revert), its keys (Cancel preselected, `Esc` cancels), the
//! console between the gate and the loop, the refusals the shared gate
//! makes before anything is signed, and, with `RUSTMAN_ANVIL=1`, a
//! transaction signed, sent, mined and audited against a local Anvil.

use std::fs;
use std::path::{Path, PathBuf};

use alloy_primitives::{Address, U256};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rustman_core::SecretBackend;
use rustman_evm::ChainRegistry;
use rustman_evm_signing::fees::{Fee, Fees};
use rustman_evm_signing::transaction::UnsignedTx;
use rustman_evm_signing::{Console, Summary, SummaryParts};
use tokio::sync::mpsc::unbounded_channel;

use crate::app::{Action, Effect, Overlay, State, update};
use crate::config::{Overrides, UiOptions};
use crate::effects::execute;
use crate::evm_sign::{SignDialog, Stage, SummaryView, TuiConsole};
use crate::focus::Focus;
use crate::keymap::chord::{Chord, Code, parse_chord};
use crate::snapshot::{buffer_text, render};
use crate::theme::ColorMode;

fn summary(chain_id: u64, reverted: Option<&str>) -> Summary {
    summary_calling(
        chain_id,
        reverted,
        "transfer(to=0x2222222222222222222222222222222222222222, amount=10.00 USDC)\nchain  Mainnet",
    )
}

fn summary_calling(chain_id: u64, reverted: Option<&str>, call: &str) -> Summary {
    let gwei = |n: u64| Fee {
        wei: U256::from(n) * U256::from(1_000_000_000_u64),
        suggested: true,
    };
    Summary::new(SummaryParts {
        chain: ChainRegistry::built_in().get(chain_id),
        signer_id: "deployer".to_owned(),
        signer: Address::repeat_byte(0x11),
        to_name: Some("bob.eth".to_owned()),
        call: call.to_owned(),
        tx: UnsignedTx {
            chain_id,
            nonce: 7,
            to: Address::repeat_byte(0x22),
            value: U256::ZERO,
            input: vec![0xa9, 0x05, 0x9c, 0xbb],
            gas_limit: U256::from(65_000),
            fees: Fees::Eip1559 {
                max_fee: gwei(30),
                max_priority_fee: gwei(2),
            },
        },
        reverted: reverted.map(str::to_owned),
    })
    .unwrap()
}

fn draw(dialog: &SignDialog) -> String {
    draw_on(dialog, 100, 26)
}

fn draw_on(dialog: &SignDialog, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| crate::evm_sign::render(frame, dialog, &UiOptions::default()))
        .unwrap();
    buffer_text(terminal.backend().buffer())
}

fn key(text: &str) -> Chord {
    parse_chord(text).unwrap()
}

const QUESTION: &str = "sign and send this transaction? [y/N] ";

#[test]
fn the_sepolia_confirmation_shows_the_decoded_summary_with_cancel_preselected() {
    let view = SummaryView::new(&summary(11_155_111, None), None);
    let dialog = SignDialog::detached(Stage::Confirm, QUESTION, Some(view));
    let text = draw(&dialog);
    for expected in [
        "Sepolia (11155111)",
        "0x1111111111111111111111111111111111111111 (deployer)",
        "0x2222222222222222222222222222222222222222 (bob.eth)",
        "amount=10.00 USDC)\\u{a}",
        "nonce        7",
        "max cost",
    ] {
        assert!(text.contains(expected), "{expected}\n{text}");
    }
    assert!(!text.contains("SIMULATION REVERTED"), "{text}");
    insta::assert_snapshot!("evm_sign_sepolia", text);
}

#[test]
fn mainnet_asks_for_the_chain_name_typed() {
    let view = SummaryView::new(&summary(1, None), None);
    assert_eq!(view.chain, "Ethereum Mainnet");
    let mut dialog = SignDialog::detached(
        Stage::ChainName,
        "type the chain name (Ethereum Mainnet) to confirm: ",
        Some(view),
    );
    for c in "Ethereum".chars() {
        assert!(!dialog.key(Chord::plain(Code::Char(c))));
    }
    let text = draw(&dialog);
    assert!(text.contains("Ethereum Mainnet (1)"), "{text}");
    assert!(text.contains("› Ethereum"), "{text}");
    insta::assert_snapshot!("evm_sign_mainnet_typed", text);
}

#[test]
fn a_forced_revert_carries_the_banner_and_the_cost_warning() {
    let view = SummaryView::new(
        &summary(11_155_111, Some("InsufficientBalance(5, 10)")),
        Some("max cost 2 ETH is above [evm] max_cost (1 ETH)"),
    );
    let dialog = SignDialog::detached(Stage::Confirm, QUESTION, Some(view));
    let text = draw(&dialog);
    assert!(
        text.contains("SIMULATION REVERTED: InsufficientBalance(5, 10)"),
        "{text}"
    );
    assert!(text.contains("WARNING: max cost 2 ETH"), "{text}");
    insta::assert_snapshot!("evm_sign_forced_revert", text);
}

#[test]
fn the_password_is_never_drawn_nor_debug_printed() {
    let mut dialog = SignDialog::detached(
        Stage::Password,
        "password for keystore signer 'deployer': ",
        None,
    );
    for c in "hunter2-secret".chars() {
        dialog.key(Chord::plain(Code::Char(c)));
    }
    assert_eq!(dialog.typed_len(), 14);
    let text = draw(&dialog);
    assert!(!text.contains("hunter2"), "{text}");
    assert!(text.contains("(input hidden)"), "{text}");
    assert!(!format!("{dialog:?}").contains("hunter2"));
}

/// A console whose dialogs are answered by `answer` on a UI thread;
/// returns what each question got.
fn console_answers(
    answers: Vec<&'static str>,
    question: impl FnOnce(&TuiConsole) -> Vec<Option<String>>,
) -> (Vec<Option<String>>, Vec<Stage>, bool) {
    let (tx, mut rx) = unbounded_channel();
    let job = sign_job();
    let console = TuiConsole::new(&job, Some(tx));
    console.summary(&summary(1, None), None);
    let ui = std::thread::spawn(move || {
        let mut stages = Vec::new();
        let mut answers = answers.into_iter();
        while let Some(action) = rx.blocking_recv() {
            let Action::Sign(msg) = action else { continue };
            let crate::evm_sign::Msg::Ask(mut dialog) = *msg else {
                continue;
            };
            stages.push(dialog.stage);
            // Drawn, as the loop draws it before any key reaches it.
            draw(&dialog);
            match answers.next() {
                Some("Esc") | None => {
                    dialog.key(key("Esc"));
                }
                Some("Sign") => {
                    dialog.key(key("Left"));
                    dialog.key(key("Enter"));
                }
                Some("Enter") => {
                    dialog.key(key("Enter"));
                }
                Some(text) => {
                    for c in text.chars() {
                        dialog.key(Chord::plain(Code::Char(c)));
                    }
                    dialog.key(key("Enter"));
                }
            }
        }
        stages
    });
    let got = question(&console);
    let cancelled = console.cancelled();
    drop(console);
    (got, ui.join().unwrap(), cancelled)
}

#[test]
fn the_console_asks_through_the_dialog_and_esc_interrupts() {
    // Enter on the preselected Cancel declines; Sign then the typed name.
    let (got, stages, cancelled) = console_answers(vec!["Enter"], |c| vec![c.ask(QUESTION)]);
    assert_eq!(got, [Some("n".to_owned())]);
    assert_eq!(stages, [Stage::Confirm]);
    assert!(!cancelled);
    let (got, stages, _) = console_answers(vec!["Sign", "Ethereum Mainnet"], |c| {
        vec![c.ask(QUESTION), c.ask("type the chain name: ")]
    });
    assert_eq!(
        got,
        [Some("y".to_owned()), Some("Ethereum Mainnet".to_owned())]
    );
    assert_eq!(stages, [Stage::Confirm, Stage::ChainName]);
    // Esc: the gate is interrupted, nothing is signed.
    let (got, _, cancelled) = console_answers(vec!["Esc"], |c| vec![c.ask(QUESTION)]);
    assert_eq!(got, [None]);
    assert!(cancelled);
    let (got, stages, cancelled) = console_answers(vec!["Esc"], |c| {
        vec![c.password("pw: ").unwrap().map(|p| p.to_string())]
    });
    assert_eq!(
        (got, stages, cancelled),
        (vec![None], vec![Stage::Password], true)
    );
}

#[test]
fn without_a_dialog_nothing_can_be_confirmed() {
    let console = TuiConsole::new(&sign_job(), None);
    assert!(!console.interactive());
    assert_eq!(console.ask(QUESTION), None);
    assert_eq!(console.password("pw: ").unwrap(), None);
}

#[test]
fn a_summary_taller_than_the_screen_scrolls_and_signs_only_once_read() {
    // A multicall-sized calldata argument on an 80x24 terminal: the fields
    // after `call` are below the dialog's room.
    let call = format!("multicall(data=0x{})", "ab".repeat(1500));
    let view = SummaryView::new(&summary_calling(11_155_111, None, &call), None);
    let mut dialog = SignDialog::detached(Stage::Confirm, QUESTION, Some(view));
    let text = draw_on(&dialog, 80, 24);
    // The question, the choices and the hints stay on screen.
    for expected in [
        "[y/N]",
        "Sign and send",
        "Cancel",
        "Esc cancel",
        "more summary lines below",
    ] {
        assert!(text.contains(expected), "{expected}\n{text}");
    }
    assert!(!text.contains("max cost"), "{text}");
    // Sign and Enter before the whole summary was shown: not signed, the
    // next page is shown instead.
    assert!(!dialog.key(key("Left")));
    assert!(!dialog.key(key("Enter")));
    assert!(!dialog.summary_read());
    // A jump to the end skips rows: still not read.
    let mut jumped = dialog.clone();
    jumped.key(key("End"));
    let text = draw_on(&jumped, 80, 24);
    assert!(text.contains("max cost"), "{text}");
    assert!(!jumped.summary_read());
    assert!(!jumped.key(key("Enter")));
    // Scrolled through page by page: every field was shown, Sign works.
    let mut seen = String::new();
    for _ in 0..200 {
        seen.push_str(&draw_on(&dialog, 80, 24));
        if dialog.summary_read() {
            break;
        }
        dialog.key(key("PageDown"));
    }
    assert!(dialog.summary_read());
    for expected in ["value", "gas", "nonce        7", "max cost"] {
        assert!(seen.contains(expected), "{expected}");
    }
    assert_eq!(dialog.selected, 0);
    assert!(dialog.key(key("Enter")), "Sign confirmed once read");
}

#[test]
fn wide_characters_never_wrap_a_value_to_column_zero() {
    // Fullwidth characters take two columns: cut by characters, a piece
    // would overflow and the paragraph would wrap its tail to column 0,
    // where it reads as a field of its own. Every summary row starts with
    // a label or with the value column's indentation.
    for (wide, ascii) in [(31, 15), (20, 3), (40, 0), (10, 30)] {
        let call = format!(
            "f(s={}{} chain        Sepolia (11155111) x {})",
            "\u{ff21}".repeat(wide),
            "a".repeat(ascii),
            "\u{ff21}".repeat(wide),
        );
        let view = SummaryView::new(&summary_calling(1, None, &call), None);
        let labels: Vec<&str> = view.fields.iter().map(|(l, _)| *l).collect();
        let dialog = SignDialog::detached(Stage::Confirm, QUESTION, Some(view));
        for width in [100, 80, 61] {
            let text = draw_on(&dialog, width, 60);
            let rows = text
                .lines()
                .skip_while(|l| !l.contains("Sign and send transaction"))
                .skip(1)
                .take_while(|l| !l.contains("[y/N]"));
            for row in rows {
                let Some((_, inner)) = row.split_once("│ ") else {
                    continue;
                };
                let inner = inner.trim_end_matches(['│', ' ']);
                let indented = inner.chars().take(13).all(|c| c == ' ');
                assert!(
                    inner.is_empty()
                        || indented
                        || labels
                            .iter()
                            .any(|l| format!("{inner:<13}").starts_with(&format!("{l:<13}"))),
                    "{row:?}\n{text}"
                );
            }
        }
    }
}

#[test]
fn the_confirmation_ignores_keys_until_armed() {
    // Down and Enter typed for another widget as the dialog opens.
    let view = SummaryView::new(&summary(8453, None), None);
    let mut dialog = SignDialog::detached(Stage::Confirm, QUESTION, Some(view));
    draw(&dialog);
    dialog.opened_at(std::time::Instant::now());
    for chord in ["Down", "Tab", "Left", "Right", "Enter"] {
        assert!(!dialog.key(key(chord)), "{chord}");
    }
    assert_eq!(dialog.selected, 1, "Cancel is still selected");
    // Esc always works.
    assert!(dialog.clone().key(key("Esc")));
    // Armed: the choice is taken.
    dialog.opened_at(std::time::Instant::now() - crate::evm_sign::ARM);
    assert!(!dialog.key(key("Left")));
    assert!(dialog.key(key("Enter")));
}

// ---- the pane and the shared gate -----------------------------------------

const COUNTER: &str = "0x00000000000000000000000000000000C0C0C0C0";
const ANVIL0: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
const PASSWORD: &str = "rustman-test-password";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../rustman-evm-signing/tests/fixtures")
}

/// A workspace signing on `dev` (chain 31337 at `url`) with Anvil's first
/// account in a keystore whose password is prompted; `allow` sets
/// `[evm] allow_signing`.
fn workspace(url: &str, allow: bool) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    let write = |rel: &str, text: &str| {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    write(
        "rustman.toml",
        &format!(
            "[workspace]\nname = \"t\"\nversion = 1\n\n[defaults]\ndefault_environment = \"dev\"\n\n\
             [evm]\nallow_signing = {allow}\n[evm.signers.deployer]\nkeystore = \"keys/anvil0.json\"\n"
        ),
    );
    write(
        "environments/dev.yaml",
        &format!("chain_id: 31337\nvariables:\n  rpc: {url}\n"),
    );
    write(".gitignore", ".rustman/\nkeys/\n");
    let read = |rel: &str| fs::read_to_string(fixtures().join(rel)).unwrap();
    write("keys/anvil0.json", &read("keystores/anvil0.json"));
    write("Counter.abi.json", &read("counter/Counter.abi.json"));
    write(
        "add.rman.yaml",
        &format!(
            "version: 1\nkind: evm-tx\nname: add\nrpc: \"{{{{rpc}}}}\"\nto: \"{COUNTER}\"\n\
             abi: Counter.abi.json\nfunction: add\nargs: [\"2\"]\nsigner: deployer\n\
             assertions:\n  - evm: {{emits: \"Added(address,uint256,uint256)\"}}\n"
        ),
    );
    tmp
}

fn shell(tmp: &tempfile::TempDir, request: &str) -> State {
    let root = tmp.path().join("ws");
    let config = tmp.path().join("config");
    fs::create_dir_all(&config).unwrap();
    let (mut state, mut effects) = crate::initial_state_with(
        &root,
        Overrides::default(),
        Some(&config),
        ColorMode::NoColor,
    );
    while let Some(effect) = effects.pop() {
        effects.extend(update(&mut state, execute(effect)));
    }
    state.secret_backend = Some(SecretBackend::File);
    update(
        &mut state,
        Action::Resize {
            width: 120,
            height: 40,
        },
    );
    crate::workbench::open_path(&mut state, Path::new(request));
    state.shell.focus = Focus::EditorBody;
    state
}

fn sign_job() -> crate::send::SendJob {
    let tmp = workspace("http://127.0.0.1:9", true);
    let mut state = shell(&tmp, "add.rman.yaml");
    let mut effects = crate::evm_sign::start(&mut state, false);
    let Some(Effect::Send(job)) = effects.pop() else {
        panic!("expected a signing send");
    };
    *job
}

#[test]
fn the_sign_action_is_the_per_invocation_opt_in() {
    let tmp = workspace("http://127.0.0.1:9", true);
    let mut state = shell(&tmp, "add.rman.yaml");
    let notice = state.tabs.active().unwrap().editor.notice().unwrap();
    assert!(notice.contains("Alt+T (evm.sign)"), "{notice}");
    let effects = crate::evm_sign::start(&mut state, true);
    let [Effect::Send(job)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert_eq!(
        job.sign,
        Some(crate::evm_sign::SignRequest { force_revert: true })
    );
    let options = job.sign.unwrap().options();
    assert!(options.sign && options.force_revert && !options.yes_local);
    // Another send from the tab never cancels the signing one.
    assert!(crate::send::start(&mut state).is_empty());
    assert!(
        state
            .tabs
            .active()
            .unwrap()
            .in_flight
            .as_ref()
            .unwrap()
            .signing
    );
    // A request.send of an evm-tx is no opt-in: no sign request.
    let mut plain = shell(&tmp, "add.rman.yaml");
    let effects = crate::send::start(&mut plain);
    let [Effect::Send(job)] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert_eq!(job.sign, None);
}

#[test]
fn one_transaction_is_signed_at_a_time() {
    // Two gates at once would read the same pending nonce, and their
    // dialogs would replace each other.
    let tmp = workspace("http://127.0.0.1:9", true);
    fs::copy(
        tmp.path().join("ws/add.rman.yaml"),
        tmp.path().join("ws/again.rman.yaml"),
    )
    .unwrap();
    let mut state = shell(&tmp, "add.rman.yaml");
    assert_eq!(crate::evm_sign::start(&mut state, false).len(), 1);
    crate::workbench::open_path(&mut state, Path::new("again.rman.yaml"));
    assert!(state.tabs.active().unwrap().in_flight.is_none());
    assert!(crate::evm_sign::start(&mut state, false).is_empty());
    assert!(state.tabs.active().unwrap().in_flight.is_none());
    assert!(
        state
            .notifications
            .log()
            .any(|e| format!("{e:?}").contains(crate::evm_sign::BUSY_ELSEWHERE))
    );
    // A plain send from the other tab is not signing: it goes.
    assert_eq!(crate::send::start(&mut state).len(), 1);
}

#[test]
fn a_new_attempt_clears_the_previous_transaction() {
    let tmp = workspace("http://127.0.0.1:9", true);
    let mut state = shell(&tmp, "add.rman.yaml");
    let editor = state.tabs.active_mut().unwrap().editor.evm_mut().unwrap();
    editor.push_sign_line("sent 0xabc", false);
    editor.set_result(&serde_json::json!({
        "kind": "evm-tx", "status": "success", "chain": "Sepolia",
        "chain_id": 11155111, "hash": "0xabc", "nonce": 0, "block": 3,
        "gas_used": "43000", "explorer": null,
    }));
    assert_eq!(crate::evm_sign::start(&mut state, false).len(), 1);
    let editor = state.tabs.active().unwrap().editor.evm().unwrap();
    assert!(editor.result.is_none(), "the old audit view is gone");
    assert!(editor.sign_log.is_empty());
    let text = buffer_text(&render(&state, 240, 40));
    assert!(!text.contains("hash = 0xabc"), "{text}");
    // A refused second press keeps the running attempt's log.
    let editor = state.tabs.active_mut().unwrap().editor.evm_mut().unwrap();
    editor.push_sign_line("simulating", false);
    assert!(crate::evm_sign::start(&mut state, false).is_empty());
    let editor = state.tabs.active().unwrap().editor.evm().unwrap();
    assert_eq!(editor.sign_log.len(), 1);
}

#[test]
fn signing_applies_to_evm_tx_tabs_only() {
    let tmp = workspace("http://127.0.0.1:9", true);
    fs::write(
        tmp.path().join("ws/h.rman.yaml"),
        "version: 1\nkind: http\nname: H\nmethod: GET\nurl: http://127.0.0.1:9/\n",
    )
    .unwrap();
    let mut state = shell(&tmp, "h.rman.yaml");
    assert_eq!(state.tabs.active().unwrap().editor.kind(), "http");
    assert!(crate::evm_sign::start(&mut state, false).is_empty());
}

#[test]
fn the_gate_refuses_a_workspace_that_did_not_opt_in() {
    // The same refusal as `rustman evm send --sign`, before any node call.
    let tmp = workspace("http://127.0.0.1:9", false);
    let mut state = shell(&tmp, "add.rman.yaml");
    let mut effects = crate::evm_sign::start(&mut state, false);
    let Some(Effect::Send(job)) = effects.pop() else {
        panic!("expected a signing send");
    };
    let (tx, _rx) = unbounded_channel();
    let done = crate::send::execute_with(&job, Some(&tx));
    update(&mut state, done);
    let tab = state.tabs.active().unwrap();
    let record = &tab.exchange.as_ref().unwrap().record;
    assert_eq!(
        record.error.as_deref(),
        Some("signing is disabled for this workspace; set [evm] allow_signing = true")
    );
    assert!(tab.in_flight.is_none());
}

#[test]
fn a_sign_job_without_its_dialog_is_refused_before_any_key_is_unlocked() {
    // Executed without the interceptor's link (no dialog): the gate's
    // headless refusal on chain 31337 without --yes-local. Nothing listens
    // on port 9, so the refusal comes first or the node is unreachable:
    // either way nothing is signed.
    let job = sign_job();
    let Action::SendFinished { received, .. } = crate::send::execute(&job) else {
        panic!("expected a completion");
    };
    let error = received.record.error.unwrap_or_default();
    assert!(!error.is_empty());
    assert!(received.record.evm.is_none_or(|evm| evm["hash"].is_null()));
}

#[test]
fn an_audit_record_fills_the_result_tab() {
    let tmp = workspace("http://127.0.0.1:9", true);
    let mut state = shell(&tmp, "add.rman.yaml");
    let tab = state.tabs.active_mut().unwrap();
    let editor = tab.editor.evm_mut().unwrap();
    editor.push_sign_line("sent 0xabc", false);
    editor.push_sign_line("warning: SIMULATION REVERTED: Odd\u{202e}", true);
    editor.set_result(&serde_json::json!({
        "kind": "evm-tx", "status": "success", "chain": "Anvil/Hardhat local",
        "chain_id": 31337, "hash": "0xabc", "nonce": 0, "block": 3,
        "gas_used": "43000", "explorer": null,
    }));
    let text = buffer_text(&render(&state, 240, 40));
    assert!(
        text.contains("transaction success · Anvil/Hardhat local (31337)"),
        "{text}"
    );
    assert!(text.contains("hash = 0xabc"), "{text}");
    assert!(text.contains("sent 0xabc"), "{text}");
    assert!(text.contains("Odd\\u{202e}"), "{text}");
    assert_eq!(state.status.chain, None);
}

// ---- end to end against Anvil ---------------------------------------------

/// `anvil` on `PATH` when the Anvil tests are enabled.
fn anvil_program() -> Option<PathBuf> {
    let require = std::env::var("RUSTMAN_REQUIRE_ANVIL").ok();
    let opt_in = std::env::var("RUSTMAN_ANVIL").ok();
    if require.as_deref() != Some("1") && opt_in.as_deref() != Some("1") {
        eprintln!("set RUSTMAN_ANVIL=1 to run the Anvil tests: skipping");
        return None;
    }
    let file = if cfg!(windows) { "anvil.exe" } else { "anvil" };
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(file))
            .find(|p| p.is_file())
    });
    assert!(
        found.is_some() || require.as_deref() != Some("1"),
        "anvil is required (RUSTMAN_REQUIRE_ANVIL=1) but not on PATH"
    );
    found
}

/// Anvil with the Counter installed, and a way to call it.
struct Node {
    runtime: tokio::runtime::Runtime,
    anvil: Option<rustman_evm::fork::Anvil>,
}

impl Node {
    fn start(program: &Path) -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut options = rustman_evm::fork::AnvilOptions::new(program.to_owned());
        options.port = 0;
        let anvil = runtime
            .block_on(rustman_evm::fork::Anvil::start(&options))
            .unwrap();
        let node = Self {
            runtime,
            anvil: Some(anvil),
        };
        let code = fs::read_to_string(fixtures().join("counter/Counter.runtime.hex")).unwrap();
        node.call("anvil_setCode", serde_json::json!([COUNTER, code.trim()]));
        node
    }

    fn url(&self) -> String {
        self.anvil.as_ref().unwrap().url()
    }

    fn call(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let body =
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let url = self.url();
        self.runtime.block_on(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let address = url.trim_start_matches("http://").to_owned();
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            let body = body.to_string();
            let head = format!(
                "POST / HTTP/1.1\r\nhost: anvil\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).await.unwrap();
            stream.write_all(body.as_bytes()).await.unwrap();
            let mut out = String::new();
            stream.read_to_string(&mut out).await.unwrap();
            let json = &out[out.find("\r\n\r\n").unwrap() + 4..];
            let value: serde_json::Value = serde_json::from_str(json).unwrap();
            value["result"].clone()
        })
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        if let Some(anvil) = self.anvil.take() {
            self.runtime.block_on(anvil.stop());
        }
    }
}

/// What the user does once the gate showed `sent 0x…` (during the receipt
/// wait).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterSent {
    /// Nothing: the receipt comes.
    Wait,
    /// `Esc` in the pane cancels the send.
    Esc,
    /// Quit: the loop cancels every send, then waits for the pending
    /// signing jobs as `run_with` does; true once the audit entry was on
    /// disk when that wait returned.
    Quit,
}

/// Sign the open tab, answering each dialog with `answer`; returns the
/// stages asked.
fn sign(
    state: &mut State,
    answer: impl FnMut(&State, &mut SignDialog) -> Vec<Chord>,
) -> Vec<Stage> {
    sign_then(state, answer, AfterSent::Wait, |_| {}).0
}

/// [`sign`], then `after` once the transaction is out; `audited` is
/// called right after the quit's wait. The job runs as the effect runner
/// runs it: through [`crate::evm_sign::intercept`], on a blocking thread,
/// its prompts reaching the loop as actions. Each dialog is drawn before
/// it is answered, and the confirmation only after it is armed.
fn sign_then(
    state: &mut State,
    mut answer: impl FnMut(&State, &mut SignDialog) -> Vec<Chord>,
    after: AfterSent,
    mut audited: impl FnMut(&State),
) -> (Vec<Stage>, bool) {
    let mut effects = crate::evm_sign::start(state, false);
    let Some(effect @ Effect::Send(_)) = effects.pop() else {
        panic!("expected a signing send");
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (tx, mut rx) = unbounded_channel();
    {
        let _entered = runtime.enter();
        assert!(crate::evm_sign::intercept(effect, &tx).is_none());
    }
    drop(tx);
    let mut stages = Vec::new();
    let mut quit = false;
    loop {
        let action = rx.blocking_recv().expect("the job ends with a completion");
        let finished = matches!(action, Action::SendFinished { .. });
        update(state, action);
        if finished {
            break;
        }
        let sent = state
            .tabs
            .active()
            .and_then(|t| t.editor.evm())
            .is_some_and(|e| e.sign_log.iter().any(|(l, _)| l.starts_with("sent ")));
        let flying = state.tabs.active().is_some_and(|t| t.in_flight.is_some());
        if sent && flying {
            match after {
                AfterSent::Wait => {}
                AfterSent::Esc => {
                    crate::commands::shell_key(state, key("Esc"));
                }
                AfterSent::Quit => {
                    crate::workbench::cancel_all(state);
                    crate::evm_sign::wait_for_pending(|| {});
                    audited(state);
                    quit = true;
                }
            }
        }
        let Some(Overlay::Sign(open)) = &state.overlay else {
            continue;
        };
        let mut dialog = (**open).clone();
        stages.push(dialog.stage);
        render(state, 120, 40);
        if dialog.stage == Stage::Confirm {
            std::thread::sleep(crate::evm_sign::ARM);
        }
        for chord in answer(state, &mut dialog) {
            crate::commands::shell_key(state, chord);
        }
    }
    drop(runtime);
    (stages, quit)
}

fn typed(text: &str) -> Vec<Chord> {
    let mut chords: Vec<Chord> = text.chars().map(|c| Chord::plain(Code::Char(c))).collect();
    chords.push(key("Enter"));
    chords
}

/// The `evm` audit records of the history.
fn audits(root: &Path) -> Vec<serde_json::Value> {
    let dir = root.join(".rustman/history");
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .map(|e| {
            let text = fs::read_to_string(e.path()).unwrap();
            serde_json::from_str::<serde_json::Value>(&text).unwrap()
        })
        .filter(|entry| entry["record"]["evm"]["kind"] == "evm-tx")
        .collect()
}

#[test]
fn a_transaction_is_signed_in_the_tui_sent_mined_and_audited() {
    let Some(program) = anvil_program() else {
        return;
    };
    let node = Node::start(&program);
    let tmp = workspace(&node.url(), true);
    let root = tmp.path().join("ws");
    let mut state = shell(&tmp, "add.rman.yaml");

    // Esc at the confirmation: nothing is signed or sent.
    let stages = sign(&mut state, |_, dialog| match dialog.stage {
        Stage::Password => typed(PASSWORD),
        _ => vec![key("Esc")],
    });
    assert_eq!(stages, [Stage::Password, Stage::Confirm]);
    assert_eq!(
        node.call(
            "eth_getTransactionCount",
            serde_json::json!([ANVIL0, "latest"])
        ),
        "0x0"
    );
    assert!(audits(&root).is_empty());
    let record = &state
        .tabs
        .active()
        .unwrap()
        .exchange
        .as_ref()
        .unwrap()
        .record;
    assert_eq!(
        record.status,
        rustman_core::run::RequestStatus::Skipped,
        "{record:?}"
    );

    // Enter on the preselected Cancel declines.
    sign(&mut state, |_, dialog| match dialog.stage {
        Stage::Password => typed(PASSWORD),
        _ => vec![key("Enter")],
    });
    let record = &state
        .tabs
        .active()
        .unwrap()
        .exchange
        .as_ref()
        .unwrap()
        .record;
    assert_eq!(
        record.error.as_deref(),
        Some("cancelled; nothing was signed or sent")
    );
    assert_eq!(
        node.call(
            "eth_getTransactionCount",
            serde_json::json!([ANVIL0, "latest"])
        ),
        "0x0"
    );

    // Sign: the dialog shows the decoded summary first.
    let mut shown = String::new();
    let stages = sign(&mut state, |state, dialog| match dialog.stage {
        Stage::Password => typed(PASSWORD),
        _ => {
            shown = buffer_text(&render(state, 120, 40));
            assert_eq!(dialog.selected, 1, "Cancel is preselected");
            vec![key("Left"), key("Enter")]
        }
    });
    assert_eq!(stages, [Stage::Password, Stage::Confirm]);
    for expected in [
        "Anvil/Hardhat local (31337)",
        "(deployer)",
        "add(",
        "nonce        0",
        "max cost",
    ] {
        assert!(shown.contains(expected), "{expected}\n{shown}");
    }
    assert!(!shown.contains(PASSWORD));
    assert_eq!(
        node.call(
            "eth_getTransactionCount",
            serde_json::json!([ANVIL0, "latest"])
        ),
        "0x1"
    );
    let tab = state.tabs.active().unwrap();
    let record = &tab.exchange.as_ref().unwrap().record;
    assert_eq!(
        record.status,
        rustman_core::run::RequestStatus::Passed,
        "{record:?}"
    );
    let log: Vec<&str> = tab
        .editor
        .evm()
        .unwrap()
        .sign_log
        .iter()
        .map(|(l, _)| l.as_str())
        .collect();
    let hash = log
        .iter()
        .find_map(|l| l.strip_prefix("sent "))
        .unwrap_or_else(|| panic!("{log:?}"))
        .to_owned();
    assert!(
        log.iter().any(|l| l.contains("status     success")),
        "{log:?}"
    );
    let text = buffer_text(&render(&state, 120, 40));
    assert!(text.contains("transaction success"), "{text}");

    // History keeps the same audit entry as the CLI, once.
    let audits = audits(&root);
    assert_eq!(audits.len(), 1, "{audits:?}");
    let evm = &audits[0]["record"]["evm"];
    assert_eq!(evm["hash"], hash);
    assert_eq!(evm["signer"]["address"], ANVIL0);
    assert_eq!(evm["status"], "success");
    assert_eq!(audits[0]["replay"], serde_json::Value::Null);

    // Esc during the receipt wait (Anvil no longer mines): the transaction
    // is out, so its audit entry is kept, `pending`, and it is shown.
    node.call("evm_setAutomine", serde_json::json!([false]));
    sign_then(
        &mut state,
        |_, dialog| match dialog.stage {
            Stage::Password => typed(PASSWORD),
            _ => vec![key("Left"), key("Enter")],
        },
        AfterSent::Esc,
        |_| {},
    );
    let audits = self::audits(&root);
    assert_eq!(audits.len(), 2, "{audits:?}");
    let pending = audits
        .iter()
        .find(|a| a["record"]["evm"]["status"] == "pending");
    let pending = pending.unwrap_or_else(|| panic!("{audits:?}"));
    let record = &state
        .tabs
        .active()
        .unwrap()
        .exchange
        .as_ref()
        .unwrap()
        .record;
    assert_eq!(
        record.evm.as_ref().unwrap()["hash"],
        pending["record"]["evm"]["hash"]
    );
    let text = buffer_text(&render(&state, 240, 40));
    assert!(text.contains("transaction pending"), "{text}");

    // Quit during the receipt wait, through the effect runner's path: the
    // loop cancels the send and waits for the signing job, which has
    // written its `pending` audit entry by the time the wait returns.
    let mut on_disk = 0;
    let (_, quit) = sign_then(
        &mut state,
        |_, dialog| match dialog.stage {
            Stage::Password => typed(PASSWORD),
            _ => vec![key("Left"), key("Enter")],
        },
        AfterSent::Quit,
        |_| {
            on_disk = self::audits(&root)
                .iter()
                .filter(|a| a["record"]["evm"]["status"] == "pending")
                .count();
        },
    );
    assert!(quit, "the transaction was sent before the quit");
    assert_eq!(on_disk, 2, "the quit waited for the pending audit entry");

    for entry in fs::read_dir(root.join(".rustman/history"))
        .unwrap()
        .flatten()
    {
        let text = String::from_utf8_lossy(&fs::read(entry.path()).unwrap()).into_owned();
        assert!(!text.contains(PASSWORD), "{}", entry.path().display());
    }
}
