//! Reading a response body: decompression, the in-memory cap with spill to
//! disk, and binary detection.
//!
//! Bodies up to `max_in_memory` bytes stay in memory. A larger body is
//! streamed to `<spill_dir>/<exchange-id>.bin`, and only its first
//! [`PREVIEW_BYTES`] are kept. Decoders are read through a fixed buffer, so a
//! highly compressed body never decodes into memory at once, and a decoded
//! body stops at `max_decoded`; a body kept as received stops at
//! `max_download`. A spill file is removed when the body is not read to the
//! end (an error, a timeout or a cancelled request).

use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use async_compression::tokio::bufread::{BrotliDecoder, GzipDecoder, ZlibDecoder, ZstdDecoder};
use bytes::{Buf, Bytes, BytesMut};
use http::HeaderMap;
use http::header::CONTENT_ENCODING;
use hyper::body::{Body, Incoming};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, ReadBuf};

use crate::error::RequestError;
use crate::transport::network_error;

/// Where spilled bodies go, relative to the workspace root.
pub const BODIES_DIR: &str = ".rustman/bodies";

/// The default `[http] max_body_bytes`: 10 MiB.
pub const DEFAULT_MAX_BODY_BYTES: u64 = 10 * 1024 * 1024;

/// The default limit of a decompressed body: 1 GiB.
pub const DEFAULT_MAX_DECODED_BYTES: u64 = 1024 * 1024 * 1024;

/// The default limit of a body that is not decoded: 1 GiB.
pub const DEFAULT_MAX_DOWNLOAD_BYTES: u64 = 1024 * 1024 * 1024;

/// The folder bodies spill to outside a workspace, in the temp folder:
/// `rustman-bodies-<uid>` on Unix, `rustman-bodies` on Windows (whose temp
/// folder is per user).
pub fn default_spill_dir() -> PathBuf {
    rustman_core::secrets::user_temp_dir("rustman-bodies")
}

/// How much of a spilled body the exchange keeps in memory.
pub const PREVIEW_BYTES: usize = 64 * 1024;

/// The buffer bodies are read through.
const READ_BUFFER: usize = 64 * 1024;

/// How response bodies are read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyOptions {
    /// Bodies larger than this are spilled to disk.
    pub max_in_memory: u64,
    /// The folder spilled bodies are written to. It must be a real folder of
    /// the current user (see [`rustman_core::secrets::ensure_private_dir`]).
    pub spill_dir: PathBuf,
    /// The workspace root when `spill_dir` is its [`BODIES_DIR`]: before the
    /// first spill, `.rustman/` is checked to be a real, git-ignored folder.
    pub workspace: Option<PathBuf>,
    /// Decode `Content-Encoding: gzip|br|deflate|zstd`.
    pub decompress: bool,
    /// A decompressed body larger than this is an error (a decompression
    /// bomb guard).
    pub max_decoded: u64,
    /// A body that is not decoded (no `Content-Encoding`, or `decompress`
    /// off) larger than this is an error, so an endless stream cannot fill
    /// the disk.
    pub max_download: u64,
}

impl Default for BodyOptions {
    fn default() -> Self {
        Self {
            max_in_memory: DEFAULT_MAX_BODY_BYTES,
            spill_dir: default_spill_dir(),
            workspace: None,
            decompress: true,
            max_decoded: DEFAULT_MAX_DECODED_BYTES,
            max_download: DEFAULT_MAX_DOWNLOAD_BYTES,
        }
    }
}

/// A response body as received (after decoding).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReadBody {
    /// The whole body, or its first [`PREVIEW_BYTES`] when spilled.
    pub bytes: Bytes,
    pub size: u64,
    pub spill: Option<PathBuf>,
    pub binary: bool,
    pub warnings: Vec<String>,
}

