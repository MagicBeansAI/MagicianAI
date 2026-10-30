//! Each harness turn's grant is good only on its own loopback channel.
//!
//! A run grant is a bearer: whoever presents it acts as the run's agent.
//! Its copies cannot be kept secret from every same-user process (the
//! harness's mcp config file, a `bearer_token_env_var` the harness reads, a
//! process environment another same-user process can read through
//! `KERN_PROCARGS2` or `/proc`), so a grant is bound to where it is used
//! instead: [`open`] gives the turn a loopback port of its own that forwards
//! to the plane-only listener (`shell_box::plane_mcp_port`). The forwarder
//! records which upstream connection carries which grant, and the plane's MCP
//! handler admits a bound grant only on such a connection ([`admits`]), on
//! the plane-only listener, never on the API port. OS boxes allow a harness
//! only its own channel port ([`current_plane_session`]), so a grant stolen
//! by another boxed process is refused on that process's own channel and
//! cannot reach the owner's. The harness's private files live in its own
//! subfolder of a fresh private folder ([`private_path`]), and every box
//! denies the other sessions' subfolders and the folder's listing
//! (`shell_box`).
//!
//! Design: `docs/features/goal-workspaces.md` (Limits).

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use tokio::net::{TcpListener, TcpStream};

/// The harness session the current task launches for: its channel port and
/// the private paths its box may still read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaneSession {
    /// Names this session's private subfolder (`<private root>/<id>`).
    pub id: String,
    /// The loopback port forwarding to the plane-only listener.
    pub port: u16,
    /// Other private paths of this session (its native home).
    pub own_paths: Vec<PathBuf>,
}

tokio::task_local! {
    static PLANE_SESSION: PlaneSession;
}

/// Run `fut` as the harness session `session` (its box allows the channel
/// port and the session's own private files). `None` runs it unchanged.
pub async fn with_plane_session<F: Future>(session: Option<PlaneSession>, fut: F) -> F::Output {
    match session {
        Some(session) => PLANE_SESSION.scope(session, fut).await,
        None => fut.await,
    }
}

/// The harness session the current task launches for, if any.
pub fn current_plane_session() -> Option<PlaneSession> {
    PLANE_SESSION.try_with(Clone::clone).ok()
}

/// The folder holding every harness session's private files (its mcp
/// config with the grant, its temporary home): `$TMPDIR/magician-plane-<random>`,
/// made fresh for this process (so nothing can be placed there first), one
/// subfolder per session. Every OS box denies it but the box of the session
/// a subfolder belongs to (`shell_box`), and its listing.
pub fn private_root() -> PathBuf {
    static ROOT: LazyLock<PathBuf> = LazyLock::new(|| {
        for _ in 0..16 {
            let candidate = std::env::temp_dir()
                .join(format!("magician-plane-{}", uuid::Uuid::new_v4().simple()));
            match std::fs::create_dir(&candidate) {
                Ok(()) => {
                    private_dir_mode(&candidate);
                    return candidate;
                },
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => break,
            }
        }
        std::env::temp_dir().join(format!("magician-plane-{}", uuid::Uuid::new_v4().simple()))
    });
    ROOT.clone()
}

