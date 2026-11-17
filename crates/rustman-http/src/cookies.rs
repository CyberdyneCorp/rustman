//! Cookie jars, one per environment (`.rustman/cookies/<env>.json`).
//!
//! Storage and matching follow RFC 6265 (domain, path, expiry and `Secure`)
//! through `cookie_store`. Session cookies are kept too: the jar is the
//! session, and `rustman cookies clear` ends it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use cookie_store::{Cookie, CookieStore, RawCookie};
use http::header::{COOKIE, SET_COOKIE};
use http::{HeaderMap, HeaderValue, Uri};
use rustman_core::secrets::{ensure_private, write_private};

use crate::error::RequestError;

/// The folder of the jars, relative to the workspace root.
pub const COOKIES_DIR: &str = ".rustman/cookies";

/// The jar name used when no environment is active. It is not a valid
/// environment name, so it never collides with one.
pub const NO_ENVIRONMENT: &str = "@none";

/// A shared, persistent cookie jar. Clones share the same cookies.
#[derive(Clone, Default)]
pub struct CookieJar {
    store: Arc<Mutex<CookieStore>>,
    path: Option<PathBuf>,
    /// The workspace of a jar under `.rustman/cookies`, checked once before
    /// the first save (no symbolic link, git-ignored).
    workspace: Option<Arc<Workspace>>,
}

/// The workspace root of a jar, and whether its folder passed the checks.
#[derive(Debug)]
struct Workspace {
    root: PathBuf,
    checked: AtomicBool,
}

impl std::fmt::Debug for CookieJar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CookieJar")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Two handles are equal when they share the same jar.
impl PartialEq for CookieJar {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.store, &other.store)
    }
}

impl Eq for CookieJar {}

impl CookieJar {
    /// A jar that is never saved.
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// The jar file of environment `env` (`None`: no active environment).
    pub fn path_for(root: &Path, env: Option<&str>) -> PathBuf {
        let name = env.unwrap_or(NO_ENVIRONMENT);
        root.join(COOKIES_DIR).join(format!("{name}.json"))
    }

    /// The jar of environment `env` in the workspace at `root`.
    pub fn for_environment(root: &Path, env: Option<&str>) -> Result<Self, RequestError> {
        let mut jar = Self::open(&Self::path_for(root, env))?;
        jar.workspace = Some(Arc::new(Workspace {
            root: root.to_path_buf(),
            checked: AtomicBool::new(false),
        }));
        Ok(jar)
    }

    /// The jar saved at `path`; empty when the file does not exist.
    pub fn open(path: &Path) -> Result<Self, RequestError> {
        let store = match fs::read(path) {
            Ok(bytes) => parse(&bytes).map_err(|e| jar_error("read", path, &e))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => CookieStore::new(),
            Err(e) => return Err(jar_error("read", path, &e)),
        };
        Ok(Self {
            store: Arc::new(Mutex::new(store)),
            path: Some(path.to_path_buf()),
            workspace: None,
        })
    }

