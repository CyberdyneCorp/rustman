//! The `evm-call` and `evm-tx` pane (add-evm-toolkit "TUI pane for EVM
//! kinds", task 4.3): a function picker from the ABI (only `view` and
//! `pure` functions unless all are shown, as read-only `eth_call`
//! simulations), typed argument fields, a block tag selector, and the
//! decoded result with unit toggles on quantities, the revert, the call
//! trace and the event timeline. An `evm-tx` opens read-only: signing is
//! not available.
//!
//! Edits patch the file's text (top-level `function`, `args` and `block`
//! only), so comments and every other field are kept as written.

use std::path::Path;
use std::sync::Arc;

use rustman_core::model::{AbiSpec, EvmRequest, Request};
use rustman_core::text::printable;
use rustman_evm::abi::{BUILT_IN, built_in};
use rustman_evm::json_abi::{Function, JsonAbi, StateMutability};
use rustman_evm::quantity::{Form, Quantity, forms, is_native_field};
use rustman_evm::{Denomination, abi::AbiCache};
use serde_json::Value;

use crate::focus::Focus;
use crate::keymap::Chord;
use crate::keymap::chord::Code;
use crate::pane::{PaneEditor, RenderCtx, RequestPane};
use crate::vim::Vim;

/// The block selector's choices, `latest` first.
pub const BLOCK_CHOICES: [&str; 5] = ["latest", "finalized", "safe", "pending", "earliest"];

/// The editor tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvmTab {
    Function,
    Args,
    Block,
    Result,
    Trace,
    Events,
}

impl EvmTab {
    pub const ALL: [Self; 6] = [
        Self::Function,
        Self::Args,
        Self::Block,
        Self::Result,
        Self::Trace,
        Self::Events,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Function => "Function",
            Self::Args => "Args",
            Self::Block => "Block",
            Self::Result => "Result",
            Self::Trace => "Trace",
            Self::Events => "Events",
        }
    }
}

/// One typed argument field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgField {
    pub name: String,
    /// The Solidity type: `address`, `uint256`, ...
    pub ty: String,
    pub value: String,
}

/// One decoded output, with the forms its quantity toggles through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputRow {
    pub name: String,
    /// The value as decoded (decimal for integers).
    pub value: String,
}

/// The last result handed to the pane (already redacted by the runner).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvmResultView {
    /// `Ethereum Mainnet (1) · geth 1.14.11`.
    pub status: Option<String>,
    pub outputs: Vec<OutputRow>,
    /// `reverted: Panic(0x12) division or modulo by zero`.
    pub revert: Option<String>,
    pub gas_estimate: Option<String>,
    /// The token symbol and decimals of an ERC-20 amount output.
    pub token: Option<(String, u8)>,
    /// The call tree, one line per frame, or why there is none.
    pub trace: Vec<String>,
    /// Why the call errored (chain mismatch, transport...).
    pub error: Option<String>,
}

impl EvmResultView {
    /// From the runner's `evm` record.
    pub fn from_record(evm: &Value) -> Self {
        let result = &evm["result"];
        let outputs = result["outputs"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(name, value)| OutputRow {
                name: name.clone(),
                value: value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned),
            })
            .collect();
        let trace = match (evm["trace"]["tree"].as_str(), evm["trace"]["note"].as_str()) {
            (Some(tree), _) => tree.lines().map(str::to_owned).collect(),
            (None, Some(note)) => vec![note.to_owned()],
            _ => Vec::new(),
        };
        let token = match (
            result["token"]["symbol"].as_str(),
            result["token"]["decimals"].as_u64(),
        ) {
            (Some(symbol), Some(decimals)) => {
                u8::try_from(decimals).ok().map(|d| (symbol.to_owned(), d))
            }
            _ => None,
        };
        Self {
            status: evm["status"].as_str().map(str::to_owned),
            outputs,
            revert: result["reverted"]["message"].as_str().map(str::to_owned),
            gas_estimate: result["gas_estimate"].as_str().map(str::to_owned),
            token,
            trace,
            error: evm["error"]["message"].as_str().map(str::to_owned),
        }
    }
}

