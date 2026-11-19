//! The collection tree in the sidebar (spec: Collection tree sidebar):
//! folders and request files in run order, each request with a kind or
//! method badge, collapsible folders, and a `/` fuzzy filter.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use rustman_core::Workspace;
use rustman_core::collection::{Collection, Entry, RequestEntry};
use rustman_core::model::Request;

use crate::theme::Token;

/// What the tree remembers between workspace reloads.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TreeState {
    /// Folders the user collapsed (workspace-relative paths).
    pub collapsed: BTreeSet<PathBuf>,
    pub filter: Option<Filter>,
}

/// The `/` filter.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Filter {
    pub query: String,
    /// Typing goes to the query (until `Enter` or `Esc`).
    pub editing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    Folder { collapsed: bool },
    Request(Badge),
}

/// A request's badge: `GET`, `POST`, `GQL`, `MCP`, ...
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Badge {
    pub text: String,
    pub token: Token,
}

/// One visible row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    /// Workspace-relative path of the folder or request file.
    pub path: PathBuf,
    pub depth: usize,
    pub label: String,
    pub kind: RowKind,
    /// Character indices of `label` the filter matched.
    pub matched: Vec<usize>,
}

impl TreeRow {
    pub fn is_request(&self) -> bool {
        matches!(self.kind, RowKind::Request(_))
    }
}

/// The badge of a request.
pub fn badge(request: &Request) -> Badge {
    let (text, token) = match request {
        Request::Http(http) => {
            let method = http.effective_method().to_owned();
            let token = method_token(&method);
            (method, token)
        }
        Request::Graphql(_) => ("GQL".to_owned(), Token::Info),
        Request::Jsonrpc(_) => ("RPC".to_owned(), Token::Info),
        Request::Mcp(_) => ("MCP".to_owned(), Token::Info),
        Request::Opaque(opaque) => {
            let text = match opaque.kind.as_str() {
                "graphql" => "GQL".to_owned(),
                other => other.to_ascii_uppercase(),
            };
            (text, Token::Info)
        }
    };
    Badge { text, token }
}

/// The colour of a method badge.
pub fn method_token(method: &str) -> Token {
    match method {
        "GET" | "HEAD" | "OPTIONS" => Token::Ok,
        "POST" => Token::Accent,
        "PUT" | "PATCH" => Token::Warn,
        "DELETE" => Token::Error,
        _ => Token::Info,
    }
}

/// A `/`-joined path for display and matching.
pub fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// The visible rows of `ws`.
pub fn rows(ws: &Workspace, state: &TreeState) -> Vec<TreeRow> {
    match state.filter.as_ref().filter(|f| !f.query.is_empty()) {
        Some(filter) => filtered(ws, &filter.query),
        None => {
            let mut out = Vec::new();
            walk(&ws.tree, 0, state, &mut out);
            out
        }
    }
}

fn walk(collection: &Collection, depth: usize, state: &TreeState, out: &mut Vec<TreeRow>) {
    for entry in &collection.entries {
        match entry {
            Entry::Collection(child) => {
                let collapsed = state.collapsed.contains(&child.path);
                out.push(TreeRow {
                    path: child.path.clone(),
                    depth,
                    label: child.name(),
                    kind: RowKind::Folder { collapsed },
                    matched: Vec::new(),
                });
                if !collapsed {
                    walk(child, depth + 1, state, out);
                }
            }
            Entry::Request(request) => out.push(request_row(request, depth, request.stem())),
        }
    }
}

fn request_row(request: &RequestEntry, depth: usize, label: String) -> TreeRow {
    TreeRow {
        path: request.path.clone(),
        depth,
        label,
        kind: RowKind::Request(badge(&request.request)),
        matched: Vec::new(),
    }
}

/// Requests whose path (without `.rman.yaml`) fuzzy-matches `query`, best
/// first, shown flat with their full path and the matched characters.
fn filtered(ws: &Workspace, query: &str) -> Vec<TreeRow> {
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, TreeRow)> = ws
        .requests()
        .into_iter()
        .filter_map(|r| {
            let label = r.entry.id();
            let mut indices = Vec::new();
            let haystack = Utf32Str::new(&label, &mut buf);
            let score = pattern.indices(haystack, &mut matcher, &mut indices)?;
            let mut row = request_row(r.entry, 0, label);
            indices.sort_unstable();
            indices.dedup();
            row.matched = indices.into_iter().map(|i| i as usize).collect();
            Some((score, row))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.label.cmp(&b.1.label)));
    scored.into_iter().map(|(_, row)| row).collect()
}