/// Read `body` to the end. `id` names the spill file.
pub(crate) async fn read(
    body: Incoming,
    headers: &HeaderMap,
    options: &BodyOptions,
    id: &str,
) -> Result<ReadBody, RequestError> {
    let mut warnings = Vec::new();
    let encoding = encoding(headers, options.decompress, &mut warnings);
    let spill_path = options.spill_dir.join(format!("{id}.bin"));
    let mut sink = Sink::new(options.max_in_memory, spill_path);
    sink.workspace.clone_from(&options.workspace);
    let mut source = BodyReader::new(body);
    decode(&mut source, encoding, options, &mut sink).await?;
    let mut read = sink.finish().await?;
    read.warnings = warnings;
    Ok(read)
}

/// The encodings Rustman decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encoding {
    Gzip,
    Brotli,
    Deflate,
    Zstd,
}

impl Encoding {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "gzip" | "x-gzip" => Some(Self::Gzip),
            "br" => Some(Self::Brotli),
            "deflate" => Some(Self::Deflate),
            "zstd" => Some(Self::Zstd),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Gzip => "gzip",
            Self::Brotli => "br",
            Self::Deflate => "deflate",
            Self::Zstd => "zstd",
        }
    }

    fn decoder<'a>(self, source: &'a mut BodyReader) -> Box<dyn AsyncRead + Send + Unpin + 'a> {
        match self {
            Self::Gzip => Box::new(GzipDecoder::new(source)),
            Self::Brotli => Box::new(BrotliDecoder::new(source)),
            Self::Deflate => Box::new(ZlibDecoder::new(source)),
            Self::Zstd => Box::new(ZstdDecoder::new(source)),
        }
    }
}

/// The response's `Content-Encoding`, if it is one we decode.
fn encoding(headers: &HeaderMap, decompress: bool, warnings: &mut Vec<String>) -> Option<Encoding> {
    let name = headers.get(CONTENT_ENCODING)?.to_str().unwrap_or_default();
    let name = name.trim().to_ascii_lowercase();
    if !decompress || name.is_empty() || name == "identity" {
        return None;
    }
    let encoding = Encoding::parse(&name);
    if encoding.is_none() {
        warnings.push(format!(
            "the body was left encoded: unsupported Content-Encoding '{name}'"
        ));
    }
    encoding
}

/// Copy `source` into `sink`, through the decoder for `encoding`. An empty
/// body is never decoded.
async fn decode(
    source: &mut BodyReader,
    encoding: Option<Encoding>,
    options: &BodyOptions,
    sink: &mut Sink,
) -> Result<(), RequestError> {
    let empty = source.fill_buf().await.map(<[u8]>::is_empty);
    let encoding = match (empty, encoding) {
        (Ok(false), Some(encoding)) => encoding,
        _ => {
            let copied = pump(&mut *source, Limit::raw(options.max_download), sink).await;
            return copied.map_err(|e| e.failure(source));
        }
    };
    let limit = Limit::decoded(options.max_decoded);
    let decoded = pump(encoding.decoder(&mut *source), limit, sink).await;
    decoded.map_err(|e| match e {
        PumpError::Read(err) if source.failure.is_none() => RequestError::Network(format!(
            "network: cannot decode the {} response body: {err}",
            encoding.name()
        )),
        other => other.failure(source),
    })
}

/// Why copying a body stopped.
enum PumpError {
    /// Reading failed: the connection (see [`BodyReader::failure`]) or the
    /// decoder.
    Read(io::Error),
    Sink(RequestError),
}

impl PumpError {
    fn failure(self, source: &mut BodyReader) -> RequestError {
        match self {
            Self::Read(err) => source
                .failure
                .take()
                .unwrap_or_else(|| RequestError::Network(format!("network: {err}"))),
            Self::Sink(err) => err,
        }
    }
}

/// The most bytes a body may have, as received or once decoded.
#[derive(Clone, Copy, Debug)]
struct Limit {
    bytes: u64,
    decoded: bool,
}

impl Limit {
    fn raw(bytes: u64) -> Self {
        Self {
            bytes,
            decoded: false,
        }
    }

