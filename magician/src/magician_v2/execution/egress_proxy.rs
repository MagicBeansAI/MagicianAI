//! The shell's egress proxy (`execution.network_policy: public`).
//!
//! A sandboxed shell may connect only to this proxy's localhost port
//! (`shell_box`), and gets `HTTP(S)_PROXY`/`ALL_PROXY` pointing at it. The
//! proxy resolves each destination itself, refuses one whose addresses are not
//! all public (`network_policy::resolve_checked`), and connects to an address
//! it checked, so a DNS answer cannot rebind it.
//!
//! It speaks `CONNECT host:port` (HTTPS, any TLS tool) and absolute-form plain
//! HTTP (`GET http://host/path`), forwarded with `Connection: close` so one
//! connection never reaches a second, unchecked host. It runs on its own thread
//! and runtime, started on first use, so it outlives any one request's runtime.

use std::sync::OnceLock;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const MAX_HEAD_BYTES: usize = 32 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);

/// The proxy's port, starting it on first use. `None` when it could not
/// start; the shell box then denies network rather than leaving it open.
pub fn port() -> Option<u16> {
    static PORT: OnceLock<Option<u16>> = OnceLock::new();
    *PORT.get_or_init(start)
}

fn start() -> Option<u16> {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("egress-proxy".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::error!(%error, "[EGRESS] proxy runtime did not start");
                    let _ = tx.send(None);
                    return;
                },
            };
            runtime.block_on(async move {
                let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
                    Ok(listener) => listener,
                    Err(error) => {
                        tracing::error!(%error, "[EGRESS] proxy could not bind");
                        let _ = tx.send(None);
                        return;
                    },
                };
                let port = listener.local_addr().ok().map(|addr| addr.port());
                let _ = tx.send(port);
                tracing::info!(?port, "[EGRESS] shell egress proxy listening on 127.0.0.1");
                loop {
                    match listener.accept().await {
                        Ok((stream, _)) => {
                            tokio::spawn(async move {
                                if let Err(error) = serve(stream).await {
                                    tracing::debug!(%error, "[EGRESS] connection ended");
                                }
                            });
                        },
                        Err(error) => {
                            tracing::warn!(%error, "[EGRESS] accept failed");
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        },
                    }
                }
            });
        });
    if spawned.is_err() {
        return None;
    }
    rx.recv_timeout(Duration::from_secs(5)).ok().flatten()
}

/// The request line and headers, and any bytes read past them.
async fn read_head(stream: &mut TcpStream) -> std::io::Result<(String, Vec<u8>)> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "closed before a request"));
        }
        buf.extend_from_slice(&chunk[..read]);
        if let Some(end) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            let rest = buf.split_off(end + 4);
            return Ok((String::from_utf8_lossy(&buf).into_owned(), rest));
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "request head too large"));
        }
    }
}

