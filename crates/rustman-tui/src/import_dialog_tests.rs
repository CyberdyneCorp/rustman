//! "Import…" driven through `update` with keys (importers task 6.1).
//! Effects run synchronously, the way the effect runner would, except in
//! the tests of results that arrive after the dialog moved on.

use std::path::Path;

use crate::app::{Action, Effect, Overlay, State, update};
use crate::copy_as_tests::{fixture, toasts, type_text};
use crate::import_dialog::{IMPORTING_HINT, ImportJob, Stage, execute};
use crate::workbench_tests::{event, press, text};

/// A literal secret in an imported collection.
const IMPORTED: &str = "imp0rt-s3cret-9d8c";

/// A Postman collection whose bearer token is a literal secret.
fn postman(token: &str) -> String {
    format!(
        r#"{{"info":{{"name":"Shop","schema":"https://schema.getpostman.com/json/collection/v2.1.0/collection.json"}},
"item":[{{"name":"List orders","request":{{"method":"GET","url":"https://api.example.com/orders",
"auth":{{"type":"bearer","bearer":[{{"key":"token","value":"{token}","type":"string"}}]}}}}}}]}}"#
    )
}

/// Every file and folder under `dir` with its size, sorted, so a dry run
/// that writes anything anywhere in the tree is caught.
fn listing(dir: &Path) -> Vec<(std::path::PathBuf, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let entry = entry.unwrap();
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                stack.push(entry.path());
            }
            out.push((entry.path(), if meta.is_dir() { 0 } else { meta.len() }));
        }
    }
    out.sort();
    out
}

#[test]
fn import_previews_a_dry_run_then_imports_on_confirmation() {
    let (tmp, mut state) = fixture();
    let root = tmp.path().join("ws");
    std::fs::write(root.join("shop.json"), postman(IMPORTED)).unwrap();
    let before = listing(tmp.path());
    press(&mut state, "Ctrl+p");
    type_text(&mut state, "import");
    press(&mut state, "Enter");
    assert!(matches!(state.overlay, Some(Overlay::Import(_))));
    type_text(&mut state, "shop.json");
    press(&mut state, "Enter");
    let screen = text(&state);
    assert!(screen.contains("Import preview"), "{screen}");
    assert!(screen.contains("would import Shop (postman)"), "{screen}");
    assert!(!screen.contains(IMPORTED), "{screen}");
    assert_eq!(listing(tmp.path()), before, "the preview writes nothing");
    insta::assert_snapshot!("import_preview", screen);
    press(&mut state, "Enter");
    assert!(state.overlay.is_none());
    assert!(root.join("shop/list-orders.rman.yaml").is_file());
    assert!(toasts(&state).contains(&"imported Shop (postman) into shop/".to_owned()));
    let ws = crate::workbench::workspace(&state).expect("reloaded");
    let paths: Vec<_> = ws.requests().iter().map(|r| r.entry.path.clone()).collect();
    assert!(
        paths.contains(&Path::new("shop/list-orders.rman.yaml").to_path_buf()),
        "the sidebar shows the import: {paths:?}"
    );
    for toast in toasts(&state) {
        assert!(!toast.contains(IMPORTED), "{toast}");
    }
    let written = std::fs::read_to_string(root.join("shop/list-orders.rman.yaml")).unwrap();
    assert!(!written.contains(IMPORTED), "{written}");
}

#[test]
fn import_reports_a_bad_source_in_the_dialog() {
    let (_tmp, mut state) = fixture();
    press(&mut state, "Ctrl+p");
    type_text(&mut state, "import");
    press(&mut state, "Enter");
    press(&mut state, "Enter");
    assert!(text(&state).contains("Enter a file or folder to import"));
    type_text(&mut state, "missing.json");
    press(&mut state, "Enter");
    let screen = text(&state);
    assert!(screen.contains("missing.json"), "{screen}");
    assert!(matches!(
        &state.overlay,
        Some(Overlay::Import(d)) if d.stage == crate::import_dialog::Stage::Source && d.error.is_some()
    ));
    press(&mut state, "Esc");
    assert!(state.overlay.is_none());
}