    fn decoded(bytes: u64) -> Self {
        Self {
            bytes,
            decoded: true,
        }
    }

    fn error(self) -> RequestError {
        let kind = if self.decoded { "decompressed " } else { "" };
        RequestError::Network(format!(
            "network: the {kind}response body is larger than {} bytes",
            self.bytes
        ))
    }
}

/// Read `reader` to the end into `sink`, failing past the limit.
async fn pump(
    mut reader: impl AsyncRead + Unpin,
    limit: Limit,
    sink: &mut Sink,
) -> Result<(), PumpError> {
    let mut buffer = vec![0u8; READ_BUFFER];
    loop {
        let n = reader.read(&mut buffer).await.map_err(PumpError::Read)?;
        if n == 0 {
            return Ok(());
        }
        if sink.size + n as u64 > limit.bytes {
            return Err(PumpError::Sink(limit.error()));
        }
        let chunk = Bytes::copy_from_slice(&buffer[..n]);
        sink.push(chunk).await.map_err(PumpError::Sink)?;
    }
}

/// The response body as an `AsyncBufRead`. A connection error is kept in
/// `failure`, since the decoders only pass an `io::Error` on.
struct BodyReader {
    body: Incoming,
    chunk: Bytes,
    failure: Option<RequestError>,
}

impl BodyReader {
    fn new(body: Incoming) -> Self {
        Self {
            body,
            chunk: Bytes::new(),
            failure: None,
        }
    }
}

impl AsyncBufRead for BodyReader {
    fn poll_fill_buf(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<&[u8]>> {
        let this = self.get_mut();
        while this.chunk.is_empty() {
            match ready!(Pin::new(&mut this.body).poll_frame(cx)) {
                None => break,
                Some(Ok(frame)) => this.chunk = frame.into_data().unwrap_or_default(),
                Some(Err(err)) => {
                    let failure = network_error(&err);
                    let message = failure.to_string();
                    this.failure = Some(failure);
                    return Poll::Ready(Err(io::Error::other(message)));
                }
            }
        }
        Poll::Ready(Ok(&this.chunk))
    }

    fn consume(self: Pin<&mut Self>, amt: usize) {
        self.get_mut().chunk.advance(amt);
    }
}

impl AsyncRead for BodyReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let available = ready!(self.as_mut().poll_fill_buf(cx))?;
        let n = available.len().min(buf.remaining());
        buf.put_slice(&available[..n]);
        self.consume(n);
        Poll::Ready(Ok(()))
    }
}

/// Where body bytes go: memory up to the cap, then a spill file. A spill
/// file still open when the sink is dropped (the body was not read to the
/// end) is removed.
struct Sink {
    limit: u64,
    chunks: Vec<Bytes>,
    size: u64,
    utf8: Utf8Check,
    spill_path: PathBuf,
    workspace: Option<PathBuf>,
    file: Option<tokio::fs::File>,
    preview: BytesMut,
}

impl Sink {
    fn new(limit: u64, spill_path: PathBuf) -> Self {
        Self {
            limit,
            chunks: Vec::new(),
            size: 0,
            utf8: Utf8Check::default(),
            spill_path,
            workspace: None,
            file: None,
            preview: BytesMut::new(),
        }
    }

    async fn push(&mut self, data: Bytes) -> Result<(), RequestError> {
        if data.is_empty() {
            return Ok(());
        }
        self.size += data.len() as u64;
        self.utf8.feed(&data);
        if let Some(file) = &mut self.file {
            return write(file, &data, &self.spill_path).await;
        }
        self.chunks.push(data);
        if self.size > self.limit {
            self.spill().await?;
        }
        Ok(())
    }

    /// Move the body so far to the spill file and keep only its preview.
    async fn spill(&mut self) -> Result<(), RequestError> {
        let file = create_spill_file(&self.spill_path, self.workspace.clone()).await?;
        let file = self.file.insert(file);
        for chunk in std::mem::take(&mut self.chunks) {
            let room = PREVIEW_BYTES.saturating_sub(self.preview.len());
            self.preview
                .extend_from_slice(&chunk[..room.min(chunk.len())]);
            write(file, &chunk, &self.spill_path).await?;
        }
        Ok(())
    }

