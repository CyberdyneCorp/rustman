use super::*;
use crate::history::entry::NewEntry;
use crate::run::{RequestRecord, SentRequest};

fn settings(max_entries: usize) -> HistorySettings {
    HistorySettings {
        enabled: true,
        max_entries,
    }
}

fn entry(url: &str, spill: Option<&str>) -> Entry {
    let record = RequestRecord::new("a".into(), "A".into(), "a.rman.yaml".into());
    Entry::new(NewEntry {
        record: &record,
        sent: SentRequest {
            method: "GET".into(),
            url: url.into(),
            headers: Vec::new(),
            body: None,
        },
        request: Some("a.rman.yaml"),
        environment: Some("dev"),
        spill: spill.map(str::to_owned),
        replay: None,
    })
}

fn urls(store: &HistoryStore) -> Vec<String> {
    let listing = store.list().unwrap();
    assert!(listing.warnings.is_empty(), "{:?}", listing.warnings);
    listing.entries.into_iter().map(|e| e.sent.url).collect()
}

#[test]
fn entries_list_newest_first_and_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStore::for_workspace(dir.path(), &settings(500));
    let first = entry("http://h/1", None);
    store.record(&first).unwrap();
    store.record(&entry("http://h/2", None)).unwrap();
    assert_eq!(urls(&store), ["http://h/2", "http://h/1"]);
    assert_eq!(store.get(&first.id).unwrap(), first);
    assert!(
        dir.path()
            .join(".rustman/history")
            .join(format!("{}.json", first.id))
            .is_file()
    );
}

#[test]
fn ids_can_be_shortened_to_a_unique_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStore::at(dir.path(), 500);
    let one = entry("http://h/1", None);
    store.record(&one).unwrap();
    assert_eq!(store.get(&one.id[..20].to_lowercase()).unwrap().id, one.id);
    store.record(&entry("http://h/2", None)).unwrap();
    let err = store.get("0").unwrap_err();
    assert!(matches!(err, HistoryError::Ambiguous { .. }), "{err}");
    let err = store.get("ZZZ").unwrap_err();
    assert_eq!(
        err.to_string(),
        "no history entry 'ZZZ' (see 'rustman history list')"
    );
}

#[test]
fn the_501st_write_prunes_the_oldest_with_its_spill_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStore::for_workspace(dir.path(), &settings(500));
    let bodies = dir.path().join(".rustman/bodies");
    fs::create_dir_all(&bodies).unwrap();
    fs::write(bodies.join("big.bin"), b"spilled").unwrap();
    let oldest = entry("http://h/0", Some("big.bin"));
    store.record(&oldest).unwrap();
    for i in 1..500 {
        store
            .record(&entry(&format!("http://h/{i}"), None))
            .unwrap();
    }
    assert_eq!(store.list().unwrap().entries.len(), 500);
    assert!(bodies.join("big.bin").is_file());
    store.record(&entry("http://h/500", None)).unwrap();
    let listed = urls(&store);
    assert_eq!(listed.len(), 500);
    assert_eq!(listed[0], "http://h/500");
    assert_eq!(listed[499], "http://h/1");
    assert!(matches!(
        store.get(&oldest.id),
        Err(HistoryError::NotFound(_))
    ));
    assert!(!bodies.join("big.bin").exists());
}

#[test]
fn clear_deletes_every_entry_and_spill_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStore::for_workspace(dir.path(), &settings(500));
    assert_eq!(store.clear().unwrap(), 0);
    let bodies = dir.path().join(".rustman/bodies");
    fs::create_dir_all(&bodies).unwrap();
    fs::write(bodies.join("x.bin"), b"spilled").unwrap();
    store.record(&entry("http://h/1", Some("x.bin"))).unwrap();
    store.record(&entry("http://h/2", None)).unwrap();
    assert_eq!(store.clear().unwrap(), 2);
    assert!(urls(&store).is_empty());
    assert!(!bodies.join("x.bin").exists());
}

#[test]
fn spill_names_outside_the_bodies_folder_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStore::for_workspace(dir.path(), &settings(500));
    let bodies = dir.path().join(".rustman/bodies");
    assert_eq!(
        store.spill_name(&bodies.join("a.bin")).as_deref(),
        Some("a.bin")
    );
    assert_eq!(store.spill_name(&std::env::temp_dir().join("a.bin")), None);
    assert_eq!(store.spill_path("../secrets.yaml"), None);
    for name in ["", ".", "..", "C:x", "C:\\x", "/etc/x", "a\\b", "a/b"] {
        assert_eq!(store.spill_path(name), None, "{name}");
    }
    assert_eq!(store.spill_path("a.bin"), Some(bodies.join("a.bin")));
    assert_eq!(HistoryStore::at(dir.path(), 5).spill_path("a.bin"), None);
}

#[test]
fn unreadable_entries_are_skipped_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStore::at(dir.path(), 500);
    store.record(&entry("http://h/ok", None)).unwrap();
    fs::write(
        dir.path().join("01ARZ3NDEKTSV4RRFFQ69G5FAV.json"),
        b"{\"trunc",
    )
    .unwrap();
    fs::write(
        dir.path().join(".01ARZ3NDEKTSV4RRFFQ69G5FAW.json.tmp"),
        b"{",
    )
    .unwrap();
    let listing = store.list().unwrap();
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(listing.warnings.len(), 1);
    assert!(
        listing.warnings[0].starts_with("skipping unreadable history entry"),
        "{:?}",
        listing.warnings
    );
}

