//! `rustman record`: run the local recording proxy and print one redacted
//! line per capture (traffic-recorder spec: Forward proxy command,
//! Loopback by default, Interception scope); `rustman record ca ...`
//! manages the local CA used for interception.

mod ca;
mod save;
pub(crate) mod test_hooks;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, channel, sync_channel};
use std::time::Duration;

use clap::{Args, Subcommand};
use rustman_core::ConfigError;
use rustman_proxy::ca::LocalCa;
use rustman_proxy::config::remote_warning;
use rustman_proxy::{
    Capture, CaptureStore, FilterRules, Proxy, ProxyConfig, ProxyError, RecordSettings,
};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

use super::current_dir;
use super::exec::{cancel_on_ctrl_c, runtime};
use crate::exit::{CliError, SUCCESS};

/// Lines waiting for the output thread; more are dropped.
const WRITE_QUEUE: usize = 1024;

/// How long recording waits, once stopped, for queued lines to be printed.
const FLUSH_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct RecordArgs {
    #[command(subcommand)]
    command: Option<RecordCommand>,
    #[command(flatten)]
    run: RunArgs,
}

#[derive(Debug, Subcommand)]
enum RecordCommand {
    /// Manage this workspace's local CA, used to decrypt intercepted hosts
    Ca(ca::CaArgs),
}

#[derive(Debug, Args)]
struct RunArgs {
    /// Address to bind: an IP address or localhost (default: [record] host
    /// in rustman.toml, else 127.0.0.1)
    #[arg(long, value_name = "HOST")]
    host: Option<String>,
    /// Port to bind; 0 picks a free port (default: [record] port in
    /// rustman.toml, else 8888)
    #[arg(long, value_name = "PORT")]
    port: Option<u16>,
    /// Allow binding an address reachable from the network (anyone who can
    /// reach it can use the proxy and see intercepted traffic)
    #[arg(long)]
    allow_remote: bool,
    /// Decrypt HTTPS to hosts matching GLOB with the local CA (repeatable;
    /// default: [record] intercept); other hosts stay opaque tunnels
    #[arg(long, value_name = "GLOB")]
    intercept: Vec<String>,
    /// Workspace whose settings and CA to use (default: search from the
    /// current directory upwards; recording without interception works
    /// without one)
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
    #[command(flatten)]
    filter: FilterArgs,
    /// Print one redacted JSON object per capture instead of a summary line
    #[arg(long)]
    json: bool,
    /// Reverse-proxy mode: forward every request received to this origin
    /// (https://api.example.com), for clients that cannot use a proxy
    #[arg(long, value_name = "URL")]
    upstream: Option<String>,
    /// Do not verify the --upstream server's TLS certificate
    #[arg(long, requires = "upstream")]
    insecure: bool,
    #[command(flatten)]
    save: save::SaveArgs,
    /// When recording stops (Ctrl+C), write every capture to FILE as HAR
    /// 1.2, secrets redacted
    #[arg(long, value_name = "FILE")]
    har: Option<PathBuf>,
    /// Keep secret values in the --har file instead of redacting them
    #[arg(long, requires = "har")]
    show_secrets: bool,
}

/// Which exchanges are recorded; every request is still proxied.
#[derive(Debug, Args)]
struct FilterArgs {
    /// Record only hosts matching GLOB (repeatable)
    #[arg(long, value_name = "GLOB")]
    include_host: Vec<String>,
    /// Do not record hosts matching GLOB (repeatable)
    #[arg(long, value_name = "GLOB")]
    exclude_host: Vec<String>,
    /// Record only this method (repeatable)
    #[arg(long, value_name = "METHOD")]
    method: Vec<String>,
    /// Record only exchanges whose request or response content type starts
    /// with PREFIX (repeatable)
    #[arg(long, value_name = "PREFIX")]
    content_type: Vec<String>,
    /// Also record static assets (images, fonts, CSS, JavaScript, media,
    /// source maps), dropped by default
    #[arg(long)]
    keep_static: bool,
}

