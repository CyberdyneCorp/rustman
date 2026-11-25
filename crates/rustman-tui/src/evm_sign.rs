//! Signing an `evm-tx` from its pane (evm-signing "Signing disabled by
//! default", the TUI opt-in; issue #112).
//!
//! `evm.sign` (`Alt+T`) is the TUI's per-invocation opt-in, the
//! counterpart of `--sign`; the workspace must still set
//! `[evm] allow_signing = true`. The request goes through the very gate
//! `rustman evm send --sign` uses ([`rustman_evm_signing::Signing`]): the
//! signer selection (keystore and keychain signers, raw keys refused), the
//! chain pin and match, the simulation (`evm.sign_force_revert` is the
//! `--force-revert` equivalent), fees, nonces, the confirmation, the
//! receipt wait and the audit record. This module only adds a
//! [`Console`] that asks through a modal dialog:
//!
//! - the keystore password, with no echo;
//! - the decoded summary, every value escaped on one line, with
//!   `Cancel` preselected, a `SIMULATION REVERTED` banner for a forced
//!   transaction and the max-cost warning;
//! - the chain name, typed, on chain 1 and `[evm] protected_chains`.
//!
//! `Esc` at any of them interrupts the gate: nothing is signed or sent.
//! The TUI is always a terminal, so `--yes-local` has no equivalent and
//! every transaction is confirmed in the dialog.
//!
//! The gate runs on a blocking thread of its own (like every send); its
//! prompts reach the loop as [`Action::Sign`] messages and the answers go
//! back on a channel, so the UI never blocks. `Esc` in the pane cancels
//! it: before the signature nothing is sent, during the receipt wait the
//! audit entry is kept with status `pending`. Quitting cancels it the same
//! way and waits for the audit entry before the process exits
//! ([`wait_for_pending`]).

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Padding, Paragraph, Wrap};
use rustman_core::run::RequestRecord;
use rustman_core::secrets::{BACKEND_VAR, Keychain, SecretBackend, keychain as os};
use rustman_core::text::printable_line;
use rustman_evm_signing::send::Wait;
use rustman_evm_signing::{Console, SignOptions, Summary, Zeroizing};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::app::{Abort, Action, Effect, Overlay, State};
use crate::config::UiOptions;
use crate::keymap::chord::{Chord, Code};
use crate::send::SendJob;
use crate::tabs::TabId;
use crate::theme::Token;
use crate::toast::Severity;
use crate::view::{block, hint};

/// The environment variable the signer id falls back to, as on the CLI.
pub const SIGNER_VAR: &str = "RUSTMAN_EVM_SIGNER";

/// Why another send from a tab that is signing is refused.
pub const BUSY: &str =
    "A transaction is being signed in this tab; Esc cancels it before it is sent";

/// The pane's notice on an `evm-tx` file.
pub const TX_NOTICE: &str = "evm-tx: Alt+T (evm.sign) signs and sends it after your confirmation; [evm] allow_signing = true is required";

/// Time between two receipt polls (as `rustman evm send`).
const POLL: Duration = Duration::from_millis(250);

/// How often a prompt looks at the cancellation while it waits.
const PROMPT_TICK: Duration = Duration::from_millis(50);

/// What `evm.sign` asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignRequest {
    /// `evm.sign_force_revert`: go on when the simulation reverts; the
    /// dialog carries the `SIMULATION REVERTED` banner.
    pub force_revert: bool,
}

impl SignRequest {
    /// The gate's options: always `--sign`, never `--yes-local` (the TUI
    /// confirms every transaction), the signer from `RUSTMAN_EVM_SIGNER`
    /// or the request's `signer`.
    pub fn options(self) -> SignOptions {
        SignOptions {
            sign: true,
            signer: std::env::var(SIGNER_VAR).ok(),
            force_revert: self.force_revert,
            yes_local: false,
            wait: Wait {
                poll: POLL,
                ..Wait::default()
            },
        }
    }
}

/// The OS keychain of keychain signers (`RUSTMAN_SECRET_STORE=file`, or
/// the session's file backend, means none: signer keys never come from a
/// file).
pub fn keychain(backend: Option<SecretBackend>) -> Option<Box<dyn Keychain>> {
    let backend = match backend {
        Some(backend) => backend,
        None => SecretBackend::from_env_value(std::env::var(BACKEND_VAR).ok().as_deref()).ok()?,
    };
    match backend {
        SecretBackend::Keychain => os::os_keychain(),
        SecretBackend::File => None,
    }
}