/// The editing state of an `evm-call` or `evm-tx` file.
#[derive(Debug, Clone)]
pub struct EvmEditor {
    request: EvmRequest,
    /// The file text as edited.
    source: String,
    pub tab: EvmTab,
    abi: Option<Arc<JsonAbi>>,
    /// Why there is no ABI to pick from yet.
    pub abi_note: Option<String>,
    /// Show every function, not only `view` and `pure` ones.
    pub show_all: bool,
    /// The highlighted row of the picker.
    pub cursor: usize,
    /// The chosen function's signature.
    pub function: Option<String>,
    pub args: Vec<ArgField>,
    /// The argument field being edited.
    pub arg: usize,
    pub block: String,
    /// The display form of quantities in the Result tab.
    pub form: usize,
    pub result: Option<EvmResultView>,
    /// The live event timeline: one decoded line per event.
    pub events: Vec<String>,
    pub native: Denomination,
}

/// The most lines the event timeline keeps.
pub const MAX_EVENTS: usize = 1000;

impl EvmEditor {
    pub fn new(request: &EvmRequest) -> Self {
        let common = request.common();
        let mut editor = Self {
            request: request.clone(),
            source: common.source.to_owned(),
            tab: EvmTab::Function,
            abi: None,
            abi_note: None,
            show_all: false,
            cursor: 0,
            function: common.function.map(|f| f.get().clone()),
            args: Vec::new(),
            arg: 0,
            block: block_of(request),
            form: 0,
            result: None,
            events: Vec::new(),
            native: Denomination::default(),
        };
        editor.abi_note = match common.abi.map(|a| a.get()) {
            Some(AbiSpec::Source(name)) if BUILT_IN.contains(&name.as_str()) => {
                editor.abi = built_in(name);
                None
            }
            Some(AbiSpec::Source(path)) => Some(format!(
                "ABI file {path}: open from a workspace to pick a function"
            )),
            Some(AbiSpec::Explorer) => {
                Some("explorer ABI: fetched when the request is sent".to_owned())
            }
            None => Some("no ABI".to_owned()),
        };
        editor.fill_args(common.args);
        editor
    }

    /// Load a file ABI relative to the request file `file` under `root`.
    pub fn load_abi(&mut self, root: &Path, file: &Path) {
        let Some(AbiSpec::Source(source)) = self.request.common().abi.map(|a| a.get().clone())
        else {
            return;
        };
        match AbiCache::new().load(root, file, &source) {
            Ok(loaded) => {
                self.abi = Some(loaded.abi);
                self.abi_note = None;
                let args: Vec<Value> = self
                    .args
                    .iter()
                    .map(|a| Value::String(a.value.clone()))
                    .collect();
                self.fill_args(&args);
            }
            Err(error) => self.abi_note = Some(error.to_string()),
        }
    }

    pub fn is_tx(&self) -> bool {
        matches!(self.request, EvmRequest::Tx(_))
    }

    /// The functions the picker lists: `view` and `pure` ones unless all
    /// are shown, sorted by signature.
    pub fn functions(&self) -> Vec<&Function> {
        let Some(abi) = &self.abi else {
            return Vec::new();
        };
        let mut all: Vec<&Function> = abi
            .functions()
            .filter(|f| self.show_all || is_read(f))
            .collect();
        all.sort_by_key(|f| f.signature());
        all
    }

    fn chosen(&self) -> Option<&Function> {
        let name = self.function.as_deref()?;
        let abi = self.abi.as_ref()?;
        rustman_evm::abi::resolve_function(abi, name).ok()
    }

    /// One field per input of the chosen function, keeping `values`.
    fn fill_args(&mut self, values: &[Value]) {
        let text = |v: &Value| v.as_str().map_or_else(|| v.to_string(), str::to_owned);
        self.args = match self.chosen() {
            Some(function) => function
                .inputs
                .iter()
                .enumerate()
                .map(|(i, p)| ArgField {
                    name: if p.name.is_empty() {
                        format!("arg{i}")
                    } else {
                        p.name.clone()
                    },
                    ty: p.ty.clone(),
                    value: values.get(i).map(text).unwrap_or_default(),
                })
                .collect(),
            None => values
                .iter()
                .enumerate()
                .map(|(i, v)| ArgField {
                    name: format!("arg{i}"),
                    ty: String::new(),
                    value: text(v),
                })
                .collect(),
        };
        self.arg = 0;
    }