/// Press `key` without running its effects; returns the import it
/// started, if any.
fn press_held(state: &mut State, key: &str) -> Option<ImportJob> {
    let effects = update(state, Action::Key(event(key)));
    effects.into_iter().find_map(|effect| match effect {
        Effect::Import(job) => Some(*job),
        _ => None,
    })
}

/// Open the dialog with `src` written in the workspace and typed in.
fn open_with(state: &mut State, root: &Path, src: &str) {
    std::fs::write(root.join(src), postman(IMPORTED)).unwrap();
    press(state, "Ctrl+p");
    type_text(state, "import");
    press(state, "Enter");
    type_text(state, src);
}

#[test]
fn esc_waits_for_a_running_import_which_is_then_reported() {
    let (tmp, mut state) = fixture();
    let root = tmp.path().join("ws");
    open_with(&mut state, &root, "shop.json");
    press(&mut state, "Enter");
    let import = press_held(&mut state, "Enter").expect("the import starts");
    assert!(!import.dry_run);
    press(&mut state, "Esc");
    assert!(
        matches!(&state.overlay, Some(Overlay::Import(d)) if d.stage == Stage::Importing),
        "Esc does not hide an import that is writing"
    );
    let screen = text(&state);
    assert!(screen.contains(IMPORTING_HINT), "{screen}");
    let effects = update(&mut state, execute(&import));
    assert!(state.overlay.is_none());
    assert!(toasts(&state).contains(&"imported Shop (postman) into shop/".to_owned()));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::LoadWorkspace(_))),
        "the sidebar is reloaded: {effects:?}"
    );
}

#[test]
fn an_import_result_is_reported_even_without_its_dialog() {
    let (tmp, mut state) = fixture();
    let root = tmp.path().join("ws");
    open_with(&mut state, &root, "shop.json");
    press(&mut state, "Enter");
    let import = press_held(&mut state, "Enter").expect("the import starts");
    // Something else took the screen while the import ran.
    state.overlay = None;
    let effects = update(&mut state, execute(&import));
    assert!(toasts(&state).contains(&"imported Shop (postman) into shop/".to_owned()));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::LoadWorkspace(_)))
    );
}

#[test]
fn a_late_preview_does_not_reach_a_reopened_dialog() {
    let (tmp, mut state) = fixture();
    let root = tmp.path().join("ws");
    open_with(&mut state, &root, "a.json");
    let old = press_held(&mut state, "Enter").expect("the dry run starts");
    assert!(old.dry_run);
    press(&mut state, "Esc");
    assert!(
        state.overlay.is_none(),
        "Esc closes a dialog that only reads"
    );
    open_with(&mut state, &root, "b.json");
    update(&mut state, execute(&old));
    assert!(
        matches!(&state.overlay, Some(Overlay::Import(d)) if d.stage == Stage::Source),
        "the old preview is dropped: {:?}",
        state.overlay
    );
    // B is previewed before it is imported, and the import is B's.
    let preview = press_held(&mut state, "Enter").expect("B's dry run");
    assert!(preview.src.ends_with("b.json"));
    update(&mut state, execute(&preview));
    let import = press_held(&mut state, "Enter").expect("the import starts");
    assert_eq!(import.src, preview.src);
    assert!(!import.dry_run);
    assert!(
        !root.join("shop").exists(),
        "nothing written before the import ran"
    );
}

#[test]
fn the_import_is_of_the_previewed_source() {
    let (tmp, mut state) = fixture();
    let root = tmp.path().join("ws");
    open_with(&mut state, &root, "shop.json");
    let preview = press_held(&mut state, "Enter").expect("the dry run starts");
    // The typed text changes after the dry run started.
    if let Some(Overlay::Import(d)) = &mut state.overlay {
        d.source = crate::textarea::TextArea::from_text("other.json");
    }
    update(&mut state, execute(&preview));
    let import = press_held(&mut state, "Enter").expect("the import starts");
    assert!(import.src.ends_with("shop.json"), "{:?}", import.src);
}
