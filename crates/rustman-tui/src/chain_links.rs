//! Links from a GraphQL result into the EVM toolkit (blockchain-graphql
//! "Links into the EVM decoder", `graphql.links`, `L` in the response):
//! the result's addresses and transaction hashes are listed; choosing one
//! offers "Decode transaction", "Show balance", "Call contract" and "Open
//! in explorer" (the explorer of the environment's pinned chain). Decoding
//! and balances run off the render loop against the environment's `{{rpc}}`
//! with its chain pin checked, read-only, and their output is redacted.

use std::fmt;
use std::sync::Arc;

use ratatui::Frame;
use ratatui::text::Line;
use rustman_core::collection::RequestEntry;
use rustman_core::model::{OpaqueRequest, Request};
use rustman_core::secrets::SecretBackend;
use rustman_core::vars::{Resolver, resolve_evm_endpoint};
use rustman_core::workspace::RequestRef;
use rustman_core::{Redactor, Workspace};
use rustman_evm::chain::ChainRegistry;
use rustman_evm::exec::EvmRunner;
use rustman_evm::gql::{Link, LinkAction, LinkKind};
use rustman_evm::quantity::{Denomination, Form, Quantity};
use rustman_evm::settings::EvmSettings;
use rustman_evm::tx::{AbiMap, DecodeContext, TokenCache, fetch_tx};
use rustman_evm::{EvmError, Rpc};
use rustman_http::{HttpEngine, SendOptions};
use serde_json::{Map, Value, json};

use crate::app::{Action, Effect, Overlay, State};
use crate::config::UiOptions;
use crate::focus::Focus;
use crate::keymap::chord::{Chord, Code};
use crate::pane::Registry;
use crate::send::secret_store;
use crate::theme::Token;
use crate::toast::Severity;
use crate::view::{dialog, hint};
use crate::workbench;

/// Without `{{rpc}}` in the environment.
pub const NO_RPC: &str = "no rpc configured: set the environment's rpc variable";

/// The overlay: the links, then a link's actions, then a lookup's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainLinks {
    pub links: Vec<Link>,
    pub selected: usize,
    /// The chosen link's actions and the selected one.
    pub actions: Option<(Vec<LinkAction>, usize)>,
    /// The explorer of the environment's pinned chain.
    pub explorer: Option<String>,
    pub lookup: Option<Lookup>,
}

/// A lookup's title and its lines; `None` while it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup {
    pub title: String,
    pub result: Option<Result<Vec<String>, String>>,
}

/// `graphql.links`: list the current result's links.
pub fn open(state: &mut State) {
    let links = state
        .tabs
        .active()
        .and_then(|t| t.exchange.as_ref())
        .and_then(|r| r.graphql.as_ref())
        .map(|view| view.links.clone())
        .unwrap_or_default();
    if links.is_empty() {
        let message = "No addresses or transaction hashes in this result";
        state.notify(Severity::Info, message);
        return;
    }
    let explorer = explorer(state);
    state.overlay = Some(Overlay::ChainLinks(Box::new(ChainLinks {
        links,
        selected: 0,
        actions: None,
        explorer,
        lookup: None,
    })));
}

/// The explorer of the active environment's `chain_id`, from the known
/// chains and `[evm.chains]`.
fn explorer(state: &State) -> Option<String> {
    let ws = workbench::workspace(state)?;
    let name = state.status.environment.as_deref()?;
    let chain_id = ws.environments.iter().find(|e| e.name == name)?.chain_id?;
    let settings = EvmSettings::from_table(&ws.settings.evm).ok()?;
    let registry = ChainRegistry::with_overrides(&settings).ok()?;
    registry.get(chain_id).explorer
}

