//! `rustman docs serve` (api-docs-generation "Live-reload preview", task
//! 7.1): loopback only, rebuild and reload within 1 s of an edit, build
//! errors in the overlay without stopping the server, a busy port, and
//! secrets never reaching a served page or a build message.

mod common;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{TOKEN, workspace, write};
use rustman_core::{Secret, SecretBackend, SecretStore};
use rustman_docs::serve::{BuildOutcome, Preview, PreviewSource, RELOAD_PATH};
use rustman_docs::{DocsError, ExampleSource};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

/// The spec's reload budget, from saving a file to the page reloading.
const RELOAD_BUDGET: Duration = Duration::from_secs(1);

/// How long a test waits for something that should happen at all.
const PATIENCE: Duration = Duration::from_secs(20);

/// How long no build must finish for the preview to count as settled: well
/// over the 200 ms rebuild debounce plus a build.
const QUIET: Duration = Duration::from_millis(1000);

type Outcomes = Arc<Mutex<Vec<BuildOutcome>>>;

struct Running {
    addr: SocketAddr,
    outcomes: Outcomes,
    stop: CancellationToken,
    _watcher: rustman_docs::serve::PreviewWatcher,
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// Serve the fixture workspace (with `docs` under `[docs]`) on a free port,
/// with `token` in its file secret store.
async fn start(docs: &str) -> Running {
    let dir = workspace(docs);
    let root = dunce::canonicalize(dir.path()).unwrap();
    SecretStore::open(&root, SecretBackend::File)
        .set("local", &Secret::new("token", TOKEN.to_owned()))
        .unwrap();
    let source = PreviewSource::new(
        root.clone(),
        None,
        ExampleSource::History,
        Box::new(|root: &Path| Ok(SecretStore::open(root, SecretBackend::File))),
    );
    let outcomes = Outcomes::default();
    let heard = Arc::clone(&outcomes);
    let reporter = Box::new(move |o: &BuildOutcome| heard.lock().unwrap().push(o.clone()));
    let preview = Preview::bind(0, Arc::new(source), reporter).await.unwrap();
    let addr = preview.local_addr();
    assert!(addr.ip().is_loopback(), "{addr}");
    preview.rebuild().await;
    let watcher = preview.watch(&root).unwrap();
    let stop = CancellationToken::new();
    tokio::spawn(preview.serve(stop.clone()));
    // Let rebuilds from late events of writing the fixture finish before a
    // test edits it.
    settle(&outcomes).await;
    Running {
        addr,
        outcomes,
        stop,
        _watcher: watcher,
        _dir: dir,
        root,
    }
}

async fn get(addr: SocketAddr, path: &str) -> (u16, String) {
    get_from(addr, path, "localhost").await
}

async fn get_from(addr: SocketAddr, path: &str, host: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let head = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let status = text[9..12].parse().unwrap();
    let body = text.split_once("\r\n\r\n").unwrap().1.to_owned();
    (status, body)
}

/// An open reload stream, as a page's `EventSource` holds it.
struct Events {
    stream: TcpStream,
    seen: String,
    read: usize,
}

impl Events {
    async fn open(addr: SocketAddr) -> Self {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let head = format!("GET {RELOAD_PATH} HTTP/1.1\r\nHost: localhost\r\n\r\n");
        stream.write_all(head.as_bytes()).await.unwrap();
        Self {
            stream,
            seen: String::new(),
            read: 0,
        }
    }

