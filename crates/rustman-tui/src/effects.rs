//! The effect runner: executes [`Effect`]s off the render loop and reports
//! their completions as [`Action`]s on the action channel (design D2).

use std::any::Any;
use std::io::Write;
use std::path::Path;

use rustman_core::Workspace;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::app::{Abort, Action, Effect};
use crate::response::{Source, find, html, picker, received};
use crate::{clipboard, external, files, graphql_schema, reveal, send, viz};

/// Run one effect to completion. This blocks (disk I/O), so the runner calls
/// it on a blocking thread; tools such as the screenshot example call it
/// directly.
pub fn execute(effect: Effect) -> Action {
    match effect {
        Effect::LoadWorkspace(root) => Action::WorkspaceLoaded(
            Workspace::load_with(&root, &files::registry())
                .map(Box::new)
                .map_err(|e| e.to_string()),
        ),
        Effect::Save { tab, path, op } => Action::Saved {
            tab,
            path,
            result: op.apply().map_err(|e| e.to_string()),
        },
        Effect::File(op) => {
            let result = op.apply().map_err(|e| e.to_string());
            Action::FileDone { op, result }
        }
        Effect::ReadRequest { tab, root, path } => Action::RequestRead {
            tab,
            result: files::read_request(&root, &path).map(Box::new),
        },
        Effect::EditExternally {
            tab,
            text,
            extension,
        } => Action::ExternalEdited {
            tab,
            result: external::edit(&text, extension, |name| std::env::var(name).ok())
                .map_err(|e| e.to_string()),
        },
        Effect::CreateWorkspace(dir) => {
            let display = dir.display().to_string();
            Action::WorkspaceCreated(
                rustman_core::init::init(&dir, &display, false)
                    .map(|_| dir)
                    .map_err(|e| e.to_string()),
            )
        }
        other => execute_response(other),
    }
}

/// The effects of sending and of the response viewer.
fn execute_response(effect: Effect) -> Action {
    match effect {
        Effect::Send(job) => send::execute(&job),
        Effect::Filter { tab, expr, source } => Action::Filtered {
            tab,
            result: find::parse(&expr).and_then(|path| find::filter(&source, &path)),
            expr,
        },
        Effect::Search { tab, job } => Action::Searched {
            tab,
            generation: job.generation,
            matches: job.run(),
        },
        Effect::ReadPage { tab, page, source } => Action::PageRead {
            tab,
            page,
            result: received::read_page(&source, page).map_err(|e| e.to_string()),
        },
        Effect::LoadExchanges { tab, dir, file } => Action::ExchangesLoaded {
            tab,
            result: picker::load(&dir, &file),
        },
        Effect::Diff(job) => Action::Diffed(Box::new(picker::DiffView::compute(&job))),
        Effect::SaveBody { source, path } => Action::BodySaved {
            result: save_body(&source, &path).map_err(|e| e.to_string()),
            path,
        },
        Effect::Copy(text) => Action::Copied {
            tool: clipboard::copy_with(&text, &clipboard::platform_tools()),
            text,
        },
        other => execute_other(other),
    }
}

/// Secrets, the viz hand-off and reports.
fn execute_other(effect: Effect) -> Action {
    match effect {
        Effect::ReadSecrets {
            root,
            environment,
            names,
            backend,
        } => Action::SecretsRead {
            result: reveal::read(&root, &environment, &names, backend),
            environment,
        },
        Effect::Viz(job) => Action::VizDone(viz::open(&job)),
        Effect::WriteReport(job) => Action::ReportWritten(html::write(&job)),
        Effect::Webhook(job) => crate::webhook::execute(*job),
        Effect::GraphqlSchema(job) => graphql_schema::execute(&job),
        Effect::Snippet(job) => crate::copy_as::execute(&job),
        Effect::Import(job) => crate::import_dialog::execute(&job),
        other => Action::Abort(Abort::Failed(format!("unexpected effect {other:?}"))),
    }
}

/// Write `source` to a new file at `path` (an existing file is kept).
fn save_body(source: &Source, path: &Path) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    match source {
        Source::Memory(bytes) => file.write_all(bytes),
        Source::Spool(spool) => {
            std::io::copy(&mut std::fs::File::open(spool.path())?, &mut file).map(|_| ())
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
            let Some(effect) = crate::webhook::intercept(effect, &actions) else {
                continue;
            };
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
    fn bodies_are_saved_to_new_files_only() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("body.png");
        let source = Source::Memory(b"\x89PNG".to_vec().into());
        let effect = Effect::SaveBody {
            source: source.clone(),
            path: path.clone(),
        };
        assert!(matches!(
            execute(effect.clone()),
            Action::BodySaved { result: Ok(()), .. }
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"\x89PNG");
        assert!(matches!(
            execute(effect),
            Action::BodySaved { result: Err(_), .. }
        ));
        let mut spool = tempfile::NamedTempFile::new().unwrap();
        spool.write_all(b"spooled").unwrap();
        let spooled = tmp.path().join("big.json");
        save_body(&Source::Spool(std::sync::Arc::new(spool)), &spooled).unwrap();
        assert_eq!(std::fs::read(&spooled).unwrap(), b"spooled");
    }

    #[test]
    fn filters_and_pages_run_off_the_loop() {
        let source = Source::Memory(br#"{"items":[{"id":1},{"id":2}]}"#.to_vec().into());
        let effect = Effect::Filter {
            tab: 1,
            expr: "$.items[*].id".into(),
            source: source.clone(),
        };
        let Action::Filtered {
            result: Ok(tree), ..
        } = execute(effect)
        else {
            panic!("expected matches");
        };
        assert_eq!(tree.len(), 2);
        let Action::PageRead {
            result: Ok(lines), ..
        } = execute(Effect::ReadPage {
            tab: 1,
            page: 0,
            source,
        })
        else {
            panic!("expected a page");
        };
        assert_eq!(lines.len(), 1);
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