/// Keys while the overlay is open.
pub fn key(state: &mut State, chord: Chord) -> Vec<Effect> {
    let Some(Overlay::ChainLinks(view)) = &mut state.overlay else {
        return Vec::new();
    };
    if view.lookup.is_some() {
        if matches!(chord.code, Code::Esc | Code::Enter | Code::Char('q')) {
            view.lookup = None;
        }
        return Vec::new();
    }
    if let Some((actions, selected)) = &mut view.actions {
        let chosen = move_or_pick(chord, selected, actions.len());
        return match chosen {
            Pick::Close => {
                view.actions = None;
                Vec::new()
            }
            Pick::Chosen(i) => {
                let action = actions[i];
                let link = view.links[view.selected].clone();
                act(state, &link, action)
            }
            Pick::Stay => Vec::new(),
        };
    }
    match move_or_pick(chord, &mut view.selected, view.links.len()) {
        Pick::Close => state.overlay = None,
        Pick::Chosen(i) => {
            let actions = view.links[i].actions(view.explorer.is_some());
            view.actions = Some((actions, 0));
        }
        Pick::Stay => {}
    }
    Vec::new()
}

enum Pick {
    Stay,
    Close,
    Chosen(usize),
}

fn move_or_pick(chord: Chord, selected: &mut usize, len: usize) -> Pick {
    let last = len.saturating_sub(1);
    match chord.code {
        Code::Esc => return Pick::Close,
        Code::Up | Code::Char('k') => *selected = selected.saturating_sub(1),
        Code::Down | Code::Char('j') => *selected = (*selected + 1).min(last),
        Code::Enter if len > 0 => return Pick::Chosen(*selected),
        _ => {}
    }
    Pick::Stay
}

fn act(state: &mut State, link: &Link, action: LinkAction) -> Vec<Effect> {
    match action {
        LinkAction::DecodeTransaction => lookup(state, link, LookupKind::Transaction),
        LinkAction::ShowBalance => lookup(state, link, LookupKind::Balance),
        LinkAction::CallContract => {
            state.overlay = None;
            call_contract(state, &link.value);
            Vec::new()
        }
        LinkAction::OpenExplorer => {
            let Some(Overlay::ChainLinks(view)) = &state.overlay else {
                return Vec::new();
            };
            let url = view.explorer.as_deref().map(|base| link.explorer_url(base));
            state.overlay = None;
            url.map(|shown| {
                Effect::OpenUrl(crate::trace_link::LinkJob {
                    shown,
                    rebuild: None,
                })
            })
            .into_iter()
            .collect()
        }
    }
}

/// Start a read-only lookup of `link` and show it running.
fn lookup(state: &mut State, link: &Link, kind: LookupKind) -> Vec<Effect> {
    let job = LookupJob {
        kind,
        value: link.value.clone(),
        workspace: workbench::workspace(state).cloned().map(Box::new),
        environment: state.status.environment.clone(),
        backend: state.secret_backend,
    };
    if let Some(Overlay::ChainLinks(view)) = &mut state.overlay {
        view.actions = None;
        view.lookup = Some(Lookup {
            title: job.title(),
            result: None,
        });
    }
    vec![Effect::EvmLookup(Box::new(job))]
}

/// "Call contract": a new, unsaved `evm-call` tab to the address, with
/// the ERC-20 ABI to start from.
fn call_contract(state: &mut State, address: &str) {
    let mut fields = Map::new();
    fields.insert("to".into(), json!(address));
    fields.insert("abi".into(), json!("erc20"));
    fields.insert("function".into(), json!("totalSupply"));
    match rustman_evm::adhoc::call_entry("{{rpc}}", fields) {
        Ok(entry) => {
            let registry = Registry::of(state);
            state.tabs.open_unsaved(&entry.request, &registry);
            state.shell.focus = Focus::Url;
        }
        Err(e) => state.notify(Severity::Error, e.to_string()),
    }
}

/// A lookup finished: show it, unless the overlay was closed meanwhile.
pub fn on_lookup(state: &mut State, result: Result<Vec<String>, String>) {
    if let Some(Overlay::ChainLinks(view)) = &mut state.overlay
        && let Some(lookup) = &mut view.lookup
    {
        lookup.result = Some(result);
    }
}