    /// Choose the highlighted function: its signature when its name is
    /// overloaded, else its name.
    fn pick(&mut self) {
        let Some(function) = self.functions().get(self.cursor).map(|f| (*f).clone()) else {
            return;
        };
        let overloaded = self
            .abi
            .as_ref()
            .and_then(|abi| abi.function(&function.name))
            .is_some_and(|all| all.len() > 1);
        self.function = Some(if overloaded {
            function.signature()
        } else {
            function.name.clone()
        });
        self.fill_args(&[]);
        self.sync();
        self.tab = EvmTab::Args;
    }

    /// Show the runner's `evm` record, and add the events its call emits
    /// (simulated when the request reads events) to the timeline.
    pub fn set_result(&mut self, evm: &Value) {
        self.result = Some(EvmResultView::from_record(evm));
        self.tab = EvmTab::Result;
        let lines = evm.get("events").and_then(Value::as_array);
        for line in lines.into_iter().flatten().filter_map(Value::as_str) {
            self.push_event(line);
        }
    }

    /// Add a decoded event line to the timeline, dropping the oldest past
    /// [`MAX_EVENTS`] so memory stays bounded.
    pub fn push_event(&mut self, line: impl Into<String>) {
        if self.events.len() >= MAX_EVENTS {
            self.events.drain(..=self.events.len() - MAX_EVENTS);
        }
        self.events.push(printable(&line.into()).into_owned());
    }

    /// The form quantities are shown in: native units for `value`- and
    /// `balance`-like outputs.
    pub fn shown(&self, row: &OutputRow) -> String {
        let Ok(quantity) = Quantity::parse(&row.value, &self.native) else {
            return row.value.clone();
        };
        if !row.value.bytes().all(|b| b.is_ascii_digit()) {
            return row.value.clone();
        }
        if let Some((symbol, decimals)) = self.result.as_ref().and_then(|r| r.token.clone()) {
            let token = Denomination { symbol, decimals };
            let all = [Form::Native, Form::Decimal, Form::Hex];
            return quantity.show(all[self.form % all.len()], &token);
        }
        let all = forms(is_native_field(&row.name));
        quantity.show(all[self.form % all.len()], &self.native)
    }

    /// The text as edited, with `function`, `args` and `block` written in.
    fn sync(&mut self) {
        let mut text = self.source.clone();
        if let Some(function) = &self.function {
            text = set_key(&text, "function", &yaml_string(function));
        }
        let args: Vec<String> = self.args.iter().map(|a| yaml_string(&a.value)).collect();
        text = set_key(&text, "args", &format!("[{}]", args.join(", ")));
        if self.block != "latest" || text.lines().any(|l| l.starts_with("block:")) {
            text = set_key(&text, "block", &self.block);
        }
        self.source = text;
    }

    fn edit_arg(&mut self, chord: Chord) -> bool {
        let Some(field) = self.args.get_mut(self.arg) else {
            return false;
        };
        match chord.code {
            Code::Char(c) => field.value.push(c),
            Code::Backspace => {
                field.value.pop();
            }
            Code::Up => self.arg = self.arg.saturating_sub(1),
            Code::Down | Code::Enter => self.arg = (self.arg + 1).min(self.args.len() - 1),
            _ => return false,
        }
        self.sync();
        true
    }

    fn picker_key(&mut self, code: Code) -> bool {
        let count = self.functions().len();
        match code {
            Code::Up | Code::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            Code::Down | Code::Char('j') => {
                self.cursor = (self.cursor + 1).min(count.saturating_sub(1))
            }
            Code::Char('a') => {
                self.show_all = !self.show_all;
                self.cursor = 0;
            }
            Code::Enter => self.pick(),
            _ => return false,
        }
        true
    }

    fn block_key(&mut self, code: Code) -> bool {
        let at = BLOCK_CHOICES.iter().position(|b| *b == self.block);
        let next = match (code, at) {
            (Code::Right | Code::Char('l'), Some(i)) => (i + 1) % BLOCK_CHOICES.len(),
            (Code::Left | Code::Char('h'), Some(i)) => {
                (i + BLOCK_CHOICES.len() - 1) % BLOCK_CHOICES.len()
            }
            (Code::Right | Code::Left | Code::Char('l' | 'h'), None) => 0,
            _ => return false,
        };
        self.block = BLOCK_CHOICES[next].to_owned();
        self.sync();
        true
    }
}