    /// The generation of the next event.
    async fn next(&mut self) -> u64 {
        loop {
            if let Some(at) = self.seen[self.read..].find("data: ") {
                let start = self.read + at + "data: ".len();
                if let Some(len) = self.seen[start..].find('\n') {
                    self.read = start + len;
                    return self.seen[start..start + len].parse().unwrap();
                }
            }
            let mut buf = [0; 1024];
            let n = tokio::time::timeout(PATIENCE, self.stream.read(&mut buf))
                .await
                .expect("a reload event")
                .unwrap();
            assert!(n > 0, "the reload stream closed");
            self.seen.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    }
}

const PAGE: &str = "/endpoints/users-create.html";

#[tokio::test(flavor = "multi_thread")]
async fn pages_are_served_with_the_reload_script_only_in_preview() {
    let running = start("").await;
    let (status, html) = get(running.addr, PAGE).await;
    assert_eq!(status, 200);
    assert!(html.contains("Creates a user."), "{html}");
    assert!(html.contains(&format!("new EventSource(\"{RELOAD_PATH}\")")));
    let (status, index) = get(running.addr, "/").await;
    assert_eq!(status, 200);
    assert!(index.contains("EventSource"));
    let (status, _) = get(running.addr, "/endpoints/nope.html").await;
    assert_eq!(status, 404);
    let (status, _) = get(running.addr, "/../rustman.toml").await;
    assert_eq!(status, 404);
    // A DNS-rebinding page sends its own name as the host.
    let (status, body) = get_from(running.addr, PAGE, "rebound.example:4000").await;
    assert_eq!((status, body.as_str()), (421, "unknown host\n"));
    let (status, _) = get_from(running.addr, PAGE, &running.addr.to_string()).await;
    assert_eq!(status, 200);

    // A build of the same workspace has no preview additions.
    let report = common::build_site(&running.root);
    for name in &report.files {
        let text = std::fs::read(running.root.join("site").join(name)).unwrap();
        let text = String::from_utf8_lossy(&text);
        assert!(!text.contains(RELOAD_PATH), "{name}");
        assert!(!text.contains("rustman-preview-error"), "{name}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_reloads_the_page_within_one_second() {
    let running = start("").await;
    let mut events = Events::open(running.addr).await;
    let mut generation = events.next().await;
    // The first rebuild may run on cold caches of a loaded runner; the
    // budget holds when one of a few edits reloads within it.
    let mut fastest = Duration::MAX;
    for attempt in 1..=3 {
        let description = format!("Creates a user, now with roles ({attempt}).");
        let (elapsed, reloaded) =
            reload_after_edit(&running, &mut events, generation, &description).await;
        generation = reloaded;
        fastest = fastest.min(elapsed);
        if fastest < RELOAD_BUDGET {
            break;
        }
    }
    assert!(
        fastest < RELOAD_BUDGET,
        "fastest reload took {fastest:?}, over {RELOAD_BUDGET:?}"
    );
    // The build itself does not touch the workspace, so the builds stop.
    // A file system event delivered late (coalesced, on a loaded runner)
    // may cause one more build; a build that triggers builds never settles.
    let extra = settle(&running.outcomes).await;
    assert!(extra <= 1, "{extra} builds after the last edit");
}

/// Wait until no build has finished for [`QUIET`], and return how many
/// finished meanwhile. Fails when builds keep coming for [`PATIENCE`].
async fn settle(outcomes: &Outcomes) -> usize {
    let count = || outcomes.lock().unwrap().len();
    let (start, first) = (Instant::now(), count());
    let (mut seen, mut since) = (first, Instant::now());
    while since.elapsed() < QUIET {
        assert!(start.elapsed() < PATIENCE, "builds never settled");
        tokio::time::sleep(Duration::from_millis(50)).await;
        if count() != seen {
            (seen, since) = (count(), Instant::now());
        }
    }
    seen - first
}

/// Write `description` into the page's request, wait until the served page
/// shows it, and return how long that took and the generation shown.
async fn reload_after_edit(
    running: &Running,
    events: &mut Events,
    before: u64,
    description: &str,
) -> (Duration, u64) {
    let path = running.root.join("users/create.rman.yaml");
    let text = std::fs::read_to_string(&path).unwrap();
    let start = text.find("description: ").unwrap();
    let end = start + text[start..].find('\n').unwrap();
    let edited = format!(
        "{}description: {description}{}",
        &text[..start],
        &text[end..]
    );
    std::fs::write(&path, edited).unwrap();
    let saved = Instant::now();
    loop {
        let generation = events.next().await;
        assert!(generation > before);
        let (_, html) = get(running.addr, PAGE).await;
        if html.contains(description) {
            assert!(html.contains(&format!("var g=\"{generation}\"")));
            return (saved.elapsed(), generation);
        }
        assert!(saved.elapsed() < PATIENCE, "no rebuild with the edit");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_build_error_is_shown_without_stopping_the_server() {
    let running = start("").await;
    let mut events = Events::open(running.addr).await;
    let before = events.next().await;
    write(
        &running.root,
        "users/broken.rman.yaml",
        "version: 1\nkind: [\n",
    );
    let mut generation = events.next().await;
    while running
        .outcomes
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .result
        .is_ok()
    {
        generation = events.next().await;
    }
    assert!(generation > before);
    let (status, html) = get(running.addr, PAGE).await;
    assert_eq!(status, 200, "the last good build is still served");
    assert!(html.contains("Creates a user."));
    assert!(html.contains("id=\"rustman-preview-error\""), "{html}");
    assert!(html.contains("broken.rman.yaml"), "{html}");

    std::fs::remove_file(running.root.join("users/broken.rman.yaml")).unwrap();
    while running
        .outcomes
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .result
        .is_err()
    {
        events.next().await;
    }
    let (_, html) = get(running.addr, PAGE).await;
    assert!(!html.contains("rustman-preview-error"));
}

#[tokio::test(flavor = "multi_thread")]
async fn secrets_never_reach_pages_or_build_messages() {
    let running = start(&format!("logo = \"../{TOKEN}.png\"\n")).await;
    let outcomes = running.outcomes.lock().unwrap().clone();
    let message = outcomes[0].result.clone().unwrap_err();
    assert!(message.contains("«redacted:token»"), "{message}");
    assert!(!message.contains(TOKEN));
    let (status, page) = get(running.addr, "/").await;
    assert_eq!(status, 503, "no build succeeded yet");
    assert!(page.contains("rustman-preview-error"));
    assert!(!page.contains(TOKEN), "{page}");

    let mut events = Events::open(running.addr).await;
    events.next().await;
    write(
        &running.root,
        "rustman.toml",
        "[workspace]\nname = \"Shop\"\nversion = 1\n",
    );
    while running
        .outcomes
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .result
        .is_err()
    {
        events.next().await;
    }
    for path in [
        "/",
        PAGE,
        "/endpoints/auth-login.html",
        "/search-index.json",
    ] {
        let (status, page) = get(running.addr, path).await;
        assert_eq!(status, 200, "{path}");
        assert!(!page.contains(TOKEN), "{path}: {page}");
    }
    for outcome in running.outcomes.lock().unwrap().iter() {
        assert!(!format!("{outcome:?}").contains(TOKEN));
    }
}

#[tokio::test]
async fn a_busy_port_fails_with_exit_code_2() {
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port();
    let builder = Arc::new(PreviewSource::new(
        PathBuf::from("."),
        None,
        ExampleSource::History,
        Box::new(|root: &Path| Ok(SecretStore::open(root, SecretBackend::File))),
    ));
    let Err(err) = Preview::bind(port, builder, Box::new(|_| {})).await else {
        panic!("bound a busy port");
    };
    assert!(matches!(err, DocsError::Bind { .. }));
    assert_eq!(
        err.to_string(),
        format!("docs: cannot bind 127.0.0.1:{port}: address in use")
    );
    assert_eq!(err.exit_code(), 2);
}