pub fn render(frame: &mut Frame, view: &ChainLinks, ui: &UiOptions) {
    if let Some(lookup) = &view.lookup {
        return render_lookup(frame, lookup, ui);
    }
    let room = usize::from(frame.area().height.saturating_sub(6)).max(1);
    let mut lines = Vec::new();
    let (title, footer) = match &view.actions {
        Some((actions, selected)) => {
            let link = &view.links[view.selected];
            lines.push(hint(&link.value, ui));
            for (i, action) in actions.iter().enumerate() {
                lines.push(selectable(action.label().to_owned(), i == *selected, ui));
            }
            (" EVM toolkit ", "Enter choose · Esc back")
        }
        None => {
            let start = (view.selected + 1).saturating_sub(room);
            for (i, link) in view.links.iter().enumerate().skip(start).take(room) {
                let kind = match link.kind {
                    LinkKind::Transaction => "tx  ",
                    LinkKind::Address => "addr",
                };
                let text = format!("{kind} {}  {}", link.value, link.path);
                lines.push(selectable(text, i == view.selected, ui));
            }
            (" Result links ", "Enter actions · Esc close")
        }
    };
    lines.extend([Line::default(), hint(footer, ui)]);
    dialog(frame, frame.area(), title, lines, ui);
}

fn selectable(text: String, selected: bool, ui: &UiOptions) -> Line<'static> {
    let line = Line::styled(text, ui.theme.fg(Token::Text));
    if selected {
        line.style(ui.theme.selection())
    } else {
        line
    }
}

fn render_lookup(frame: &mut Frame, lookup: &Lookup, ui: &UiOptions) {
    let mut lines: Vec<Line<'static>> = match &lookup.result {
        None => vec![hint(&format!("Reading{}", ui.glyphs.ellipsis), ui)],
        Some(Ok(lines)) => lines
            .iter()
            .map(|l| Line::styled(l.clone(), ui.theme.fg(Token::Text)))
            .collect(),
        Some(Err(message)) => vec![Line::styled(message.clone(), ui.theme.fg(Token::Error))],
    };
    lines.extend([Line::default(), hint("Esc back", ui)]);
    dialog(
        frame,
        frame.area(),
        &format!(" {} ", lookup.title),
        lines,
        ui,
    );
}

/// What a lookup reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupKind {
    /// The transaction and its receipt, decoded (`rustman evm tx`).
    Transaction,
    /// The address's balance in the native currency.
    Balance,
}

/// A lookup, owned, to run on another thread.
#[derive(Clone)]
pub struct LookupJob {
    pub kind: LookupKind,
    /// The hash, or the checksummed address.
    pub value: String,
    pub workspace: Option<Box<Workspace>>,
    pub environment: Option<String>,
    pub backend: Option<SecretBackend>,
}

impl fmt::Debug for LookupJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LookupJob")
            .field("kind", &self.kind)
            .field("value", &self.value)
            .finish_non_exhaustive()
    }
}

impl PartialEq for LookupJob {
    fn eq(&self, other: &Self) -> bool {
        (self.kind, &self.value) == (other.kind, &other.value)
    }
}

impl Eq for LookupJob {}

impl LookupJob {
    /// `EVM transaction 0x5c50…2060` or `Balance of 0xd8dA…6045`.
    pub fn title(&self) -> String {
        let short = match (self.value.get(..6), self.value.get(self.value.len() - 4..)) {
            (Some(head), Some(tail)) => format!("{head}…{tail}"),
            _ => self.value.clone(),
        };
        match self.kind {
            LookupKind::Transaction => format!("EVM transaction {short}"),
            LookupKind::Balance => format!("Balance of {short}"),
        }
    }
}