impl FilterArgs {
    fn rules(&self) -> FilterRules {
        FilterRules {
            include_hosts: self.include_host.clone(),
            exclude_hosts: self.exclude_host.clone(),
            methods: self.method.clone(),
            content_types: self.content_type.clone(),
            keep_static: self.keep_static,
        }
    }
}

pub fn run(args: &RecordArgs) -> Result<u8, CliError> {
    match &args.command {
        Some(RecordCommand::Ca(ca)) => ca::run(ca),
        None => record(&args.run),
    }
}

/// Proxy errors are configuration errors (exit code 2).
fn proxy_error(err: ProxyError) -> CliError {
    ConfigError::Usage(err.to_string()).into()
}

fn record(args: &RunArgs) -> Result<u8, CliError> {
    let root = workspace_root(args.workspace.as_deref())?;
    let settings = match &root {
        Some(root) => RecordSettings::load(root).map_err(|e| ConfigError::Invalid(vec![e]))?,
        None => RecordSettings::default(),
    };
    let upstream_tls = match &root {
        Some(root) => {
            rustman_proxy::config::workspace_upstream_tls(root).map_err(ConfigError::Usage)?
        }
        None => rustman_http::TlsSettings::default(),
    };
    for warning in settings.warnings(!args.intercept.is_empty()) {
        eprintln!("{warning}");
    }
    let config = proxy_config(args, &settings, upstream_tls);
    // Refuse a network-reachable address before touching the keychain.
    config.bind_addr().map_err(|e| proxy_error(e.into()))?;
    let saving = args.save.prepare(root.as_deref())?;
    let ca = local_ca(&config, root.as_deref())?;
    if args.insecure {
        eprintln!("warning: the --upstream server's TLS certificate is not verified");
    }
    let store = runtime()?.block_on(serve(config, ca, args.json))?;
    if let Some(path) = &args.har {
        write_har(path, &store.snapshot(), args.show_secrets, args.json)?;
    }
    match saving {
        Some(saving) => saving.save(&store.snapshot(), args.json),
        None => Ok(SUCCESS),
    }
}

