//! Termination signals become an [`Action::Abort`], so the loop ends normally
//! and the terminal guard restores the terminal (spec: Launching the TUI).
//! Without this, `kill` or closing the terminal window would leave the shell
//! in raw mode on the alternate screen with mouse reporting on.

use std::io;

use tokio::sync::mpsc::UnboundedSender;

use crate::app::{Abort, Action};

/// Send an abort on `actions` when the process is asked to stop. Must be
/// called inside a tokio runtime with I/O enabled.
#[cfg(unix)]
pub fn spawn(actions: &UnboundedSender<Action>) -> io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    // Exit statuses follow the shell convention: 128 + signal number.
    let kinds = [
        (SignalKind::terminate(), "SIGTERM", 143),
        (SignalKind::hangup(), "SIGHUP", 129),
        (SignalKind::interrupt(), "SIGINT", 130),
    ];
    for (kind, name, code) in kinds {
        let mut stream = signal(kind)?;
        forward(actions, Abort::Signal { name, code }, async move {
            stream.recv().await;
        });
    }
    Ok(())
}

/// Send an abort on `actions` when the console is closed, the user logs off
/// or the system shuts down, or on `Ctrl+Break`.
#[cfg(windows)]
pub fn spawn(actions: &UnboundedSender<Action>) -> io::Result<()> {
    use tokio::signal::windows::{ctrl_break, ctrl_close, ctrl_logoff, ctrl_shutdown};
    let mut close = ctrl_close()?;
    let mut shutdown = ctrl_shutdown()?;
    let mut logoff = ctrl_logoff()?;
    let mut brk = ctrl_break()?;
    let abort = |name| Abort::Signal { name, code: 130 };
    forward(actions, abort("CTRL_CLOSE"), async move {
        close.recv().await;
    });
    forward(actions, abort("CTRL_SHUTDOWN"), async move {
        shutdown.recv().await;
    });
    forward(actions, abort("CTRL_LOGOFF"), async move {
        logoff.recv().await;
    });
    forward(actions, abort("CTRL_BREAK"), async move {
        brk.recv().await;
    });
    Ok(())
}

/// Send `abort` once `arrived` completes.
fn forward(
    actions: &UnboundedSender<Action>,
    abort: Abort,
    arrived: impl Future<Output = ()> + Send + 'static,
) {
    let actions = actions.clone();
    tokio::spawn(async move {
        arrived.await;
        let _ = actions.send(Action::Abort(abort));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    #[tokio::test]
    async fn a_forwarded_event_aborts_the_loop() {
        let (tx, mut rx) = unbounded_channel();
        let abort = Abort::Signal {
            name: "SIGTERM",
            code: 143,
        };
        forward(&tx, abort.clone(), async {});
        let Some(Action::Abort(got)) = rx.recv().await else {
            panic!("expected an abort");
        };
        assert_eq!(got, abort);
    }
}