/// Run `job`. Blocks: the effect runner calls it on a blocking thread.
pub fn execute(job: &LookupJob) -> Action {
    let result = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(run(job)),
        Err(e) => Err(format!("cannot start the async runtime: {e}")),
    };
    Action::EvmLookup(result)
}

async fn run(job: &LookupJob) -> Result<Vec<String>, String> {
    let ws = job
        .workspace
        .as_deref()
        .ok_or("Scratch session: open a workspace whose environment sets rpc")?;
    let env = ws
        .environments
        .iter()
        .find(|e| job.environment.as_deref() == Some(e.name.as_str()));
    let store = secret_store(&ws.root, job.backend)?;
    let entry = bare_entry();
    let bare = RequestRef {
        root: &ws.root,
        http: &ws.settings.http,
        entry: &entry,
        ancestors: Vec::new(),
    };
    let resolver = Resolver::for_request(&ws.settings, &bare, env, &store);
    if resolver
        .lookup(rustman_http::runner::CHAIN_RPC_VARIABLE)
        .is_none()
    {
        return Err(NO_RPC.to_owned());
    }
    let endpoint = resolve_evm_endpoint(&bare, &resolver, "{{rpc}}").map_err(|e| e.to_string())?;
    let redactor = Redactor::for_request(&endpoint);
    let options = SendOptions::for_request(&ws.settings, &ws.root, &endpoint)
        .map_err(|e| redactor.text(&e.to_string()).into_owned())?;
    let evm = EvmRunner::new(Arc::new(HttpEngine::new()));
    let rpc = evm.transport(endpoint, &options);
    let lines = read(job, &evm, &rpc, ws, env).await;
    let redact = |text: String| redactor.text(&text).into_owned();
    lines
        .map(|lines| lines.into_iter().map(redact).collect())
        .map_err(|e| redact(e.to_string()))
}

/// An entry with no headers or auth: the node gets none of a request's.
fn bare_entry() -> RequestEntry {
    RequestEntry {
        path: "evm-rpc".into(),
        request: Request::Opaque(OpaqueRequest {
            kind: "evm-rpc".to_owned(),
            name: "rpc".to_owned(),
            description: None,
            tags: None,
            docs: None,
            variables: None,
            source: String::new(),
        }),
    }
}

/// Identify the node (checking the environment's chain pin), then read.
async fn read(
    job: &LookupJob,
    evm: &EvmRunner,
    rpc: &rustman_evm::JsonrpcTransport,
    ws: &Workspace,
    env: Option<&rustman_core::Environment>,
) -> Result<Vec<String>, EvmError> {
    let connected = evm.connect(rpc, &ws.settings.evm, env).await?;
    let chain = connected.identity.chain;
    let mut lines = vec![format!("chain: {}", chain.label())];
    match job.kind {
        LookupKind::Transaction => {
            let (abis, tokens) = (AbiMap::new(), TokenCache::new());
            let ctx = DecodeContext {
                rpc,
                chain,
                abis: &abis,
                tokens: &tokens,
            };
            let report = fetch_tx(&ctx, &job.value).await?;
            lines.extend(report.render().lines().map(str::to_owned));
            lines.extend(report.warnings.iter().map(|w| format!("warning: {w}")));
        }
        LookupKind::Balance => {
            let value = rpc_call(rpc, "eth_getBalance", json!([job.value, "latest"])).await?;
            let text = value.as_str().unwrap_or_default();
            let quantity = Quantity::from_rpc(text).map_err(|e| EvmError::Rpc(e.to_string()))?;
            let native = Denomination::native(&chain);
            lines.push(format!("address: {}", job.value));
            lines.push(format!("balance: {}", quantity.show(Form::Native, &native)));
            lines.push(format!("wei: {}", quantity.show(Form::Wei, &native)));
        }
    }
    Ok(lines)
}

async fn rpc_call(rpc: &dyn Rpc, method: &str, params: Value) -> Result<Value, EvmError> {
    rustman_evm::rpc::call(rpc, method, params).await
}