fn is_read(function: &Function) -> bool {
    matches!(
        function.state_mutability,
        StateMutability::View | StateMutability::Pure
    )
}

fn block_of(request: &EvmRequest) -> String {
    match request {
        EvmRequest::Call(call) => call
            .block
            .as_ref()
            .map_or_else(|| "latest".to_owned(), |b| b.as_str().to_owned()),
        EvmRequest::Tx(_) => "latest".to_owned(),
    }
}

/// `text` as a double-quoted YAML scalar.
fn yaml_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_default()
}

/// `text` with the top-level `key` (and its indented or list lines) set
/// to the single line `key: value`, appended when absent.
pub fn set_key(text: &str, key: &str, value: &str) -> String {
    let prefix = format!("{key}:");
    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;
    let mut skipping = false;
    for line in text.lines() {
        if skipping && (line.starts_with(' ') || line.starts_with('-') || line.is_empty()) {
            continue;
        }
        skipping = false;
        if line.starts_with(&prefix) && !replaced {
            out.push(format!("{key}: {value}"));
            replaced = true;
            skipping = true;
            continue;
        }
        out.push(line.to_owned());
    }
    if !replaced {
        out.push(format!("{key}: {value}"));
    }
    out.join("\n") + "\n"
}

impl PaneEditor for EvmEditor {
    fn kind(&self) -> &str {
        self.request.kind()
    }

    fn tabs(&self) -> Vec<&'static str> {
        EvmTab::ALL.iter().map(|t| t.label()).collect()
    }

    fn tab(&self) -> usize {
        EvmTab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0)
    }

    fn set_tab(&mut self, index: usize) {
        if let Some(tab) = EvmTab::ALL.get(index) {
            self.tab = *tab;
        }
    }

    fn method(&self) -> Option<&str> {
        None
    }

    fn url(&self) -> Option<&crate::textarea::TextArea> {
        None
    }

    fn key(&mut self, focus: Focus, chord: Chord, _vim: Option<&mut Vim>) -> bool {
        if focus != Focus::EditorBody || chord.ctrl || chord.alt || self.is_tx() {
            return false;
        }
        match self.tab {
            EvmTab::Function => self.picker_key(chord.code),
            EvmTab::Args => self.edit_arg(chord),
            EvmTab::Block => self.block_key(chord.code),
            EvmTab::Result if chord.code == Code::Char('u') => self.cycle_mode(),
            _ => false,
        }
    }

    fn cycle_mode(&mut self) -> bool {
        match self.tab {
            EvmTab::Function => {
                self.show_all = !self.show_all;
                self.cursor = 0;
            }
            EvmTab::Result => self.form += 1,
            _ => return false,
        }
        true
    }

    fn request(&self) -> Result<Request, String> {
        let file = Path::new("request.rman.yaml");
        EvmRequest::parse(file, &self.source)
            .map(|r| Request::Evm(Box::new(r)))
            .map_err(|e| e.message)
    }

    fn render(&self, frame: &mut ratatui::Frame, rect: ratatui::layout::Rect, ctx: &RenderCtx) {
        crate::evm_view::content(frame, rect, self, ctx);
    }

    fn notice(&self) -> Option<String> {
        if self.is_tx() {
            return Some(rustman_core::model::SIGNING_UNAVAILABLE.to_owned());
        }
        self.abi_note.clone()
    }

    fn variable_at_cursor(&self, _focus: Focus) -> Option<String> {
        let value = &self.args.get(self.arg)?.value;
        let start = value.rfind("{{")?;
        let end = value[start..].find("}}")?;
        Some(value[start + 2..start + end].trim().to_owned())
    }

    fn evm(&self) -> Option<&EvmEditor> {
        Some(self)
    }

    fn evm_mut(&mut self) -> Option<&mut EvmEditor> {
        Some(self)
    }
}

/// The pane of `evm-call` and `evm-tx` files.
pub struct EvmPane {
    kind: &'static str,
}

impl EvmPane {
    pub const CALL: Self = Self { kind: "evm-call" };
    pub const TX: Self = Self { kind: "evm-tx" };
}

impl RequestPane for EvmPane {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn open(&self, request: &Request) -> Option<Box<dyn PaneEditor>> {
        match request {
            Request::Evm(evm) => Some(Box::new(EvmEditor::new(evm))),
            _ => None,
        }
    }
}
