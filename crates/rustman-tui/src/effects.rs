//! The effect runner: executes [`Effect`]s off the render loop and reports
//! their completions as [`Action`]s on the action channel (design D2).

use std::any::Any;

use rustman_core::Workspace;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::app::{Abort, Action, Effect};

/// Run one effect to completion. This blocks (disk I/O), so the runner calls
/// it on a blocking thread; tools such as the screenshot example call it
/// directly.
pub fn execute(effect: Effect) -> Action {
    match effect {
        Effect::LoadWorkspace(root) => Action::WorkspaceLoaded(
            Workspace::load(&root)
                .map(Box::new)
                .map_err(|e| e.to_string()),
        ),
        Effect::CreateWorkspace(dir) => {
            let display = dir.display().to_string();
            Action::WorkspaceCreated(
                rustman_core::init::init(&dir, &display, false)
                    .map(|_| dir)
                    .map_err(|e| e.to_string()),
            )
        }
    }
}

/// Spawn the effect runner task. Effects sent to the returned sender run
/// concurrently; each completion is sent to `actions`. The task ends when the
/// returned sender is dropped.
pub fn spawn_runner(actions: UnboundedSender<Action>) -> UnboundedSender<Effect> {
    spawn_runner_with(actions, execute)
}

/// [`spawn_runner`] with another executor. An effect that panics stops the
/// loop with [`Abort::Failed`] instead of leaving the screen waiting for a
/// completion that never comes.
pub fn spawn_runner_with(
    actions: UnboundedSender<Action>,
    exec: fn(Effect) -> Action,
) -> UnboundedSender<Effect> {
    let (tx, mut rx) = unbounded_channel::<Effect>();
    tokio::spawn(async move {
        while let Some(effect) = rx.recv().await {
            let actions = actions.clone();
            tokio::spawn(async move {
                let action = tokio::task::spawn_blocking(move || exec(effect))
                    .await
                    .unwrap_or_else(|err| match err.try_into_panic() {
                        Ok(payload) => Action::Abort(Abort::Failed(panic_text(&*payload))),
                        Err(err) => Action::Abort(Abort::Failed(err.to_string())),
                    });
                // The loop may have ended; a lost completion is fine then.
                let _ = actions.send(action);
            });
        }
    });
    tx
}

/// The message of a panic payload.
pub fn panic_text(payload: &(dyn Any + Send)) -> String {
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic");
    format!("background task panicked: {message}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_runner_reports_completions_on_the_action_channel() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("new");
        let (tx, mut rx) = unbounded_channel();
        let effects = spawn_runner(tx);
        effects.send(Effect::CreateWorkspace(dir.clone())).unwrap();
        let action = rx.recv().await.unwrap();
        assert!(matches!(action, Action::WorkspaceCreated(Ok(ref d)) if *d == dir));
        effects.send(Effect::LoadWorkspace(dir.clone())).unwrap();
        let Action::WorkspaceLoaded(Ok(ws)) = rx.recv().await.unwrap() else {
            panic!("expected a loaded workspace");
        };
        assert_eq!(ws.settings.workspace.name, "new");
    }

    #[tokio::test]
    async fn a_panicking_effect_aborts_the_loop() {
        fn boom(_: Effect) -> Action {
            panic!("disk on fire");
        }
        let (tx, mut rx) = unbounded_channel();
        let effects = spawn_runner_with(tx, boom);
        effects.send(Effect::LoadWorkspace("/x".into())).unwrap();
        let Action::Abort(Abort::Failed(message)) = rx.recv().await.unwrap() else {
            panic!("expected an abort");
        };
        assert!(message.contains("disk on fire"), "{message}");
    }

    #[test]
    fn panic_payloads_become_text() {
        let owned: Box<dyn Any + Send> = Box::new(String::from("owned"));
        assert!(panic_text(&*owned).ends_with("owned"));
        let other: Box<dyn Any + Send> = Box::new(3);
        assert!(panic_text(&*other).ends_with("unknown panic"));
    }

    #[test]
    fn errors_are_reported_as_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let action = execute(Effect::LoadWorkspace(tmp.path().to_path_buf()));
        assert!(matches!(action, Action::WorkspaceLoaded(Err(_))));
        std::fs::write(tmp.path().join("rustman.toml"), "").unwrap();
        let action = execute(Effect::CreateWorkspace(tmp.path().to_path_buf()));
        assert!(matches!(action, Action::WorkspaceCreated(Err(ref m)) if m.contains("already")));
    }
}