/// `evm.sign` (`force_revert: false`) and `evm.sign_force_revert`: take
/// the current `evm-tx` tab through the signing gate.
pub fn start(state: &mut State, force_revert: bool) -> Vec<Effect> {
    let Some(tab) = state.tabs.active_mut() else {
        state.notify(Severity::Info, "Open an evm-tx request to sign it");
        return Vec::new();
    };
    if !tab.editor.evm().is_some_and(|e| e.is_tx()) {
        state.notify(Severity::Info, "Signing applies to evm-tx requests");
        return Vec::new();
    }
    if let Some(editor) = tab.editor.evm_mut() {
        editor.sign_log.clear();
        editor.tab = crate::evm_pane::EvmTab::Result;
    }
    let mut effects = crate::send::start(state);
    for effect in &mut effects {
        if let Effect::Send(job) = effect {
            job.sign = Some(SignRequest { force_revert });
            if let Some(flight) = state
                .tabs
                .get_mut(job.tab)
                .and_then(|t| t.in_flight.as_mut())
            {
                flight.signing = true;
            }
        }
    }
    effects
}

/// Whether `record` is the audit record of a signed transaction: it may
/// be out, whatever ended the request.
pub fn is_signed(record: &RequestRecord) -> bool {
    record
        .evm
        .as_ref()
        .is_some_and(rustman_evm_signing::receipt::is_signed)
}

// ---- jobs in flight -------------------------------------------------------

/// Signing jobs still running, so quitting can wait for their audit
/// entries.
static PENDING: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());

/// One running signing job; dropped when it ends.
struct PendingGuard;

impl PendingGuard {
    fn new() -> Self {
        *PENDING.0.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        Self
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        let mut count = PENDING.0.lock().unwrap_or_else(|e| e.into_inner());
        *count = count.saturating_sub(1);
        PENDING.1.notify_all();
    }
}

/// After the loop ended (its sends cancelled): wait until every signing
/// job wrote its audit entry. `notice` runs once when one is still going.
pub fn wait_for_pending(notice: impl FnOnce()) {
    let mut count = PENDING.0.lock().unwrap_or_else(|e| e.into_inner());
    if *count > 0 {
        notice();
    }
    while *count > 0 {
        count = PENDING.1.wait(count).unwrap_or_else(|e| e.into_inner());
    }
}

/// The effect runner's hook: a signing job runs with the action channel
/// its dialog asks through, and counts as pending until it ends.
pub fn intercept(effect: Effect, actions: &UnboundedSender<Action>) -> Option<Effect> {
    let Effect::Send(job) = effect else {
        return Some(effect);
    };
    if job.sign.is_none() {
        return Some(Effect::Send(job));
    }
    let guard = PendingGuard::new();
    let actions = actions.clone();
    tokio::spawn(async move {
        let link = actions.clone();
        let done = tokio::task::spawn_blocking(move || {
            let _guard = guard;
            crate::send::execute_with(&job, Some(&link))
        })
        .await
        .unwrap_or_else(|err| match err.try_into_panic() {
            Ok(payload) => Action::Abort(Abort::Failed(crate::effects::panic_text(&*payload))),
            Err(err) => Action::Abort(Abort::Failed(err.to_string())),
        });
        let _ = actions.send(done);
    });
    None
}

// ---- the console ----------------------------------------------------------

/// A message from a signing job to the loop.
#[derive(Debug)]
pub enum Msg {
    /// A line the gate showed: the preview, the summary, the hash, the
    /// receipt, the explorer link; `warn` for its notes.
    Log {
        tab: TabId,
        line: String,
        warn: bool,
    },
    /// Open the dialog.
    Ask(Box<SignDialog>),
}

/// The answer to a prompt.
enum Answer {
    Text(Zeroizing<String>),
    /// `Esc`: nothing is signed or sent.
    Cancelled,
}

/// Where a dialog's answer goes; answered once.
#[derive(Clone)]
pub struct Reply(Arc<Mutex<Option<mpsc::Sender<Answer>>>>);

impl Reply {
    fn send(&self, answer: Answer) {
        let sender = self.0.lock().ok().and_then(|mut s| s.take());
        if let Some(sender) = sender {
            let _ = sender.send(answer);
        }
    }
}

/// The confirmation summary as the dialog shows it: escaped, one value per
/// line, never redacted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryView {
    pub fields: Vec<(&'static str, String)>,
    /// `SIMULATION REVERTED: …` of a forced transaction.
    pub reverted: Option<String>,
    /// The max cost above `[evm] max_cost`.
    pub cost: Option<String>,
    /// The chain's name, typed on a protected chain.
    pub chain: String,
}