    async fn finish(mut self) -> Result<ReadBody, RequestError> {
        let binary = !self.utf8.is_valid();
        let Some(file) = &mut self.file else {
            return Ok(ReadBody {
                bytes: concat(std::mem::take(&mut self.chunks), self.size),
                size: self.size,
                spill: None,
                binary,
                warnings: Vec::new(),
            });
        };
        file.flush()
            .await
            .map_err(|e| io_error(&self.spill_path, &e))?;
        self.file = None;
        Ok(ReadBody {
            bytes: std::mem::take(&mut self.preview).freeze(),
            size: self.size,
            spill: Some(self.spill_path.clone()),
            binary,
            warnings: Vec::new(),
        })
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        if self.file.take().is_some() {
            // Best effort: a file that cannot be removed is only left behind.
            let _ = std::fs::remove_file(&self.spill_path);
        }
    }
}

async fn write(file: &mut tokio::fs::File, data: &[u8], path: &Path) -> Result<(), RequestError> {
    file.write_all(data).await.map_err(|e| io_error(path, &e))
}

/// Create `path`, readable by the current user only, in a private folder of
/// the current user. With a `workspace`, its `.rustman/` must first pass the
/// private-state checks (no symbolic link, git-ignored). An existing file is
/// never opened: `path` is created new.
async fn create_spill_file(
    path: &Path,
    workspace: Option<PathBuf>,
) -> Result<tokio::fs::File, RequestError> {
    let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    tokio::task::spawn_blocking(move || prepare_spill_dir(&dir, workspace.as_deref()))
        .await
        .map_err(|e| io_error(path, &e))?
        .map_err(|e| io_error(path, &e))?;
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path).await.map_err(|e| io_error(path, &e))
}

fn prepare_spill_dir(dir: &Path, workspace: Option<&Path>) -> Result<(), String> {
    if let Some(root) = workspace {
        rustman_core::secrets::ensure_private(root, BODIES_DIR).map_err(|e| e.to_string())?;
    }
    rustman_core::secrets::ensure_private_dir(dir).map_err(|e| e.to_string())
}

fn io_error(path: &Path, err: &dyn std::fmt::Display) -> RequestError {
    RequestError::Io(format!(
        "cannot write the response body to {}: {err}",
        path.display()
    ))
}

fn concat(chunks: Vec<Bytes>, size: u64) -> Bytes {
    if chunks.len() == 1 {
        return chunks.into_iter().next().unwrap_or_default();
    }
    let mut all = BytesMut::with_capacity(usize::try_from(size).unwrap_or_default());
    for chunk in chunks {
        all.extend_from_slice(&chunk);
    }
    all.freeze()
}

/// Incremental UTF-8 validation across chunk boundaries.
#[derive(Debug, Default)]
struct Utf8Check {
    invalid: bool,
    /// The start of a character split across chunks (at most 3 bytes).
    pending: Vec<u8>,
}

impl Utf8Check {
    fn feed(&mut self, mut data: &[u8]) {
        while !self.invalid && !self.pending.is_empty() && !data.is_empty() {
            self.pending.push(data[0]);
            data = &data[1..];
            self.check_pending();
        }
        if self.invalid || !self.pending.is_empty() {
            return;
        }
        if let Err(e) = std::str::from_utf8(data) {
            match e.error_len() {
                Some(_) => self.invalid = true,
                None => self.pending = data[e.valid_up_to()..].to_vec(),
            }
        }
    }

    fn check_pending(&mut self) {
        match std::str::from_utf8(&self.pending) {
            Ok(_) => self.pending.clear(),
            Err(e) if e.error_len().is_some() => self.invalid = true,
            Err(_) => {}
        }
    }