/// Every request file of `ws`, as `/`-joined paths (for the palette).
pub fn request_paths(ws: &Workspace) -> Vec<String> {
    ws.requests()
        .iter()
        .map(|r| display_path(&r.entry.path))
        .collect()
}

/// The folder a new request goes into when `row` is selected.
pub fn folder_of(row: Option<&TreeRow>) -> PathBuf {
    match row {
        Some(row) if !row.is_request() => row.path.clone(),
        Some(row) => row.path.parent().map(Path::to_path_buf).unwrap_or_default(),
        None => PathBuf::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> (tempfile::TempDir, Workspace) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(
            root.join("rustman.toml"),
            "[workspace]\nname = \"t\"\nversion = 1\n",
        )
        .unwrap();
        let request = |rel: &str, extra: &str| {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let text = format!("version: 1\nname: N\n{extra}");
            fs::write(path, text).unwrap();
        };
        request("users/get-user.rman.yaml", "kind: http\nurl: http://x/\n");
        request(
            "users/create-user.rman.yaml",
            "kind: http\nmethod: POST\nurl: http://x/\n",
        );
        request(
            "graph/query.rman.yaml",
            "kind: graphql\nendpoint: http://x/\ndocument: '{ a }'\n",
        );
        request(
            "ping.rman.yaml",
            "kind: http\nmethod: DELETE\nurl: http://x/\n",
        );
        let ws = Workspace::load(root).unwrap();
        (tmp, ws)
    }

    fn labels(rows: &[TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.label))
            .collect()
    }

    #[test]
    fn folders_and_requests_in_run_order_with_badges() {
        let (_tmp, ws) = fixture();
        let rows = rows(&ws, &TreeState::default());
        assert_eq!(
            labels(&rows),
            [
                "graph",
                "  query",
                "ping",
                "users",
                "  create-user",
                "  get-user"
            ]
        );
        let badges: Vec<_> = rows
            .iter()
            .filter_map(|r| match &r.kind {
                RowKind::Request(b) => Some((b.text.as_str(), b.token)),
                RowKind::Folder { .. } => None,
            })
            .collect();
        assert_eq!(
            badges,
            [
                ("GQL", Token::Info),
                ("DELETE", Token::Error),
                ("POST", Token::Accent),
                ("GET", Token::Ok)
            ]
        );
    }

    #[test]
    fn collapsed_folders_hide_their_requests() {
        let (_tmp, ws) = fixture();
        let mut state = TreeState::default();
        state.collapsed.insert(PathBuf::from("users"));
        let rows = rows(&ws, &state);
        assert_eq!(labels(&rows), ["graph", "  query", "ping", "users"]);
        assert_eq!(rows[3].kind, RowKind::Folder { collapsed: true });
    }

    #[test]
    fn the_filter_keeps_fuzzy_matches_with_their_characters() {
        // Scenario "Filter the tree".
        let (_tmp, ws) = fixture();
        let state = TreeState {
            filter: Some(Filter {
                query: "gusr".into(),
                editing: true,
            }),
            ..TreeState::default()
        };
        let rows = rows(&ws, &state);
        assert_eq!(labels(&rows), ["users/get-user"]);
        let matched: String = rows[0]
            .matched
            .iter()
            .map(|&i| rows[0].label.chars().nth(i).unwrap())
            .collect();
        assert_eq!(matched.to_lowercase(), "gusr");
    }

    #[test]
    fn new_requests_go_into_the_selected_folder() {
        let (_tmp, ws) = fixture();
        let rows = rows(&ws, &TreeState::default());
        assert_eq!(folder_of(Some(&rows[3])), PathBuf::from("users"));
        assert_eq!(folder_of(Some(&rows[5])), PathBuf::from("users"));
        assert_eq!(folder_of(Some(&rows[2])), PathBuf::new());
        assert_eq!(folder_of(None), PathBuf::new());
        assert_eq!(request_paths(&ws).len(), 4);
    }
}