impl SummaryView {
    pub fn new(summary: &Summary, cost: Option<&str>) -> Self {
        Self {
            fields: summary.lines(),
            reverted: summary.warning(),
            cost: cost.map(|c| printable_line(c).into_owned()),
            chain: printable_line(&summary.chain.name).into_owned(),
        }
    }
}

/// The gate's console in the TUI: lines go to the tab's pane, questions
/// to the dialog.
pub struct TuiConsole {
    tab: TabId,
    seq: u64,
    /// `None`: no dialog to confirm in; the gate refuses.
    actions: Option<UnboundedSender<Action>>,
    cancel: CancellationToken,
    /// `Esc` at a prompt.
    aborted: AtomicBool,
    summary: Mutex<Option<SummaryView>>,
    /// The yes of the first question was given: the next one is the
    /// typed chain name.
    confirmed: AtomicBool,
}

impl TuiConsole {
    pub fn new(job: &SendJob, actions: Option<UnboundedSender<Action>>) -> Self {
        Self {
            tab: job.tab,
            seq: job.seq,
            actions,
            cancel: job.cancel.clone(),
            aborted: AtomicBool::new(false),
            summary: Mutex::new(None),
            confirmed: AtomicBool::new(false),
        }
    }

    fn post(&self, msg: Msg) -> bool {
        self.actions
            .as_ref()
            .is_some_and(|a| a.send(Action::Sign(Box::new(msg))).is_ok())
    }

    fn log(&self, text: &str, warn: bool) {
        for line in text.lines() {
            let line = line.to_owned();
            self.post(Msg::Log {
                tab: self.tab,
                line,
                warn,
            });
        }
    }

