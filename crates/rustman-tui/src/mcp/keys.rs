//! Keys of the MCP inspector. With the pane open, the trust dialog and then
//! the oldest waiting approval take every key; otherwise keys move between
//! the server list, the tab's list and the form. `Ctrl+C` cancels the call
//! in flight, `Ctrl+S` saves the last call as a request file, `Ctrl+O`
//! sends arguments that broke the input schema anyway and `Ctrl+J` swaps
//! the form for the raw JSON editor.

use rustman_core::model::{McpRequest, REQUEST_FILE_SUFFIX};
use rustman_mcp::client::protocol::Tool;
use rustman_mcp::client::schema::form as schema_form;
use rustman_mcp::client::{McpOp, expand_template, template_variables};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::form::{Form, Row, Target};
use super::{Busy, Command, Conn, Focus, Job, LEVELS, Prompt, Tab, answered, save_path, set_conn};
use crate::app::{Effect, Overlay, State};
use crate::dialogs::{Input, Purpose};
use crate::keymap::chord::{Chord, Code};
use crate::toast::Severity;
use crate::workbench;

/// Keys while the pane is open; `None` leaves the key to the keymap.
pub fn key(state: &mut State, chord: Chord) -> Option<Vec<Effect>> {
    if !state.mcp.open {
        return None;
    }
    if state.mcp.trust.is_some() {
        return Some(trust_key(state, chord));
    }
    if !state.mcp.approvals.is_empty() {
        approval_key(state, chord);
        return Some(Vec::new());
    }
    if chord.ctrl {
        return ctrl_key(state, chord);
    }
    if chord.alt {
        return None;
    }
    match state.mcp.focus {
        Focus::Form if state.mcp.form.is_some() => Some(form_key(state, chord)),
        _ => nav_key(state, chord),
    }
}

