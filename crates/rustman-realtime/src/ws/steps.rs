//! The steps interpreter (design D3): `send`, `wait_for`, `sleep` and
//! `close` run in order on the session's task, and messages that arrive
//! meanwhile are recorded. `wait_for` matches only messages read after it
//! starts, with the JSONPath and regex evaluators of the assertions
//! (design D4). After the steps, the session listens until its bounds.

use std::time::{Duration, Instant};

use rustman_core::assertions::wait_matches;
use rustman_core::model::{CloseStep, WaitFor};
use rustman_core::vars::ResolvedStep;

use super::live::{Event, Live};
use crate::error::RealtimeError;

/// Whether the session goes on after a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Continue,
    /// A `close` step or a command closed the session.
    Closed,
    Cancelled,
}

impl Live {
    /// Run `steps`, then listen until the bounds (headless) or until the
    /// session is closed or cancelled (interactive).
    pub(super) async fn drive(&mut self, steps: &[ResolvedStep]) -> Result<(), RealtimeError> {
        for step in steps {
            match self.step(step).await? {
                Flow::Continue => {}
                Flow::Closed => return Ok(()),
                Flow::Cancelled => return self.cancelled().await,
            }
        }
        self.listen().await
    }

    async fn step(&mut self, step: &ResolvedStep) -> Result<Flow, RealtimeError> {
        match step {
            ResolvedStep::Send(frame) => self.send(frame).await.map(|()| Flow::Continue),
            ResolvedStep::WaitFor(wait) => self.wait_for(wait).await,
            ResolvedStep::Sleep(ms) => {
                let until = Instant::now() + Duration::from_millis(*ms);
                self.listen_until(until).await
            }
            ResolvedStep::Close(close) => {
                self.close(close, None).await;
                Ok(Flow::Closed)
            }
        }
    }

    /// Wait for a matching message, up to the step's timeout.
    async fn wait_for(&mut self, wait: &WaitFor) -> Result<Flow, RealtimeError> {
        let ms = wait.effective_timeout_ms();
        let deadline = Instant::now() + Duration::from_millis(ms);
        loop {
            match self.next(Some(deadline)).await? {
                Event::Message(Some(text)) => {
                    if wait_matches(wait, &text).map_err(RealtimeError::Step)? {
                        return Ok(Flow::Continue);
                    }
                }
                Event::Message(None) => {}
                Event::Deadline => {
                    let matcher = wait.describe();
                    return Err(RealtimeError::WaitTimedOut { ms, matcher });
                }
                Event::Closed => return Err(RealtimeError::ClosedWhileWaiting(wait.describe())),
                Event::Cancelled => return Ok(Flow::Cancelled),
                Event::Command(command) => self.command(command).await,
            }
            if !self.is_open() {
                return Err(RealtimeError::ClosedWhileWaiting(wait.describe()));
            }
        }
    }

    /// Record what arrives until `until` (a `sleep` step).
    async fn listen_until(&mut self, until: Instant) -> Result<Flow, RealtimeError> {
        while self.is_open() {
            match self.next(Some(until)).await? {
                Event::Message(_) => {}
                Event::Deadline | Event::Closed => break,
                Event::Cancelled => return Ok(Flow::Cancelled),
                Event::Command(command) => self.command(command).await,
            }
        }
        Ok(Flow::Continue)
    }

    /// After the steps: listen until `max_messages` received messages or
    /// `duration` since the start (headless), or until closed or
    /// cancelled (interactive). Bounds close with 1000.
    async fn listen(&mut self) -> Result<(), RealtimeError> {
        let bounds = self.bounds();
        let deadline = bounds.map(|b| self.started() + b.duration);
        while self.is_open() {
            if let Some(b) = bounds
                && self.received() >= b.max_messages
            {
                let end = format!("max_messages reached ({})", b.max_messages);
                self.close(&CloseStep::default(), Some(end)).await;
                break;
            }
            match self.next(deadline).await? {
                Event::Message(_) | Event::Closed => {}
                Event::Deadline => {
                    let ms = bounds.map_or(0, |b| b.duration.as_millis());
                    let end = format!("duration reached ({ms} ms)");
                    self.close(&CloseStep::default(), Some(end)).await;
                }
                Event::Cancelled => return self.cancelled().await,
                Event::Command(command) => self.command(command).await,
            }
        }
        Ok(())
    }

    async fn cancelled(&mut self) -> Result<(), RealtimeError> {
        let end = "cancelled".to_owned();
        self.close(&CloseStep::default(), Some(end)).await;
        Ok(())
    }
}