/// Write the session's captures as HAR (spec: HAR export).
fn write_har(
    path: &Path,
    captures: &[Arc<Capture>],
    show_secrets: bool,
    json: bool,
) -> Result<(), CliError> {
    let path = current_dir()?.join(path);
    rustman_proxy::har::write_har(&path, captures, show_secrets)
        .map_err(|e| ConfigError::Usage(format!("record: cannot write {}: {e}", path.display())))?;
    let line = format!("Wrote {} captures to {}", captures.len(), path.display());
    if json {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
    Ok(())
}

/// Flags first, then `[record]`, then the defaults; `upstream_tls` is the
/// workspace's (`[http] ca_bundle`).
fn proxy_config(
    args: &RunArgs,
    settings: &RecordSettings,
    upstream_tls: rustman_http::TlsSettings,
) -> ProxyConfig {
    let base = settings.proxy_config();
    let intercept = if args.intercept.is_empty() {
        base.intercept.clone()
    } else {
        args.intercept.clone()
    };
    let upstream_tls = rustman_http::TlsSettings {
        verify: !args.insecure,
        ..upstream_tls
    };
    ProxyConfig {
        host: args.host.clone().unwrap_or_else(|| base.host.clone()),
        port: args.port.unwrap_or(base.port),
        allow_remote: args.allow_remote,
        intercept,
        upstream_tls,
        filter: args.filter.rules(),
        upstream: args.upstream.clone(),
        ..base
    }
}

/// The CA interception needs: loaded, or created after the prompt.
fn local_ca(config: &ProxyConfig, root: Option<&Path>) -> Result<Option<LocalCa>, CliError> {
    if config.intercept.is_empty() {
        return Ok(None);
    }
    let root = root.ok_or_else(|| {
        ConfigError::Usage(
            "record: --intercept needs a workspace (the local CA belongs to one); \
             run 'rustman init' or pass --workspace"
                .to_owned(),
        )
    })?;
    if let Some(store) = ca::existing_store(root)?
        && let Some(found) = store.load().map_err(ca::ca_error)?
    {
        return Ok(Some(found));
    }
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if interactive && ca::confirmed("This workspace has no local CA. Type yes to create one: ") {
        return ca::create(&ca::new_store(root)?).map(Some);
    }
    Err(proxy_error(ProxyError::NoCa))
}

/// Run the proxy until Ctrl+C; returns the capture store.
async fn serve(
    config: ProxyConfig,
    ca: Option<LocalCa>,
    json: bool,
) -> Result<CaptureStore, CliError> {
    let store = CaptureStore::new(config.max_captures, config.max_buffer_bytes);
    let mut live = store.subscribe();
    let proxy = Proxy::bind(&config, store.clone(), ca.as_ref())
        .await
        .map_err(proxy_error)?;
    let addr = proxy.local_addr();
    // Listen for Ctrl+C before the banner: whoever reads it may interrupt.
    let cancel = CancellationToken::new();
    cancel_on_ctrl_c(&cancel);
    print_banner(addr, config.upstream.as_deref(), json);
    if !addr.ip().is_loopback() {
        eprintln!("{}", remote_warning(addr));
    }
    let _ = std::io::stdout().flush();
    let mut writer = Writer::spawn(std::io::stdout(), WRITE_QUEUE);
    tokio::spawn(proxy.run(cancel.clone()));
    let stop_after = test_hooks::stop_after();
    let mut shown = 0;
    loop {
        let capture = tokio::select! {
            () = cancel.cancelled() => break,
            capture = live.recv() => capture,
        };
        match capture {
            Ok(capture) => {
                if let Some(warning) = writer.offer(line(&capture, json)) {
                    eprintln!("{warning}");
                }
                shown += 1;
                if stop_after == Some(shown) {
                    break;
                }
            }
            Err(RecvError::Lagged(skipped)) => {
                eprintln!("warning: {skipped} captures not shown");
            }
            Err(RecvError::Closed) => break,
        }
    }
    cancel.cancel();
    if let Some(warning) = writer.finish(FLUSH_WAIT) {
        eprintln!("{warning}");
    }
    Ok(store)
}

/// Prints lines on a thread of its own, so a blocked stdout never stalls
/// the proxy; lines that do not fit its queue are dropped and counted.
struct Writer {
    queue: Option<SyncSender<String>>,
    /// Signalled when the thread has written every queued line.
    done: Receiver<()>,
    /// Lines dropped since the last warning.
    dropped: u64,
}

impl Writer {
    fn spawn<W: Write + Send + 'static>(mut out: W, capacity: usize) -> Self {
        let (queue, lines) = sync_channel::<String>(capacity);
        let (finished, done) = channel();
        std::thread::Builder::new()
            .name("record-output".into())
            .spawn(move || {
                for line in lines {
                    if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
                        break;
                    }
                }
                let _ = finished.send(());
            })
            .expect("cannot start the output thread");
        Self {
            queue: Some(queue),
            done,
            dropped: 0,
        }
    }

    /// Queue `line`. A full queue drops it and counts it; the next line
    /// that fits returns the warning for the gap.
    fn offer(&mut self, line: String) -> Option<String> {
        let queue = self.queue.as_ref()?;
        match queue.try_send(line) {
            Ok(()) if self.dropped > 0 => Some(not_shown(std::mem::take(&mut self.dropped))),
            Ok(()) | Err(TrySendError::Disconnected(_)) => None,
            Err(TrySendError::Full(_)) => {
                self.dropped += 1;
                None
            }
        }
    }

    /// Stop queueing and wait, at most `wait`, until the queued lines are
    /// written; the warning for lines dropped since the last one.
    fn finish(mut self, wait: Duration) -> Option<String> {
        drop(self.queue.take());
        let _ = self.done.recv_timeout(wait);
        (self.dropped > 0).then(|| not_shown(self.dropped))
    }
}