    /// Delete the jar of `env`. Returns whether it held a file.
    pub fn clear(root: &Path, env: Option<&str>) -> io::Result<bool> {
        match fs::remove_file(Self::path_for(root, env)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// The `name=value` pairs that would be sent to `url`.
    pub fn cookies_for(&self, url: &str) -> Vec<(String, String)> {
        let Ok(url) = url::Url::parse(url) else {
            return Vec::new();
        };
        self.lock()
            .get_request_values(&url)
            .map(|(n, v)| (n.to_owned(), v.to_owned()))
            .collect()
    }

    /// The `Cookie` header for a request to `uri`, if any cookie matches.
    pub(crate) fn header_for(&self, uri: &Uri) -> Option<HeaderValue> {
        let pairs = self.cookies_for(&uri.to_string());
        let joined = pairs
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");
        (!joined.is_empty())
            .then(|| HeaderValue::from_str(&joined).ok())
            .flatten()
    }

    /// Store the `Set-Cookie` headers of a response from `uri`, saving the jar
    /// (on a blocking thread) when there were any.
    pub(crate) async fn store(&self, uri: &Uri, headers: &HeaderMap) -> Result<(), RequestError> {
        if !self.remember(uri, headers) {
            return Ok(());
        }
        let jar = self.clone();
        tokio::task::spawn_blocking(move || jar.save())
            .await
            .map_err(|e| RequestError::Io(format!("cannot save cookie jar: {e}")))?
    }

    /// Add the `Set-Cookie` headers to the jar; false when there were none.
    fn remember(&self, uri: &Uri, headers: &HeaderMap) -> bool {
        let Ok(url) = url::Url::parse(&uri.to_string()) else {
            return false;
        };
        let cookies: Vec<RawCookie<'static>> = headers
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(|v| RawCookie::parse(v.to_owned()).ok())
            .collect();
        if cookies.is_empty() {
            return false;
        }
        self.lock()
            .store_response_cookies(cookies.into_iter(), &url);
        true
    }

    /// Write the jar to its file (a uniquely named temp file renamed into
    /// place, readable by the current user only). A workspace jar is written
    /// only when `.rustman/cookies` is a real, git-ignored folder. In-memory
    /// jars are not written.
    pub fn save(&self) -> Result<(), RequestError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        self.check_workspace()
            .map_err(|e| jar_error("save", path, &e))?;
        let json = {
            let store = self.lock();
            let cookies: Vec<&Cookie<'static>> = store.iter_unexpired().collect();
            serde_json::to_vec_pretty(&cookies).expect("cookies serialize")
        };
        write_private(path, &json).map_err(|e| jar_error("save", path, &e))
    }

    fn check_workspace(&self) -> Result<(), rustman_core::SecretError> {
        let Some(ws) = &self.workspace else {
            return Ok(());
        };
        if !ws.checked.load(Ordering::Relaxed) {
            ensure_private(&ws.root, COOKIES_DIR)?;
            ws.checked.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, CookieStore> {
        self.store.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Whether `request` carries its own `Cookie` header, which then replaces
/// the jar's cookies.
pub(crate) fn has_explicit_cookie(headers: &HeaderMap) -> bool {
    headers.contains_key(COOKIE)
}

fn parse(bytes: &[u8]) -> Result<CookieStore, serde_json::Error> {
    let cookies: Vec<Cookie<'static>> = serde_json::from_slice(bytes)?;
    Ok(
        CookieStore::from_cookies(cookies.into_iter().map(Ok::<_, serde_json::Error>), false)
            .expect("infallible"),
    )
}

fn jar_error(action: &str, path: &Path, err: &dyn std::fmt::Display) -> RequestError {
    RequestError::Io(format!(
        "cannot {action} cookie jar {}: {err}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(set_cookies: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in set_cookies {
            headers.append(SET_COOKIE, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    fn uri(text: &str) -> Uri {
        text.parse().unwrap()
    }

    #[tokio::test]
    async fn domain_path_and_secure_rules_apply() {
        let jar = CookieJar::in_memory();
        let set = headers(&["a=1; Path=/api", "b=2; Secure", "c=3; Domain=other.test"]);
        jar.store(&uri("http://h.test/api/login"), &set)
            .await
            .unwrap();
        assert_eq!(
            jar.cookies_for("http://h.test/api/x"),
            [("a".into(), "1".into())]
        );
        assert!(jar.cookies_for("http://h.test/web").is_empty());
        let mut secure = jar.cookies_for("https://h.test/api/x");
        secure.sort();
        assert_eq!(secure, [("a".into(), "1".into()), ("b".into(), "2".into())]);
        assert!(jar.cookies_for("http://other.test/").is_empty());
    }

    #[tokio::test]
    async fn expired_cookies_are_removed() {
        let jar = CookieJar::in_memory();
        let u = uri("http://h.test/");
        jar.store(&u, &headers(&["sid=abc"])).await.unwrap();
        jar.store(&u, &headers(&["sid=gone; Max-Age=0"]))
            .await
            .unwrap();
        assert!(jar.cookies_for("http://h.test/").is_empty());
    }

    #[tokio::test]
    async fn the_header_joins_every_matching_cookie() {
        let jar = CookieJar::in_memory();
        jar.store(&uri("http://h.test/"), &headers(&["a=1", "b=2"]))
            .await
            .unwrap();
        let header = jar.header_for(&uri("http://h.test/x")).unwrap();
        let mut parts: Vec<&str> = header.to_str().unwrap().split("; ").collect();
        parts.sort_unstable();
        assert_eq!(parts, ["a=1", "b=2"]);
        assert!(jar.header_for(&uri("http://elsewhere.test/")).is_none());
    }

    #[tokio::test]
    async fn jars_persist_session_cookies_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let jar = CookieJar::for_environment(dir.path(), Some("staging")).unwrap();
        jar.store(&uri("http://h.test/"), &headers(&["sid=abc; Path=/"]))
            .await
            .unwrap();
        let path = dir.path().join(".rustman/cookies/staging.json");
        assert!(path.is_file());
        let again = CookieJar::for_environment(dir.path(), Some("staging")).unwrap();
        assert_eq!(
            again.cookies_for("http://h.test/"),
            [("sid".into(), "abc".into())]
        );
        assert!(CookieJar::clear(dir.path(), Some("staging")).unwrap());
        assert!(!CookieJar::clear(dir.path(), Some("staging")).unwrap());
        let cleared = CookieJar::for_environment(dir.path(), Some("staging")).unwrap();
        assert!(cleared.cookies_for("http://h.test/").is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn jar_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let jar = CookieJar::for_environment(dir.path(), None).unwrap();
        jar.store(&uri("http://h.test/"), &headers(&["sid=abc"]))
            .await
            .unwrap();
        let path = CookieJar::path_for(dir.path(), None);
        assert!(path.ends_with(".rustman/cookies/@none.json"));
        let mode = fs::metadata(path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_cookies_folder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".rustman")).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), dir.path().join(COOKIES_DIR)).unwrap();
        let jar = CookieJar::for_environment(dir.path(), Some("dev")).unwrap();
        let err = jar
            .store(&uri("http://h.test/"), &headers(&["sid=abc"]))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("symbolic link"), "{err}");
        assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    #[test]
    fn concurrent_saves_of_one_jar_file_never_collide() {
        let dir = tempfile::tempdir().unwrap();
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let jar = CookieJar::for_environment(dir.path(), Some("dev")).unwrap();
                std::thread::spawn(move || (0..25).try_for_each(|_| jar.save()))
            })
            .collect();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }
        let files: Vec<_> = fs::read_dir(dir.path().join(COOKIES_DIR))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(files, [std::ffi::OsString::from("dev.json")]);
    }

    #[test]
    fn a_corrupt_jar_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = CookieJar::path_for(dir.path(), Some("dev"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "not json").unwrap();
        let err = CookieJar::open(&path).unwrap_err();
        assert!(
            err.to_string().starts_with("cannot read cookie jar "),
            "{err}"
        );
    }

    #[test]
    fn handles_compare_by_identity() {
        let jar = CookieJar::in_memory();
        assert_eq!(jar, jar.clone());
        assert_ne!(jar, CookieJar::in_memory());
    }
}