    /// Open the dialog at `stage` and wait for its answer; `None` when it
    /// was cancelled (`Esc`, the send cancelled, the loop gone).
    fn prompt(&self, stage: Stage, question: &str) -> Option<Zeroizing<String>> {
        let (tx, rx) = mpsc::channel();
        let summary = self.summary.lock().ok().and_then(|s| s.clone());
        let dialog = SignDialog {
            tab: self.tab,
            seq: self.seq,
            stage,
            question: printable_line(question).into_owned(),
            summary,
            selected: CANCEL,
            text: Zeroizing::new(String::new()),
            reply: Reply(Arc::new(Mutex::new(Some(tx)))),
        };
        if !self.post(Msg::Ask(Box::new(dialog))) {
            return None;
        }
        loop {
            if self.cancel.is_cancelled() {
                return None;
            }
            match rx.recv_timeout(PROMPT_TICK) {
                Ok(Answer::Text(text)) => return Some(text),
                Ok(Answer::Cancelled) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.aborted.store(true, Ordering::SeqCst);
                    return None;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

impl Console for TuiConsole {
    fn interactive(&self) -> bool {
        self.actions.is_some()
    }

    fn show(&self, text: &str) {
        self.log(text, false);
    }

    fn note(&self, text: &str) {
        self.log(text, true);
    }

    fn summary(&self, summary: &Summary, cost: Option<&str>) {
        self.show(&rustman_evm_signing::flow::confirmation_text(summary, cost));
        if let Ok(mut kept) = self.summary.lock() {
            *kept = Some(SummaryView::new(summary, cost));
        }
    }

    fn ask(&self, question: &str) -> Option<String> {
        let stage = match self.confirmed.load(Ordering::SeqCst) {
            true => Stage::ChainName,
            false => Stage::Confirm,
        };
        let answer = self.prompt(stage, question)?;
        if stage == Stage::Confirm && rustman_evm_signing::confirm::is_yes(&answer) {
            self.confirmed.store(true, Ordering::SeqCst);
        }
        Some(answer.as_str().to_owned())
    }

    fn password(&self, question: &str) -> Result<Option<Zeroizing<String>>, String> {
        if self.actions.is_none() {
            return Ok(None);
        }
        Ok(self.prompt(Stage::Password, question))
    }

    fn cancelled(&self) -> bool {
        self.cancel.is_cancelled() || self.aborted.load(Ordering::SeqCst)
    }
}

// ---- the dialog -----------------------------------------------------------

/// What the dialog asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The keystore password, never echoed.
    Password,
    /// Sign or Cancel, Cancel preselected.
    Confirm,
    /// The chain name, typed exactly.
    ChainName,
}

/// The column the summary's values start at.
const LABEL: usize = 13;

/// The choices of the confirmation.
const SIGN: usize = 0;
const CANCEL: usize = 1;

/// The signing dialog of one job.
#[derive(Clone)]
pub struct SignDialog {
    pub tab: TabId,
    pub seq: u64,
    pub stage: Stage,
    /// The gate's question, on one line.
    pub question: String,
    pub summary: Option<SummaryView>,
    /// `SIGN` or `CANCEL`.
    pub selected: usize,
    /// What is typed: the password or the chain name.
    text: Zeroizing<String>,
    reply: Reply,
}

impl SignDialog {
    /// A dialog with no job behind it (tests, snapshots).
    pub fn detached(stage: Stage, question: &str, summary: Option<SummaryView>) -> Self {
        Self {
            tab: 0,
            seq: 0,
            stage,
            question: printable_line(question).into_owned(),
            summary,
            selected: CANCEL,
            text: Zeroizing::new(String::new()),
            reply: Reply(Arc::new(Mutex::new(None))),
        }
    }

    /// How many characters are typed (the password is never shown).
    pub fn typed_len(&self) -> usize {
        self.text.chars().count()
    }

    /// A key; true when the dialog answered and closes.
    pub fn key(&mut self, chord: Chord) -> bool {
        let cancel =
            chord.code == Code::Esc || (chord.ctrl && matches!(chord.code, Code::Char('c' | 'q')));
        if cancel {
            self.reply.send(Answer::Cancelled);
            return true;
        }
        match self.stage {
            Stage::Confirm => self.choice_key(chord.code),
            Stage::Password | Stage::ChainName => self.text_key(chord),
        }
    }

    fn choice_key(&mut self, code: Code) -> bool {
        match code {
            Code::Left | Code::Right | Code::Up | Code::Down | Code::Tab | Code::BackTab => {
                self.selected = 1 - self.selected;
                false
            }
            Code::Enter => {
                let answer = if self.selected == SIGN { "y" } else { "n" };
                self.reply.send(Answer::Text(Zeroizing::new(answer.into())));
                true
            }
            _ => false,
        }
    }

    fn text_key(&mut self, chord: Chord) -> bool {
        match chord.code {
            Code::Enter => {
                let text = std::mem::replace(&mut self.text, Zeroizing::new(String::new()));
                self.reply.send(Answer::Text(text));
                true
            }
            Code::Backspace => {
                self.text.pop();
                false
            }
            _ => {
                if let Some(c) = chord.typed_char() {
                    self.text.push(c);
                }
                false
            }
        }
    }

    fn title(&self) -> &'static str {
        match self.stage {
            Stage::Password => " Unlock signer ",
            Stage::Confirm | Stage::ChainName => " Sign and send transaction ",
        }
    }

    /// The dialog's lines for an inner width of `width` columns. A long
    /// value continues on indented lines under its own column, so no
    /// part of it can look like another field.
    fn lines(&self, ui: &UiOptions, width: usize) -> Vec<Line<'static>> {
        let theme = &ui.theme;
        let mut lines = Vec::new();
        if let Some(summary) = &self.summary {
            if let Some(reverted) = &summary.reverted {
                let style = theme.badge(Token::Error).add_modifier(Modifier::BOLD);
                lines.push(Line::styled(format!(" {reverted} "), style));
            }
            if let Some(cost) = &summary.cost {
                let style = theme.fg(Token::Warn).add_modifier(Modifier::BOLD);
                lines.push(Line::styled(format!("WARNING: {cost}"), style));
            }
            let room = width.saturating_sub(LABEL).max(16);
            for (label, value) in &summary.fields {
                let chars: Vec<char> = value.chars().collect();
                for (i, part) in chars.chunks(room).enumerate() {
                    let label = if i == 0 { *label } else { "" };
                    lines.push(Line::from(vec![
                        Span::styled(format!("{label:<LABEL$}"), theme.fg(Token::Muted)),
                        Span::styled(part.iter().collect::<String>(), theme.fg(Token::Text)),
                    ]));
                }
            }
            lines.push(Line::default());
        }
        let dot = ui.glyphs.dot;
        match self.stage {
            Stage::Confirm => {
                lines.push(Line::from(self.question.trim_end().to_owned()));
                lines.push(Line::default());
                let mut spans = Vec::new();
                for (i, label) in ["Sign and send", "Cancel"].into_iter().enumerate() {
                    let style = if i == self.selected {
                        theme.selection().add_modifier(Modifier::BOLD)
                    } else {
                        theme.fg(Token::Text)
                    };
                    spans.push(Span::styled(format!(" {label} "), style));
                    spans.push(Span::raw("  "));
                }
                lines.push(Line::from(spans));
                lines.push(Line::default());
                lines.push(hint(
                    &format!(
                        "Left/Right choose {dot} Enter confirm {dot} Esc cancel: nothing is signed"
                    ),
                    ui,
                ));
            }
            Stage::ChainName | Stage::Password => {
                lines.push(Line::from(self.question.trim_end().to_owned()));
                let shown = match self.stage {
                    Stage::Password => String::new(),
                    _ => printable_line(&self.text).into_owned(),
                };
                let style = theme.fg(Token::Text);
                let mut input = vec![
                    Span::styled(format!("{} {shown}", ui.glyphs.pointer), style),
                    Span::styled(" ", style.add_modifier(Modifier::REVERSED)),
                ];
                if self.stage == Stage::Password {
                    input.push(Span::styled(" (input hidden)", theme.fg(Token::Muted)));
                }
                lines.push(Line::from(input));
                lines.push(Line::default());
                lines.push(hint(
                    &format!("Enter confirm {dot} Esc cancel: nothing is signed"),
                    ui,
                ));
            }
        }
        lines
    }
}

impl fmt::Debug for SignDialog {
    /// Never the typed text: it may be the password.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignDialog")
            .field("tab", &self.tab)
            .field("seq", &self.seq)
            .field("stage", &self.stage)
            .field("question", &self.question)
            .field("summary", &self.summary)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl PartialEq for SignDialog {
    fn eq(&self, other: &Self) -> bool {
        (self.tab, self.seq, self.stage, self.selected)
            == (other.tab, other.seq, other.stage, other.selected)
            && self.question == other.question
            && self.summary == other.summary
            && Arc::ptr_eq(&self.reply.0, &other.reply.0)
    }
}

impl Eq for SignDialog {}

/// Draw the dialog centred, wide enough for an address and its label.
pub fn render(frame: &mut Frame, dialog: &SignDialog, ui: &UiOptions) {
    let area = frame.area();
    let width = area.width.min(96);
    let inner = usize::from(width.saturating_sub(4));
    let paragraph = Paragraph::new(dialog.lines(ui, inner)).wrap(Wrap { trim: false });
    let rows = paragraph.line_count(width.saturating_sub(4));
    let height = u16::try_from(rows + 2).unwrap_or(u16::MAX).min(area.height);
    let [row] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [rect] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(row);
    frame.render_widget(Clear, rect);
    let frame_block = block(dialog.title(), ui).padding(Padding::horizontal(1));
    frame.render_widget(paragraph.block(frame_block), rect);
}

// ---- the loop's side ------------------------------------------------------

/// A message from a signing job.
pub fn on_msg(state: &mut State, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Log { tab, line, warn } => {
            if let Some(editor) = state.tabs.get_mut(tab).and_then(|t| t.editor.evm_mut()) {
                editor.push_sign_line(&line, warn);
            }
        }
        Msg::Ask(dialog) => {
            let live = state
                .tabs
                .get(dialog.tab)
                .and_then(|t| t.in_flight.as_ref())
                .is_some_and(|f| f.seq == dialog.seq && f.signing);
            if live {
                // The prompt wins over any other overlay: the gate waits.
                state.overlay = Some(Overlay::Sign(dialog));
            } else {
                dialog.reply.send(Answer::Cancelled);
            }
        }
    }
    Vec::new()
}

/// Keys while the dialog is open: nothing else reaches the shell.
pub fn key(state: &mut State, chord: Chord) -> Vec<Effect> {
    let Some(Overlay::Sign(dialog)) = &mut state.overlay else {
        return Vec::new();
    };
    if dialog.key(chord) {
        state.overlay = None;
    }
    Vec::new()
}

/// The job of `tab`'s send `seq` ended: its dialog (if still open) goes.
pub fn close_dialog(state: &mut State, tab: TabId, seq: u64) {
    if let Some(Overlay::Sign(dialog)) = &state.overlay
        && dialog.tab == tab
        && dialog.seq == seq
    {
        state.overlay = None;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn quitting_waits_for_a_signing_job_to_record_its_audit_entry() {
        let guard = PendingGuard::new();
        let job = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            drop(guard);
        });
        let started = Instant::now();
        let mut told = false;
        wait_for_pending(|| told = true);
        assert!(told, "the user is told why quitting waits");
        assert!(started.elapsed() >= Duration::from_millis(100));
        job.join().unwrap();
        let mut told = false;
        wait_for_pending(|| told = true);
        assert!(!told, "nothing pending: no wait");
    }
}