fn private_dir_mode(dir: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// The private folder of the current harness session (`<private root>/<id>`),
/// or of every session without a channel (`<private root>/unbound`).
pub fn session_private_dir(session: Option<&PlaneSession>) -> PathBuf {
    private_root().join(session.map_or("unbound", |session| session.id.as_str()))
}

/// A private path for the current harness session: inside its own
/// subfolder of [`private_root`], which only its own box may read.
pub fn private_path(rest: &str) -> PathBuf {
    let dir = session_private_dir(current_plane_session().as_ref());
    if std::fs::create_dir_all(&dir).is_ok() {
        private_dir_mode(&dir);
    }
    dir.join(rest)
}

/// Concurrent connections one channel forwards; more are dropped.
const MAX_CHANNEL_CONNECTIONS: usize = 32;

/// How long the forwarder waits to reach the plane-only listener.
const UPSTREAM_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

struct Channel {
    token: String,
    port: u16,
    accept: tokio::task::AbortHandle,
}

#[derive(Default)]
struct Registry {
    /// Channel id → its live channel (removed when its turn ends). A token
    /// opened twice has two channels; each guard closes only its own.
    channels: HashMap<u64, Channel>,
    /// Live channel ports, for an O(1) [`is_channel_port`].
    ports: HashSet<u16>,
    /// Tokens bound to a channel, kept after the turn ends until the grant
    /// is revoked ([`forget`]): a late replay of a bound token off its
    /// channel stays refused.
    bound: HashSet<String>,
    /// Upstream connection (its full local address toward the plane-only
    /// listener) → the grant whose channel opened it.
    connections: HashMap<SocketAddr, String>,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| Mutex::new(Registry::default()));

fn registry() -> std::sync::MutexGuard<'static, Registry> {
    REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Closes the channel when the turn ends: its accept loop stops and its
/// port is released; connections already open finish. The grant stays bound.
pub struct ChannelGuard {
    id: u64,
}

impl Drop for ChannelGuard {
    fn drop(&mut self) {
        let mut registry = registry();
        if let Some(channel) = registry.channels.remove(&self.id) {
            channel.accept.abort();
            registry.ports.remove(&channel.port);
        }
    }
}

static NEXT_CHANNEL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Open a channel for `token` to the plane-only listener at `upstream`.
/// `own_paths`: the session's private paths besides its temp files.
pub async fn open_to(
    token: &str,
    upstream: u16,
    own_paths: Vec<PathBuf>,
) -> std::io::Result<(PlaneSession, ChannelGuard)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let bound = token.to_string();
    let live = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let accept = tokio::spawn(async move {
        loop {
            let Ok((inbound, peer)) = listener.accept().await else {
                break;
            };
            if !peer.ip().is_loopback() {
                continue;
            }
            if live.fetch_add(1, std::sync::atomic::Ordering::AcqRel) >= MAX_CHANNEL_CONNECTIONS {
                live.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                drop(inbound);
                continue;
            }
            let live = std::sync::Arc::clone(&live);
            let token = bound.clone();
            tokio::spawn(async move {
                forward(inbound, upstream, token).await;
                live.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            });
        }
    })
    .abort_handle();
    let id = NEXT_CHANNEL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    {
        let mut registry = registry();
        registry.channels.insert(
            id,
            Channel {
                token: token.to_string(),
                port,
                accept,
            },
        );
        registry.ports.insert(port);
        registry.bound.insert(token.to_string());
    }
    Ok((
        PlaneSession {
            id: uuid::Uuid::new_v4().simple().to_string(),
            port,
            own_paths,
        },
        ChannelGuard { id },
    ))
}

/// [`open_to`] the registered plane-only listener; `None` (the grant left
/// unbound, on the old endpoint) when there is none or the port cannot be
/// bound.
pub async fn open(token: &str, own_paths: Vec<PathBuf>) -> Option<(PlaneSession, ChannelGuard)> {
    let upstream = crate::magician_v2::execution::shell_box::plane_mcp_port()?;
    match open_to(token, upstream, own_paths).await {
        Ok(opened) => Some(opened),
        Err(error) => {
            tracing::warn!(%error, "plane grant channel could not be opened; the grant stays unbound");
            None
        },
    }
}

async fn forward(mut inbound: TcpStream, upstream: u16, token: String) {
    let Ok(Ok(mut outbound)) = tokio::time::timeout(
        UPSTREAM_CONNECT_TIMEOUT,
        TcpStream::connect(("127.0.0.1", upstream)),
    )
    .await
    else {
        return;
    };
    let Ok(local) = outbound.local_addr() else {
        return;
    };
    // Recorded before a byte is forwarded, so the listener never sees this
    // connection's request unrecorded.
    registry().connections.insert(local, token);
    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
    registry().connections.remove(&local);
}

/// Unbind `token` (its grant was revoked).
pub fn forget(token: &str) {
    let mut registry = registry();
    let ids: Vec<u64> = registry
        .channels
        .iter()
        .filter(|(_, channel)| channel.token == token)
        .map(|(id, _)| *id)
        .collect();
    for id in ids {
        if let Some(channel) = registry.channels.remove(&id) {
            channel.accept.abort();
            registry.ports.remove(&channel.port);
        }
    }
    registry.bound.remove(token);
    registry.connections.retain(|_, bound| bound != token);
}

/// Whether `port` is a live grant channel's: no agent tool (the `http`
/// tool, the egress proxy, another session's box) may call it.
pub fn is_channel_port(port: u16) -> bool {
    registry().ports.contains(&port)
}

/// The live channel ports (for boxes to deny all but their own).
pub fn channel_ports() -> Vec<u16> {
    registry().ports.iter().copied().collect()
}

/// Whether `token` is bound to a channel.
pub fn is_bound(token: &str) -> bool {
    registry().bound.contains(token)
}

/// Whether a request presenting `token`, from `peer`, arriving on the
/// listener at `local_port`, may use it. A bound grant only on its own
/// channel's upstream connection to the plane-only listener `plane_port`;
/// an unbound one (a durable terminal grant, a run whose channel could not
/// open) anywhere but the plane-only listener, which only channels reach.
pub fn admits_on(
    token: &str,
    peer: Option<SocketAddr>,
    local_port: Option<u16>,
    plane_port: Option<u16>,
) -> bool {
    let on_plane_listener = local_port.is_some() && local_port == plane_port;
    if !is_bound(token) {
        return !on_plane_listener;
    }
    on_plane_listener
        && peer.is_some_and(|peer| {
            // The forwarder connects from 127.0.0.1 exactly; on Linux every
            // 127.x address is loopback, so a port alone could be another
            // socket's.
            peer.ip() == std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
                && registry()
                    .connections
                    .get(&peer)
                    .is_some_and(|bound| bound == token)
        })
}