/// `y` approves (and records) the command line, `n` or `Esc` does not.
fn trust_key(state: &mut State, chord: Chord) -> Vec<Effect> {
    match chord.code {
        Code::Char('y' | 'Y') => {
            let Some(prompt) = state.mcp.trust.take() else {
                return Vec::new();
            };
            let Some(root) = workbench::workspace(state).map(|ws| ws.root.clone()) else {
                return Vec::new();
            };
            vec![Effect::Mcp(Box::new(Job::Trust {
                root,
                trust_key: state.mcp.trust_key.clone(),
                pending: prompt.pending,
                server: prompt.server,
            }))]
        }
        Code::Char('n' | 'N') | Code::Esc => {
            if let Some(prompt) = state.mcp.trust.take() {
                set_conn(state, &prompt.server.name, Conn::Idle);
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn approval_key(state: &mut State, chord: Chord) {
    let Some(waiting) = state.mcp.approvals.front_mut() else {
        return;
    };
    let finished = if waiting.is_sampling() {
        sampling_key(waiting, chord)
    } else {
        elicitation_key(waiting, chord)
    };
    if finished {
        answered(state);
    }
}

/// Type the response; `Ctrl+S` sends it, `Ctrl+R` or `Esc` rejects.
fn sampling_key(waiting: &mut super::dialog::Waiting, chord: Chord) -> bool {
    match (chord.ctrl, chord.code) {
        (true, Code::Char('s')) => {
            waiting.respond();
            true
        }
        (true, Code::Char('r')) | (false, Code::Esc) => {
            waiting.refuse(false);
            true
        }
        (false, Code::Backspace) => {
            waiting.response.pop();
            false
        }
        (false, Code::Enter) => {
            waiting.response.push('\n');
            false
        }
        _ => {
            if let Some(c) = chord.typed_char() {
                waiting.response.push(c);
            }
            false
        }
    }
}

/// Fill the form; `Ctrl+S` accepts, `Ctrl+D` declines, `Esc` cancels.
fn elicitation_key(waiting: &mut super::dialog::Waiting, chord: Chord) -> bool {
    match (chord.ctrl, chord.code) {
        (true, Code::Char('s')) => match waiting.accept() {
            Ok(()) => true,
            Err(errors) => {
                if let Some(form) = &mut waiting.form {
                    form.errors = errors;
                }
                false
            }
        },
        (true, Code::Char('d')) => {
            waiting.refuse(false);
            true
        }
        (false, Code::Esc) => {
            waiting.refuse(true);
            true
        }
        _ => {
            if let Some(form) = &mut waiting.form {
                edit(form, chord);
            }
            false
        }
    }
}

/// Moving, typing and picking in a form.
fn edit(form: &mut Form, chord: Chord) {
    match chord.code {
        Code::Up => form.move_by(-1),
        Code::Down => form.move_by(1),
        Code::Left => form.cycle(-1),
        Code::Right => form.cycle(1),
        Code::Backspace => form.backspace(),
        _ => {
            if let Some(c) = chord.typed_char() {
                form.type_char(c);
            }
        }
    }
}

fn ctrl_key(state: &mut State, chord: Chord) -> Option<Vec<Effect>> {
    match chord.code {
        Code::Char('c') => cancel(state),
        Code::Char('s') => return Some(ask_save(state)),
        Code::Char('o') => resend_unvalidated(state),
        Code::Char('j') => {
            if let Some(form) = &mut state.mcp.form {
                form.toggle_raw();
            }
        }
        _ => return None,
    }
    Some(Vec::new())
}

/// `Ctrl+C`: abandon the call in flight (`notifications/cancelled`).
fn cancel(state: &mut State) {
    if let Some(busy) = state.mcp.session.as_ref().and_then(|s| s.busy.as_ref()) {
        busy.cancel.cancel();
    }
}

fn form_key(state: &mut State, chord: Chord) -> Vec<Effect> {
    match chord.code {
        Code::Esc => state.mcp.focus = Focus::List,
        Code::Tab => state.mcp.focus = Focus::Servers,
        Code::Enter => submit_form(state, true),
        _ => {
            if let Some(form) = &mut state.mcp.form {
                edit(form, chord);
            }
        }
    }
    Vec::new()
}

fn nav_key(state: &mut State, chord: Chord) -> Option<Vec<Effect>> {
    let effects = match chord.code {
        Code::Esc => {
            state.mcp.open = false;
            Vec::new()
        }
        Code::Tab => {
            state.mcp.focus = next_focus(state);
            Vec::new()
        }
        Code::Up | Code::Char('k') => move_by(state, -1),
        Code::Down | Code::Char('j') => move_by(state, 1),
        Code::Enter => activate(state),
        Code::Char(c @ '1'..='5') => {
            state.mcp.tab = Tab::ALL[c as usize - '1' as usize];
            state.mcp.item = 0;
            Vec::new()
        }
        Code::Char('?') => return None,
        Code::Char(c) => letter(state, c),
        _ => return None,
    };
    Some(effects)
}

fn next_focus(state: &State) -> Focus {
    match state.mcp.focus {
        Focus::Servers if state.mcp.session.is_some() => Focus::List,
        Focus::List if state.mcp.form.is_some() => Focus::Form,
        _ => Focus::Servers,
    }
}

fn move_by(state: &mut State, by: isize) -> Vec<Effect> {
    let items = super::view::item_count(&state.mcp);
    let pane = &mut state.mcp;
    let (index, count) = match pane.focus {
        Focus::Servers => (&mut pane.selected, pane.servers.len()),
        _ => (&mut pane.item, items),
    };
    *index = index.saturating_add_signed(by).min(count.saturating_sub(1));
    Vec::new()
}

fn activate(state: &mut State) -> Vec<Effect> {
    match state.mcp.focus {
        Focus::Servers => connect_selected(state),
        _ => {
            open_item(state);
            Vec::new()
        }
    }
}

/// Resolve the selected server and check its trust.
fn connect_selected(state: &mut State) -> Vec<Effect> {
    let Some(row) = state.mcp.servers.get(state.mcp.selected) else {
        return Vec::new();
    };
    let Some(root) = workbench::workspace(state).map(|ws| ws.root.clone()) else {
        return Vec::new();
    };
    let name = row.name.clone();
    set_conn(state, &name, Conn::Connecting);
    vec![Effect::Mcp(Box::new(Job::Prepare {
        root,
        name,
        environment: state.status.environment.clone(),
        backend: state.secret_backend,
        trust_key: state.mcp.trust_key.clone(),
    }))]
}

/// What `Enter` on a list item does.
enum Opened {
    Form(Form),
    Read(String),
    Nothing,
}

/// Open the selected tool's or prompt's form, or read a resource.
fn open_item(state: &mut State) {
    let opened = match &state.mcp.session {
        Some(live) => opened(&live.lists, state.mcp.tab, state.mcp.item),
        None => Opened::Nothing,
    };
    match opened {
        Opened::Form(form) => {
            state.mcp.form = Some(form);
            state.mcp.focus = Focus::Form;
        }
        Opened::Read(uri) => {
            let title = format!("resources/read {uri}");
            perform(state, title, McpOp::ResourcesRead { uri }, true);
        }
        Opened::Nothing => {}
    }
}

fn opened(lists: &super::Lists, tab: Tab, item: usize) -> Opened {
    let form = match tab {
        Tab::Tools => lists.tools.get(item).map(tool_form),
        Tab::Prompts => lists.prompts.get(item).map(|p| {
            let rows = p
                .arguments
                .iter()
                .flatten()
                .map(|a| Row::text(&a.name, a.required == Some(true), a.description.clone()))
                .collect();
            Form::with_rows(Target::Prompt(p.name.clone()), rows, None)
        }),
        Tab::Resources => {
            if let Some(resource) = lists.resources.get(item) {
                return Opened::Read(resource.uri.clone());
            }
            lists
                .templates
                .get(item - lists.resources.len())
                .map(|t| template_form(&t.uri_template))
        }
        Tab::Notifications | Tab::Stderr => None,
    };
    form.map_or(Opened::Nothing, Opened::Form)
}

/// The argument form of `tool`, from its `inputSchema`.
pub fn tool_form(tool: &Tool) -> Form {
    let schema = Value::Object((*tool.input_schema).clone());
    Form::of_schema(Target::Tool(tool.name.to_string()), &schema_form(&schema))
}

fn template_form(template: &str) -> Form {
    let rows = template_variables(template)
        .iter()
        .map(|name| Row::text(name, true, None))
        .collect();
    Form::with_rows(Target::Template(template.to_owned()), rows, None)
}

/// `Enter` in a form: call the tool, get the prompt or read the expanded
/// template. `validate: false` sends arguments that break the schema.
fn submit_form(state: &mut State, validate: bool) {
    let Some(form) = &mut state.mcp.form else {
        return;
    };
    let call = match &form.target {
        Target::Tool(name) => form.value().map(|arguments| McpOp::ToolsCall {
            name: name.clone(),
            arguments,
        }),
        Target::Prompt(name) => Ok(McpOp::PromptsGet {
            name: name.clone(),
            arguments: form.strings(),
        }),
        Target::Template(template) => expand_template(template, &form.strings())
            .map(|uri| McpOp::ResourcesRead { uri })
            .map_err(|e| vec![e.to_string()]),
        Target::Elicitation => return,
    };
    match call {
        Ok(op) => {
            form.errors.clear();
            let title = title(&op);
            perform(state, title, op, validate);
        }
        Err(errors) => form.errors = errors,
    }
}

fn title(op: &McpOp) -> String {
    match op {
        McpOp::ToolsCall { name, .. } => format!("tools/call {name}"),
        McpOp::PromptsGet { name, .. } => format!("prompts/get {name}"),
        McpOp::ResourcesRead { uri } => format!("resources/read {uri}"),
        other => other.operation().to_string(),
    }
}

/// Send `op` to the session, tracking it as the call in flight.
fn perform(state: &mut State, title: String, op: McpOp, validate: bool) {
    let Some(live) = &mut state.mcp.session else {
        return;
    };
    if live.busy.is_some() {
        state.notify(Severity::Info, "A call is in flight (Ctrl+C cancels it)");
        return;
    }
    let cancel = CancellationToken::new();
    live.busy = Some(Busy {
        title: title.clone(),
        progress: None,
        cancel: cancel.clone(),
    });
    state.mcp.attempted = Some(op.clone());
    let command = Command::Perform {
        title,
        op,
        validate,
        cancel,
    };
    if live.commands.send(command).is_err() {
        live.busy = None;
    }
}

/// `Ctrl+O`: the user confirmed sending arguments that break the schema.
fn resend_unvalidated(state: &mut State) {
    let overridable = state.mcp.result.as_ref().is_some_and(|r| r.overridable);
    if let (true, Some(op)) = (overridable, state.mcp.attempted.clone()) {
        let title = format!("{} (not validated)", title(&op));
        perform(state, title, op, false);
    }
}

fn letter(state: &mut State, c: char) -> Vec<Effect> {
    match (c, state.mcp.focus, state.mcp.tab) {
        ('d', Focus::Servers, _) => disconnect(state),
        ('l', _, _) => cycle_level(state),
        ('s', Focus::List, Tab::Resources) => subscribe(state),
        ('r', _, Tab::Notifications) => reread(state),
        _ => {}
    }
    Vec::new()
}

fn disconnect(state: &mut State) {
    if let Some(live) = state.mcp.session.take() {
        let _ = live.commands.send(Command::Disconnect);
        set_conn(state, &live.server, Conn::Idle);
        state.mcp.form = None;
        state.mcp.focus = Focus::Servers;
    }
}

/// `l`: the next server log level (`logging/setLevel`).
fn cycle_level(state: &mut State) {
    let Some(live) = &mut state.mcp.session else {
        return;
    };
    if let Err(disabled) = live
        .info
        .capabilities
        .check(rustman_mcp::client::Operation::SetLogLevel)
    {
        let reason = disabled.reason;
        state.notify(Severity::Info, reason);
        return;
    }
    let next = live.level.map_or(0, |l| (l + 1) % LEVELS.len());
    live.level = Some(next);
    if let Some(level) = rustman_mcp::client::notify::parse_level(LEVELS[next]) {
        let _ = live.commands.send(Command::SetLevel(level));
    }
}

fn subscribe(state: &mut State) {
    let Some(live) = &state.mcp.session else {
        return;
    };
    if let Some(resource) = live.lists.resources.get(state.mcp.item) {
        let _ = live.commands.send(Command::Subscribe(resource.uri.clone()));
    }
}

/// `r`: read the resource the last update offered again.
fn reread(state: &mut State) {
    let Some(uri) = state.mcp.session.as_ref().and_then(|s| s.reread.clone()) else {
        return;
    };
    let title = format!("resources/read {uri}");
    perform(state, title, McpOp::ResourcesRead { uri }, true);
}

/// `Ctrl+S`: ask for the file to save the last call in.
fn ask_save(state: &mut State) -> Vec<Effect> {
    let Some((_, op)) = &state.mcp.last_call else {
        state.notify(Severity::Info, "Make a call first, then save it");
        return Vec::new();
    };
    if workbench::workspace(state).is_none() {
        state.notify(Severity::Info, "Saving a request needs a workspace");
        return Vec::new();
    }
    let stem = match op {
        McpOp::ToolsCall { name, .. } | McpOp::PromptsGet { name, .. } => name.clone(),
        other => other.operation().as_str().replace('/', "-"),
    };
    let input = Input::new(
        " Save the call as a request ",
        "File name:",
        Purpose::Mcp(Prompt::SaveCall),
        &format!("{stem}{REQUEST_FILE_SUFFIX}"),
    );
    state.overlay = Some(Overlay::Input(input));
    Vec::new()
}

/// A pane prompt was submitted with `text`.
pub fn submit(state: &mut State, prompt: Prompt, text: &str) -> Vec<Effect> {
    state.overlay = None;
    let Prompt::SaveCall = prompt;
    let Some((server, op)) = state.mcp.last_call.clone() else {
        return Vec::new();
    };
    let Some(path) = save_path(state, text) else {
        state.notify(Severity::Error, "Type a file name without `..` or a drive.");
        return Vec::new();
    };
    let name = text
        .trim()
        .trim_end_matches(REQUEST_FILE_SUFFIX)
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_owned();
    let request = McpRequest::new(&name, &server, op.operation(), op.params());
    vec![Effect::Mcp(Box::new(Job::Save {
        path,
        request: Box::new(request),
    }))]
}