async fn refuse(stream: &mut TcpStream, status: &str, reason: &str) -> std::io::Result<()> {
    let body = format!("Blocked by Magician's network policy: {reason}\n");
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

/// `host:port` with a default port, IPv6 brackets kept for the caller.
fn split_authority(authority: &str, default_port: u16) -> Option<(String, u16)> {
    let authority = authority.trim();
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let port = match tail.strip_prefix(':') {
            Some(port) => port.parse().ok()?,
            None => default_port,
        };
        return Some((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => Some((host.to_string(), port.parse().ok()?)),
        _ => Some((authority.to_string(), default_port)),
    }
}

async fn connect_checked(host: &str, port: u16) -> Result<TcpStream, String> {
    let addrs = crate::magician_v2::execution::network_policy::resolve_checked(host, port).await?;
    let mut last = String::from("no address");
    for addr in addrs {
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => last = format!("{addr}: {error}"),
            Err(_) => last = format!("{addr}: timed out"),
        }
    }
    Err(format!("could not connect to {host}:{port} ({last})"))
}

async fn serve(mut client: TcpStream) -> std::io::Result<()> {
    let (head, rest) = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut client)).await {
        Ok(result) => result?,
        Err(_) => return Ok(()),
    };
    let request_line = head.lines().next().unwrap_or_default().to_string();
    let mut parts = request_line.split_whitespace();
    let (method, target, version) = match (parts.next(), parts.next(), parts.next()) {
        (Some(method), Some(target), Some(version)) => (method.to_string(), target.to_string(), version.to_string()),
        _ => return refuse(&mut client, "400 Bad Request", "malformed request").await,
    };

    if method.eq_ignore_ascii_case("CONNECT") {
        let Some((host, port)) = split_authority(&target, 443) else {
            return refuse(&mut client, "400 Bad Request", "bad CONNECT target").await;
        };
        let mut upstream = match connect_checked(&host, port).await {
            Ok(stream) => stream,
            Err(reason) => {
                tracing::info!(%host, port, %reason, "[EGRESS] refused");
                return refuse(&mut client, "403 Forbidden", &reason).await;
            },
        };
        tracing::debug!(%host, port, "[EGRESS] CONNECT");
        client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
        if !rest.is_empty() {
            upstream.write_all(&rest).await?;
        }
        tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
        return Ok(());
    }

    // Absolute-form plain HTTP.
    let Ok(url) = url::Url::parse(&target) else {
        return refuse(&mut client, "400 Bad Request", "only CONNECT or absolute http:// requests").await;
    };
    if url.scheme() != "http" {
        return refuse(&mut client, "400 Bad Request", "use CONNECT for https").await;
    }
    let Some(host) = url.host_str().map(str::to_string) else {
        return refuse(&mut client, "400 Bad Request", "no host").await;
    };
    let port = url.port_or_known_default().unwrap_or(80);
    let mut upstream = match connect_checked(&host, port).await {
        Ok(stream) => stream,
        Err(reason) => {
            tracing::info!(%host, port, %reason, "[EGRESS] refused");
            return refuse(&mut client, "403 Forbidden", &reason).await;
        },
    };
    let mut path = url.path().to_string();
    if let Some(query) = url.query() {
        path.push('?');
        path.push_str(query);
    }
    let mut forwarded = format!("{method} {path} {version}\r\n");
    for line in head.lines().skip(1) {
        if line.is_empty() {
            continue;
        }
        let name = line.split(':').next().unwrap_or_default().trim().to_ascii_lowercase();
        if matches!(name.as_str(), "connection" | "proxy-connection" | "keep-alive" | "proxy-authorization") {
            continue;
        }
        forwarded.push_str(line);
        forwarded.push_str("\r\n");
    }
    forwarded.push_str("Connection: close\r\n\r\n");
    upstream.write_all(forwarded.as_bytes()).await?;
    if !rest.is_empty() {
        upstream.write_all(&rest).await?;
    }
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

/// The proxy URL a sandboxed shell is given.
pub fn proxy_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{NetworkMode, NetworkPolicySettings};
    use crate::magician_v2::execution::network_policy;

    #[test]
    fn authorities_split_with_defaults() {
        assert_eq!(split_authority("example.com:8443", 443), Some(("example.com".into(), 8443)));
        assert_eq!(split_authority("example.com", 443), Some(("example.com".into(), 443)));
        assert_eq!(split_authority("[::1]:80", 443), Some(("::1".into(), 80)));
        assert_eq!(split_authority("[::1]", 443), Some(("::1".into(), 443)));
    }

    async fn through_proxy(port: u16, request: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut out)).await;
        String::from_utf8_lossy(&out).into_owned()
    }

    /// Under `public` the proxy refuses loopback, and reaches a local server
    /// once it is on `allow_private`.
    #[tokio::test]
    async fn loopback_is_refused_unless_allowlisted() {
        let _lock = network_policy::test_lock();
        let local = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let local_port = local.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = local.accept().await {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                    .await;
            }
        });
        let port = port().expect("proxy");
        let get = format!("GET http://127.0.0.1:{local_port}/ HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");

        network_policy::configure(&NetworkPolicySettings { mode: NetworkMode::Public, allow_private: vec![] });
        let refused = through_proxy(port, &get).await;
        assert!(refused.starts_with("HTTP/1.1 403"), "{refused}");
        assert!(refused.contains("non-public address"), "{refused}");
        let connect = through_proxy(port, &format!("CONNECT 127.0.0.1:{local_port} HTTP/1.1\r\n\r\n")).await;
        assert!(connect.starts_with("HTTP/1.1 403"), "{connect}");

        network_policy::configure(&NetworkPolicySettings {
            mode: NetworkMode::Public,
            allow_private: vec![format!("127.0.0.1:{local_port}")],
        });
        let allowed = through_proxy(port, &get).await;
        assert!(allowed.starts_with("HTTP/1.1 200") && allowed.ends_with("ok"), "{allowed}");
        network_policy::configure(&NetworkPolicySettings::default());
    }
}