/// [`admits_on`] the registered plane-only listener.
pub fn admits(token: &str, peer: Option<SocketAddr>, local_port: Option<u16>) -> bool {
    admits_on(
        token,
        peer,
        local_port,
        crate::magician_v2::execution::shell_box::plane_mcp_port(),
    )
}

/// `url` with its port replaced by `port` (the channel's).
pub fn endpoint_on(url: &str, port: u16) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else {
        return url.to_string();
    };
    if parsed.set_port(Some(port)).is_ok() {
        parsed.to_string()
    } else {
        url.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A stand-in plane-only listener that answers each connection with the
    /// verdict `admits_on` gives the presented token on it.
    async fn plane_listener() -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut stream, peer)) = listener.accept().await {
                let mut buf = vec![0u8; 256];
                let read = stream.read(&mut buf).await.unwrap_or(0);
                let token = String::from_utf8_lossy(&buf[..read]).trim().to_string();
                let verdict = if admits_on(&token, Some(peer), Some(port), Some(port)) {
                    "ok"
                } else {
                    "refused"
                };
                let _ = stream.write_all(verdict.as_bytes()).await;
            }
        });
        port
    }

    async fn present(port: u16, token: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(token.as_bytes()).await.unwrap();
        let mut out = String::new();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream.read_to_string(&mut out),
        )
        .await;
        out
    }

    /// A grant works on its own channel only: the same grant presented on
    /// another session's channel, or straight to the plane-only listener, is
    /// refused; an unbound grant is refused on the plane-only listener.
    #[tokio::test]
    async fn a_stolen_grant_presented_by_another_session_is_refused() {
        let plane = plane_listener().await;
        let (admin, _admin_guard) = open_to("plt_admin_test", plane, Vec::new()).await.unwrap();
        let (fenced, _fenced_guard) = open_to("plt_fenced_test", plane, Vec::new()).await.unwrap();
        assert_eq!(present(admin.port, "plt_admin_test").await, "ok");
        assert_eq!(present(fenced.port, "plt_fenced_test").await, "ok");
        // The fenced session presents the admin's grant on its own channel.
        assert_eq!(present(fenced.port, "plt_admin_test").await, "refused");
        // Or goes around every channel.
        assert_eq!(present(plane, "plt_admin_test").await, "refused");
        assert_eq!(present(plane, "plt_unbound_test").await, "refused");
        // Off the plane-only listener (the API port), a bound grant is
        // refused and an unbound one is left to the API's own checks.
        assert!(!admits_on("plt_admin_test", None, Some(3002), Some(plane)));
        assert!(admits_on("plt_unbound_test", None, Some(3002), Some(plane)));
        assert!(is_channel_port(admin.port));
        // The turn ends: the port closes, the grant stays bound (a late
        // replay off its channel is still refused).
        drop(_fenced_guard);
        assert!(!is_channel_port(fenced.port));
        assert!(is_bound("plt_fenced_test"));
        assert!(!admits_on("plt_fenced_test", None, Some(3002), Some(plane)));
        forget("plt_admin_test");
        forget("plt_fenced_test");
        assert!(!is_bound("plt_admin_test"));
        assert!(!is_channel_port(admin.port));
    }

    /// A connection is recognised by its full address from 127.0.0.1, not
    /// by its port alone.
    #[test]
    fn a_connection_is_matched_by_its_full_loopback_address() {
        let ours: SocketAddr = "127.0.0.1:5555".parse().unwrap();
        registry().bound.insert("plt_addr_test".into());
        registry().connections.insert(ours, "plt_addr_test".into());
        assert!(admits_on("plt_addr_test", Some(ours), Some(9), Some(9)));
        assert!(!admits_on(
            "plt_addr_test",
            Some("127.0.0.2:5555".parse().unwrap()),
            Some(9),
            Some(9)
        ));
        assert!(!admits_on(
            "plt_addr_test",
            Some("[::1]:5555".parse().unwrap()),
            Some(9),
            Some(9)
        ));
        forget("plt_addr_test");
    }

    /// The same token opened twice: each guard closes only its own channel.
    #[tokio::test]
    async fn a_guard_closes_only_its_own_channel() {
        let (first, first_guard) = open_to("plt_twice_test", 1, Vec::new()).await.unwrap();
        let (second, _second_guard) = open_to("plt_twice_test", 1, Vec::new()).await.unwrap();
        drop(first_guard);
        assert!(!is_channel_port(first.port));
        assert!(is_channel_port(second.port));
        assert!(is_bound("plt_twice_test"));
        forget("plt_twice_test");
        assert!(!is_channel_port(second.port));
    }

    #[test]
    fn the_endpoint_moves_to_the_channel_port() {
        assert_eq!(
            endpoint_on("http://127.0.0.1:41000/api/magician/v2/plane/mcp", 41001),
            "http://127.0.0.1:41001/api/magician/v2/plane/mcp"
        );
    }
}