fn not_shown(dropped: u64) -> String {
    format!("warning: {dropped} captures not shown (output too slow)")
}

/// The proxy URL and exports, on stderr with `--json` so stdout holds only
/// JSON lines.
fn print_banner(addr: std::net::SocketAddr, upstream: Option<&str>, json: bool) {
    let lines = match upstream {
        Some(upstream) => vec![format!(
            "Recording reverse proxy on http://{addr} -> {upstream}"
        )],
        None => rustman_proxy::banner(addr).to_vec(),
    };
    for line in lines {
        if json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }
}

/// The summary line of `capture`, or its JSON object; redacted either way.
fn line(capture: &Arc<Capture>, json: bool) -> String {
    if json {
        return rustman_proxy::jsonl::capture_line(capture);
    }
    capture.redactor().text(&capture.to_string()).into_owned()
}

/// The workspace given with `--workspace`, or found from the current
/// directory; recording works without one.
fn workspace_root(explicit: Option<&Path>) -> Result<Option<PathBuf>, ConfigError> {
    match rustman_core::discovery::find_root(&current_dir()?, explicit) {
        Ok(root) => Ok(Some(root)),
        Err(ConfigError::NoWorkspace) if explicit.is_none() => Ok(None),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::Writer;

    /// An output whose first write waits for `gate`, after telling
    /// `entered`: the queue then fills while it is blocked.
    struct Gated {
        out: Arc<Mutex<Vec<u8>>>,
        entered: Sender<()>,
        gate: Option<Receiver<()>>,
    }

    impl Write for Gated {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if let Some(gate) = self.gate.take() {
                let _ = self.entered.send(());
                let _ = gate.recv();
            }
            self.out.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A gated output, what it wrote, its "entered" signal and its gate.
    struct Harness {
        gated: Gated,
        out: Arc<Mutex<Vec<u8>>>,
        entered: Receiver<()>,
        open: Sender<()>,
    }

    fn gated() -> Harness {
        let out = Arc::default();
        let (entered, on_enter) = channel();
        let (open, gate) = channel();
        let gated = Gated {
            out: Arc::clone(&out),
            entered,
            gate: Some(gate),
        };
        Harness {
            gated,
            out,
            entered: on_enter,
            open,
        }
    }

    #[test]
    fn lines_dropped_on_a_full_queue_are_counted_and_reported_by_the_next_one() {
        let Harness {
            gated,
            out,
            entered,
            open,
        } = gated();
        let mut writer = Writer::spawn(gated, 1);
        assert_eq!(writer.offer("a".into()), None);
        entered.recv().unwrap();
        assert_eq!(writer.offer("b".into()), None, "fits the queue");
        assert_eq!(writer.offer("c".into()), None);
        assert_eq!(writer.offer("d".into()), None);
        assert_eq!(writer.dropped, 2);
        open.send(()).unwrap();
        while writer.offer("e".into()).is_none() {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(writer.dropped, 0, "the warning resets the count");
        assert_eq!(writer.finish(Duration::from_secs(5)), None);
        assert_eq!(
            String::from_utf8(out.lock().unwrap().clone()).unwrap(),
            "a\nb\ne\n"
        );
    }

    #[test]
    fn finishing_prints_the_queued_lines_and_warns_about_the_dropped_ones() {
        let Harness {
            gated,
            out,
            entered,
            open,
        } = gated();
        let mut writer = Writer::spawn(gated, 1);
        writer.offer("a".into());
        entered.recv().unwrap();
        writer.offer("b".into());
        writer.offer("c".into());
        open.send(()).unwrap();
        assert_eq!(
            writer.finish(Duration::from_secs(5)).as_deref(),
            Some("warning: 1 captures not shown (output too slow)")
        );
        let written = String::from_utf8(out.lock().unwrap().clone()).unwrap();
        assert_eq!(written, "a\nb\n", "queued lines are not lost");
    }
}