    fn is_valid(&self) -> bool {
        !self.invalid && self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_in_chunks(chunks: &[&[u8]]) -> bool {
        let mut check = Utf8Check::default();
        for chunk in chunks {
            check.feed(chunk);
        }
        check.is_valid()
    }

    #[test]
    fn utf8_split_across_chunks_is_valid() {
        let text = "a€b😀".as_bytes();
        for split in 0..text.len() {
            let (a, b) = text.split_at(split);
            assert!(valid_in_chunks(&[a, b]), "split at {split}");
        }
        let one_byte_at_a_time: Vec<&[u8]> = text.chunks(1).collect();
        assert!(valid_in_chunks(&one_byte_at_a_time));
    }

    #[test]
    fn invalid_or_truncated_utf8_is_binary() {
        assert!(!valid_in_chunks(&[b"ok", &[0xff], b"ok"]));
        assert!(!valid_in_chunks(&["€".as_bytes()[..2].as_ref()]));
        assert!(!valid_in_chunks(&[&[0xe2], &[0x41]]));
        assert!(valid_in_chunks(&[]));
    }

    #[tokio::test]
    async fn the_sink_spills_past_the_limit_and_keeps_a_preview() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bodies/x.bin");
        let mut sink = Sink::new(100_000, path.clone());
        let chunk = Bytes::from(vec![b'a'; 40_000]);
        for _ in 0..4 {
            sink.push(chunk.clone()).await.unwrap();
        }
        let read = sink.finish().await.unwrap();
        assert_eq!(read.size, 160_000);
        assert_eq!(read.bytes.len(), PREVIEW_BYTES);
        assert_eq!(read.spill.as_deref(), Some(path.as_path()));
        assert_eq!(std::fs::read(&path).unwrap().len(), 160_000);
        assert!(!read.binary);
    }

