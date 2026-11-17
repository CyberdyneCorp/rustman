//! In-process proxies on `127.0.0.1`: an HTTP proxy (`CONNECT` tunnels and
//! absolute-form forwarding, optionally over TLS and with required
//! credentials) and a SOCKS5 proxy. Each records what it was asked for.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, copy_bidirectional};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use crate::certs::TestCerts;
use crate::server::{Tls, server_config};

type Log = Arc<Mutex<Vec<String>>>;

/// What an HTTP proxy demands.
#[derive(Clone, Default)]
pub struct HttpProxyOptions {
    /// The exact `Proxy-Authorization` value required, if any.
    pub require_auth: Option<String>,
    /// Serve TLS (as `localhost`, signed by the test CA).
    pub tls: Option<Arc<rustls::ServerConfig>>,
}

/// A running proxy; stops when dropped.
pub struct TestProxy {
    addr: SocketAddr,
    scheme: &'static str,
    log: Log,
    task: JoinHandle<()>,
}

impl Drop for TestProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl TestProxy {
    pub async fn http() -> Self {
        Self::http_with(HttpProxyOptions::default()).await
    }

    /// An HTTP proxy reached over TLS.
    pub async fn https(certs: &TestCerts) -> Self {
        let mut config = server_config(certs, Tls::Http1Only);
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Self::http_with(HttpProxyOptions {
            tls: Some(Arc::new(config)),
            ..HttpProxyOptions::default()
        })
        .await
    }

    pub async fn http_with(options: HttpProxyOptions) -> Self {
        let scheme = if options.tls.is_some() {
            "https"
        } else {
            "http"
        };
        Self::spawn(scheme, move |tcp, log| {
            let options = options.clone();
            async move {
                match &options.tls {
                    Some(config) => {
                        let acceptor = TlsAcceptor::from(config.clone());
                        if let Ok(tls) = acceptor.accept(tcp).await {
                            if tls.get_ref().1.peer_certificates().is_some() {
                                log.lock().unwrap().push("client certificate".into());
                            }
                            http_proxy(tls, &options, &log).await;
                        }
                    }
                    None => http_proxy(tcp, &options, &log).await,
                }
            }
        })
        .await
    }

    /// A SOCKS5 proxy; with `credentials`, it demands that user and password.
    pub async fn socks5(credentials: Option<(&str, &str)>) -> Self {
        let credentials = credentials.map(|(u, p)| (u.to_owned(), p.to_owned()));
        Self::spawn("socks5", move |tcp, log| {
            let credentials = credentials.clone();
            async move {
                let _ = socks5(tcp, credentials, &log).await;
            }
        })
        .await
    }

    async fn spawn<F, Fut>(scheme: &'static str, handle: F) -> Self
    where
        F: Fn(TcpStream, Log) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let log = Log::default();
        let shared = log.clone();
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                tokio::spawn(handle(tcp, shared.clone()));
            }
        });
        Self {
            addr,
            scheme,
            log,
            task,
        }
    }

    /// `<scheme>://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("{}://127.0.0.1:{}", self.scheme, self.addr.port())
    }

    /// `<scheme>://localhost:<port>`, for a proxy whose certificate names
    /// `localhost`.
    pub fn localhost_url(&self) -> String {
        format!("{}://localhost:{}", self.scheme, self.addr.port())
    }

    /// What the proxy was asked for, in order: HTTP request heads, or
    /// `SOCKS5 <host>:<port>` (prefixed by `auth <user>:<password>`).
    pub fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

async fn http_proxy<S>(mut client: S, options: &HttpProxyOptions, log: &Log)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let Some(head) = read_head(&mut client).await else {
        return;
    };
    log.lock().unwrap().push(head.clone());
    if let Some(required) = &options.require_auth {
        let expected = format!("proxy-authorization: {required}");
        if !head.lines().any(|l| l.eq_ignore_ascii_case(&expected)) {
            let _ = client
                .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                .await;
            return;
        }
    }
    let target = head
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    if head.starts_with("CONNECT ") {
        let Ok(mut upstream) = TcpStream::connect(&target).await else {
            let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
            return;
        };
        let _ = client
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .await;
        let _ = copy_bidirectional(&mut client, &mut upstream).await;
    } else {
        let uri: http::Uri = target.parse().unwrap();
        let authority = uri.authority().unwrap().as_str().to_owned();
        let mut upstream = TcpStream::connect(authority).await.unwrap();
        upstream.write_all(head.as_bytes()).await.unwrap();
        let _ = copy_bidirectional(&mut client, &mut upstream).await;
    }
}

/// The request head, through the blank line.
async fn read_head<S: AsyncRead + Unpin>(stream: &mut S) -> Option<String> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(stream.read_u8().await.ok()?);
    }
    String::from_utf8(head).ok()
}

async fn socks5(
    mut client: TcpStream,
    credentials: Option<(String, String)>,
    log: &Log,
) -> std::io::Result<()> {
    let mut greeting = [0; 2];
    client.read_exact(&mut greeting).await?;
    let mut methods = vec![0; usize::from(greeting[1])];
    client.read_exact(&mut methods).await?;
    let method = if credentials.is_some() { 2 } else { 0 };
    client.write_all(&[5, method]).await?;
    if let Some((user, password)) = credentials {
        let (got_user, got_password) = read_user_password(&mut client).await?;
        log.lock()
            .unwrap()
            .push(format!("auth {got_user}:{got_password}"));
        let ok = got_user == user && got_password == password;
        client.write_all(&[1, if ok { 0 } else { 1 }]).await?;
        if !ok {
            return Ok(());
        }
    }
    let target = read_socks_target(&mut client).await?;
    log.lock().unwrap().push(format!("SOCKS5 {target}"));
    let mut upstream = TcpStream::connect(&target).await?;
    client.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

async fn read_user_password(client: &mut TcpStream) -> std::io::Result<(String, String)> {
    let _version = client.read_u8().await?;
    let user = read_short_string(client).await?;
    let password = read_short_string(client).await?;
    Ok((user, password))
}

async fn read_short_string(client: &mut TcpStream) -> std::io::Result<String> {
    let len = client.read_u8().await?;
    let mut bytes = vec![0; usize::from(len)];
    client.read_exact(&mut bytes).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// `host:port` from a SOCKS5 CONNECT request.
async fn read_socks_target(client: &mut TcpStream) -> std::io::Result<String> {
    let mut header = [0; 4];
    client.read_exact(&mut header).await?;
    let host = match header[3] {
        1 => {
            let mut ip = [0; 4];
            client.read_exact(&mut ip).await?;
            std::net::Ipv4Addr::from(ip).to_string()
        }
        3 => read_short_string(client).await?,
        _ => {
            let mut ip = [0; 16];
            client.read_exact(&mut ip).await?;
            format!("[{}]", std::net::Ipv6Addr::from(ip))
        }
    };
    let port = client.read_u16().await?;
    Ok(format!("{host}:{port}"))
}
