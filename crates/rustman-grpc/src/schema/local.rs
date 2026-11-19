//! Local schema sources (design D3): `.proto` files compiled in-process
//! with protox (no `protoc`), and binary `FileDescriptorSet` files.
//!
//! Paths arrive already resolved against the request file and contained in
//! the workspace (`rustman_core::vars::resolve_grpc`). Imports are looked up
//! only inside those folders: one reached through a symlink pointing out of
//! its import path is refused, so the workspace cannot be escaped that way.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use miette::{Diagnostic as _, SourceSpan};
use prost_reflect::DescriptorPool;
use protox::file::{
    ChainFileResolver, File, FileResolver, GoogleFileResolver, IncludeFileResolver,
};

use crate::error::GrpcError;

/// Compile `protos`, looking imports up in `import_paths` (each proto's
/// own folder when there are none). Errors read
/// `<file>:<line>:<col>: <message>`, the file named as imported.
pub fn compile_protos(
    protos: &[PathBuf],
    import_paths: &[PathBuf],
) -> Result<DescriptorPool, GrpcError> {
    let mut resolver = ChainFileResolver::new();
    for include in includes(protos, import_paths) {
        resolver.add(ContainedInclude::new(include));
    }
    resolver.add(GoogleFileResolver::new());
    let mut compiler = protox::Compiler::with_file_resolver(resolver);
    compiler.include_imports(true);
    compiler.open_files(protos).map_err(|e| compile_error(&e))?;
    Ok(compiler.descriptor_pool())
}

/// Load a binary `FileDescriptorSet` from `path`; `shown` names it in
/// errors.
pub fn load_descriptor_set(path: &Path, shown: &str) -> Result<DescriptorPool, GrpcError> {
    let bytes = read_capped(path, MAX_DESCRIPTOR_SET_BYTES)
        .map_err(|e| GrpcError::Schema(format!("cannot read descriptor set {shown}: {e}")))?;
    DescriptorPool::decode(bytes.as_slice())
        .map_err(|e| GrpcError::Schema(format!("invalid descriptor set {shown}: {e}")))
}

/// The largest descriptor set read, as for one fetched by reflection.
const MAX_DESCRIPTOR_SET_BYTES: u64 = 64 * 1024 * 1024;

/// The bytes of `path`, refused when longer than `limit`; reading stops
/// past the limit, so a huge file is never loaded whole.
fn read_capped(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::other(format!(
            "larger than the limit of {limit} bytes"
        )));
    }
    Ok(bytes)
}

fn includes(protos: &[PathBuf], import_paths: &[PathBuf]) -> Vec<PathBuf> {
    if !import_paths.is_empty() {
        return import_paths.to_vec();
    }
    let mut folders: Vec<PathBuf> = Vec::new();
    for folder in protos.iter().filter_map(|p| p.parent()) {
        if !folders.iter().any(|f| f == folder) {
            folders.push(folder.to_path_buf());
        }
    }
    folders
}

/// An import path whose files must really be inside it: a symlink that
/// points out of the folder is refused instead of followed.
struct ContainedInclude {
    folder: PathBuf,
    inner: IncludeFileResolver,
}

impl ContainedInclude {
    fn new(folder: PathBuf) -> Self {
        let inner = IncludeFileResolver::new(folder.clone());
        Self { folder, inner }
    }

    /// Whether `name` in this folder resolves inside it. A file that does
    /// not exist is left to the inner resolver (`file not found`).
    fn contains(&self, name: &str) -> bool {
        let Ok(real) = std::fs::canonicalize(self.folder.join(name)) else {
            return true;
        };
        std::fs::canonicalize(&self.folder).is_ok_and(|folder| real.starts_with(folder))
    }
}

impl FileResolver for ContainedInclude {
    fn resolve_path(&self, path: &Path) -> Option<String> {
        self.inner.resolve_path(path)
    }

    fn open_file(&self, name: &str) -> Result<File, protox::Error> {
        if !self.contains(name) {
            return Err(protox::Error::new(format!(
                "import '{name}' resolves outside its import path"
            )));
        }
        self.inner.open_file(name)
    }
}

/// `<file>:<line>:<col>: <message>` when the error points into a file,
/// `<file>: <message>` when it names one, else the message.
fn compile_error(error: &protox::Error) -> GrpcError {
    let message = message_of(error);
    let text = match (error.file(), location(error)) {
        (Some(file), Some((line, column))) => format!("{file}:{line}:{column}: {message}"),
        (Some(file), None) if !message.contains(file) => format!("{file}: {message}"),
        _ => message,
    };
    GrpcError::Compile(text)
}

/// The message of `error`. protox reports an import path it refuses
/// (absolute, or with `.` or `..` components) as "invalid group name";
/// its help text says what is wrong, so that is shown instead.
fn message_of(error: &protox::Error) -> String {
    let help = error.help().map(|h| h.to_string()).unwrap_or_default();
    if help.starts_with("imports may not") {
        return format!("invalid import path: {help} (an import cannot leave its import path)");
    }
    error.to_string()
}

/// The 1-based line and column of the first label of `error`. A token
/// that was expected but missing (`expected ';'`) is placed right after
/// the token before it, where it belongs, not at the next line's start.
fn location(error: &protox::Error) -> Option<(usize, usize)> {
    let offset = error.labels()?.next()?.offset();
    let source = error.source_code()?;
    let before = source
        .read_span(&SourceSpan::new(0.into(), offset), 0, 0)
        .ok()?;
    let before = String::from_utf8_lossy(before.data().get(..offset)?);
    let anchor = if error.to_string().starts_with("expected ") {
        before.trim_end()
    } else {
        &before
    };
    Some(line_and_column(anchor))
}

/// The 1-based line and column just after `text`.
fn line_and_column(text: &str) -> (usize, usize) {
    let line = text.matches('\n').count() + 1;
    let column = text.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_default_to_the_proto_folders() {
        let protos = [PathBuf::from("a/x.proto"), PathBuf::from("a/y.proto")];
        assert_eq!(includes(&protos, &[]), [PathBuf::from("a")]);
        let paths = [PathBuf::from("p")];
        assert_eq!(includes(&protos, &paths), paths);
    }

    #[test]
    fn descriptor_sets_over_the_limit_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("set.binpb");
        std::fs::write(&path, [1u8; 9]).unwrap();
        assert_eq!(read_capped(&path, 9).unwrap().len(), 9);
        let err = read_capped(&path, 8).unwrap_err();
        assert_eq!(err.to_string(), "larger than the limit of 8 bytes");
    }

    #[test]
    fn lines_and_columns_are_one_based() {
        assert_eq!(line_and_column(""), (1, 1));
        assert_eq!(line_and_column("ab"), (1, 3));
        assert_eq!(line_and_column("a\n  x = 2"), (2, 8));
        assert_eq!(line_and_column("a\r\nb"), (2, 2));
    }
}