    #[tokio::test]
    async fn the_sink_keeps_small_bodies_in_memory() {
        let mut sink = Sink::new(10, PathBuf::from("unused"));
        sink.push(Bytes::from_static(b"hello ")).await.unwrap();
        sink.push(Bytes::from_static(b"you")).await.unwrap();
        let read = sink.finish().await.unwrap();
        assert_eq!(&read.bytes[..], b"hello you");
        assert_eq!((read.size, read.spill), (9, None));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spill_files_and_their_folder_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(BODIES_DIR).join("x.bin");
        let mut sink = Sink::new(1, path.clone());
        sink.workspace = Some(dir.path().to_path_buf());
        sink.push(Bytes::from_static(b"ab")).await.unwrap();
        let read = sink.finish().await.unwrap();
        assert_eq!(read.spill.as_deref(), Some(path.as_path()));
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_bodies_folder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".rustman")).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), dir.path().join(BODIES_DIR)).unwrap();
        let mut sink = Sink::new(1, dir.path().join(BODIES_DIR).join("x.bin"));
        sink.workspace = Some(dir.path().to_path_buf());
        let err = sink.push(Bytes::from_static(b"ab")).await.unwrap_err();
        assert!(err.to_string().contains("symbolic link"), "{err}");
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_spill_dir_outside_a_workspace_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let link = dir.path().join("bodies");
        std::os::unix::fs::symlink(elsewhere.path(), &link).unwrap();
        let mut sink = Sink::new(1, link.join("x.bin"));
        let err = sink.push(Bytes::from_static(b"ab")).await.unwrap_err();
        assert!(err.to_string().contains("symbolic link"), "{err}");
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn an_existing_spill_file_is_never_opened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.bin");
        std::fs::write(&path, "planted").unwrap();
        let mut sink = Sink::new(1, path.clone());
        let err = sink.push(Bytes::from_static(b"ab")).await.unwrap_err();
        assert!(
            err.to_string()
                .starts_with("cannot write the response body to"),
            "{err}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"planted");
    }

    #[test]
    fn the_default_spill_dir_is_private_to_the_user() {
        let dir = BodyOptions::default().spill_dir;
        assert_eq!(dir, default_spill_dir());
        assert_eq!(dir.parent(), Some(std::env::temp_dir().as_path()));
        #[cfg(unix)]
        assert_ne!(dir.file_name().unwrap(), "rustman-bodies");
    }

    #[tokio::test]
    async fn an_unwritable_spill_dir_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, "x").unwrap();
        let mut sink = Sink::new(1, blocker.join("x.bin"));
        let err = sink.push(Bytes::from_static(b"ab")).await.unwrap_err();
        assert!(
            err.to_string()
                .starts_with("cannot write the response body to"),
            "{err}"
        );
    }

    #[test]
    fn unknown_encodings_are_left_alone_with_a_warning() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "compress".parse().unwrap());
        let mut warnings = Vec::new();
        assert!(encoding(&headers, true, &mut warnings).is_none());
        assert_eq!(
            warnings,
            ["the body was left encoded: unsupported Content-Encoding 'compress'"]
        );
        headers.insert(CONTENT_ENCODING, "GZip".parse().unwrap());
        let mut warnings = Vec::new();
        assert!(encoding(&headers, false, &mut warnings).is_none());
        assert_eq!(
            encoding(&headers, true, &mut warnings),
            Some(Encoding::Gzip)
        );
        assert!(warnings.is_empty());
    }

    async fn zstd_zeros(len: u64) -> Vec<u8> {
        use async_compression::tokio::bufread::ZstdEncoder;
        let zeros = tokio::io::BufReader::new(tokio::io::repeat(0).take(len));
        let mut out = Vec::new();
        ZstdEncoder::new(zeros).read_to_end(&mut out).await.unwrap();
        out
    }

    #[tokio::test]
    async fn decoded_output_is_bounded_by_the_limit() {
        let bomb = zstd_zeros(64 * 1024 * 1024).await;
        assert!(bomb.len() < 64 * 1024, "{}", bomb.len());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bomb.bin");
        let mut sink = Sink::new(1024, path.clone());
        let decoder = ZstdDecoder::new(&bomb[..]);
        let Err(PumpError::Sink(err)) = pump(decoder, Limit::decoded(1_000_000), &mut sink).await
        else {
            panic!("the limit was not enforced");
        };
        assert_eq!(
            err.to_string(),
            "network: the decompressed response body is larger than 1000000 bytes"
        );
        assert!(sink.size <= 1_000_000);
        assert!(path.exists());
        drop(sink);
        assert!(!path.exists(), "the spill file of a failed body is removed");
    }

    #[tokio::test]
    async fn decoders_are_read_through_a_fixed_buffer() {
        let compressed = zstd_zeros(3 * READ_BUFFER as u64).await;
        let mut sink = Sink::new(u64::MAX, PathBuf::from("unused"));
        let decoder = ZstdDecoder::new(&compressed[..]);
        assert!(pump(decoder, Limit::raw(u64::MAX), &mut sink).await.is_ok());
        assert!(sink.chunks.len() >= 3);
        assert!(sink.chunks.iter().all(|c| c.len() <= READ_BUFFER));
        assert_eq!(sink.size, 3 * READ_BUFFER as u64);
    }

    #[tokio::test]
    async fn corrupt_compressed_input_is_a_read_error() {
        let mut sink = Sink::new(u64::MAX, PathBuf::from("unused"));
        let decoder = GzipDecoder::new(&b"not gzip at all"[..]);
        assert!(matches!(
            pump(decoder, Limit::raw(u64::MAX), &mut sink).await,
            Err(PumpError::Read(_))
        ));
    }

    #[tokio::test]
    async fn a_finished_spill_file_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kept.bin");
        let mut sink = Sink::new(1, path.clone());
        sink.push(Bytes::from_static(b"abc")).await.unwrap();
        let read = sink.finish().await.unwrap();
        assert_eq!(read.spill.as_deref(), Some(path.as_path()));
        assert_eq!(std::fs::read(&path).unwrap(), b"abc");
    }
}