#[test]
fn concurrent_writers_keep_every_entry_readable() {
    let dir = tempfile::tempdir().unwrap();
    let writers: Vec<_> = (0..2)
        .map(|w| {
            let path = dir.path().to_path_buf();
            std::thread::spawn(move || {
                let store = HistoryStore::at(&path, 60);
                for i in 0..50 {
                    store
                        .record(&entry(&format!("http://h/{w}/{i}"), None))
                        .unwrap();
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let store = HistoryStore::at(dir.path(), 60);
    assert_eq!(urls(&store).len(), 60);
}

#[cfg(unix)]
#[test]
fn a_symlinked_state_folder_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), dir.path().join(".rustman")).unwrap();
    let store = HistoryStore::for_workspace(dir.path(), &settings(500));
    let err = store.record(&entry("http://h/1", None)).unwrap_err();
    assert!(matches!(err, HistoryError::Unsafe(_)), "{err}");
    assert!(fs::read_dir(elsewhere.path()).unwrap().next().is_none());
}

#[test]
fn an_empty_id_matches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStore::at(dir.path(), 500);
    store.record(&entry("http://h/1", None)).unwrap();
    for id in ["", "  "] {
        let err = store.get(id).unwrap_err();
        assert_eq!(
            err.to_string(),
            "a history id cannot be empty (see 'rustman history list')"
        );
    }
}

#[test]
fn getting_an_unreadable_entry_names_it_once_and_does_not_skip() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStore::at(dir.path(), 500);
    let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let path = dir.path().join(format!("{id}.json"));
    fs::write(&path, b"{\"trunc").unwrap();
    let message = store.get(id).unwrap_err().to_string();
    let shown = path.display().to_string();
    assert!(
        message.starts_with(&format!("cannot read history entry {shown}: ")),
        "{message}"
    );
    assert_eq!(message.matches(&shown).count(), 1, "{message}");
    assert!(!message.contains("skipping"), "{message}");
}

#[cfg(unix)]
#[test]
fn entries_are_private_and_leave_no_temporary_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStore::at(&dir.path().join("history"), 500);
    let one = entry("http://h/1", None);
    store.record(&one).unwrap();
    let path = store.dir().join(format!("{}.json", one.id));
    let mode = fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let dir_mode = fs::metadata(store.dir()).unwrap().permissions().mode();
    assert_eq!(dir_mode & 0o777, 0o700);
    let names: Vec<_> = fs::read_dir(store.dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(names.is_empty(), "{names:?}");
}

#[cfg(windows)]
#[test]
fn entries_do_not_inherit_the_folder_acl_on_windows() {
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStore::at(dir.path(), 500);
    let one = entry("http://h/1", None);
    store.record(&one).unwrap();
    let path = store.dir().join(format!("{}.json", one.id));
    let out = Command::new("icacls").arg(&path).output().unwrap();
    let listing = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(!listing.contains("(I)"), "inherited grant:\n{listing}");
    for broad in ["Everyone", "Authenticated Users", "BUILTIN\\Users"] {
        assert!(!listing.contains(broad), "{broad}:\n{listing}");
    }
}

#[test]
fn files_held_open_do_not_fail_pruning_on_windows() {
    assert!(busy(&io::Error::from(ErrorKind::ResourceBusy)));
    assert_eq!(
        busy(&io::Error::from(ErrorKind::PermissionDenied)),
        cfg!(windows)
    );
    assert_eq!(busy(&io::Error::from_raw_os_error(32)), cfg!(windows));
    assert!(!busy(&io::Error::from(ErrorKind::NotFound)));
    assert!(!busy(&io::Error::from(ErrorKind::StorageFull)));
}

fn spill(bodies: &Path, name: &str, age: std::time::Duration) {
    let path = bodies.join(name);
    fs::write(&path, b"raw body").unwrap();
    let file = fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(std::time::SystemTime::now() - age)
        .unwrap();
}

#[test]
fn pruning_deletes_old_orphan_spill_files_and_clear_deletes_all() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStore::for_workspace(dir.path(), &settings(10));
    let bodies = dir.path().join(".rustman/bodies");
    fs::create_dir_all(&bodies).unwrap();
    let hours = std::time::Duration::from_secs(2 * 60 * 60);
    spill(&bodies, "kept.bin", hours);
    spill(&bodies, "orphan.bin", hours);
    spill(&bodies, "fresh.bin", std::time::Duration::ZERO);
    store
        .record(&entry("http://h/0", Some("kept.bin")))
        .unwrap();
    assert!(bodies.join("kept.bin").is_file());
    assert!(bodies.join("fresh.bin").is_file());
    assert!(!bodies.join("orphan.bin").exists());
    assert_eq!(store.clear().unwrap(), 1);
    assert_eq!(fs::read_dir(&bodies).unwrap().count(), 0);
    spill(&bodies, "left.bin", std::time::Duration::ZERO);
    fs::remove_dir_all(dir.path().join(".rustman/history")).unwrap();
    assert_eq!(store.clear().unwrap(), 0);
    assert!(!bodies.join("left.bin").exists());
}
