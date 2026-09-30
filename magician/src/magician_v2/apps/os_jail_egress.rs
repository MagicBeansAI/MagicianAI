//! Magician-owned egress broker for OS-jail skills (`app_egress` v1).
//!
//! A jailed skill normally has no network. A skill whose reviewed source
//! declares its HTTPS destinations (`metadata.magician.app_egress`, one
//! `destination` or a bounded `destinations` list), or an in-place skill the
//! owner granted network to, instead runs in MagicRun's brokered-egress jail,
//! whose only reachable endpoint is the broker started here for that one
//! call. The broker, not the jail, owns every network decision:
//!
//! - only `CONNECT <host>:443` for an admitted host is accepted: a declared
//!   host the app is granted, every granted host for an in-place skill that
//!   declares none, or any public host when the owner granted "any public
//!   host"; any other host, port or method is refused, so plain HTTP and
//!   absolute-form proxying never leave;
//! - the host is resolved here, never by the child, and every resolved
//!   address must be public (the same rule as the bound-HTTP owner);
//! - connections, concurrency, bytes each way and idle time are capped;
//! - the receipt records every host contacted (connections and bytes each
//!   way, never payloads) and what was refused.
//!
//! Whether the run may reach the destination at all is decided before launch
//! by tool disclosure: the jail call is admitted as an external tool call to
//! `destination:<host>`, so the run's granted network destinations and data
//! handling policy apply exactly as they do to the in-process bound-HTTP tool.
//!
//! On macOS the broker listens on an ephemeral loopback TCP port; on Linux on
//! a unix socket in a private directory, which the in-jail forwarder relays.
//! Any local process of the same user can find the macOS port while a call is
//! running; it gains nothing it could not do directly (the same public host,
//! no injected credentials) and can only spend this call's budget.

use std::{
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tool_runtime_core::{
    governed_process_jail::GovernedEgressBrokerEndpoint, manifest_parser::parse_skill_frontmatter,
};

pub(crate) const APP_OS_JAIL_EGRESS_SCHEMA_VERSION: u32 = 1;
pub(crate) const APP_OS_JAIL_EGRESS_PROFILE_V1: &str = "magician.app-os-jail-egress.v1";
pub(crate) const APP_OS_JAIL_EGRESS_PORT: u16 = 443;

const MAX_DESTINATION_BYTES: usize = 253;
/// Most hosts one skill may declare.
pub(crate) const MAX_APP_OS_JAIL_EGRESS_DESTINATIONS: usize = 16;
/// Most granted hosts one call's broker admits.
pub(crate) const MAX_APP_OS_JAIL_EGRESS_ADMITTED_HOSTS: usize = 64;
/// The destination reference an "any public host" call is attested with. It
/// is a single DNS label, so no real host (which needs two) can collide with
/// it, and tool disclosure admits it only for an owner-granted
/// `granted_any_public_host`.
pub(crate) const APP_OS_JAIL_ANY_PUBLIC_HOST_REF: &str = "destination:any-public-host";
/// The one `app_egress.destinations` entry meaning "arbitrary sites"; only
/// valid as the sole entry.
pub(crate) const APP_OS_JAIL_ANY_DECLARED_HOST: &str = "*";
const MAX_REQUEST_HEAD_BYTES: usize = 8 * 1024;
const MAX_RECORDED_REFUSALS: usize = 8;
const MAX_RESOLVED_ADDRESSES: usize = 16;
const ACCEPT_POLL: Duration = Duration::from_millis(20);
const IO_TICK: Duration = Duration::from_millis(250);
const RELAY_BUFFER_BYTES: usize = 16 * 1024;

/// Finite ceilings for one call's broker. They are part of the reviewed plan
/// digest, so changing them re-reviews every egress-enabled lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct AppOsJailEgressLimits {
    pub max_connections: u32,
    pub max_concurrent_connections: u32,
    pub max_bytes_up: u64,
    pub max_bytes_down: u64,
    pub request_head_timeout_ms: u64,
    pub resolve_timeout_ms: u64,
    pub connect_timeout_ms: u64,
    pub idle_timeout_ms: u64,
}

pub(crate) const APP_OS_JAIL_EGRESS_LIMITS: AppOsJailEgressLimits = AppOsJailEgressLimits {
    max_connections: 16,
    max_concurrent_connections: 4,
    max_bytes_up: 1024 * 1024,
    max_bytes_down: 32 * 1024 * 1024,
    request_head_timeout_ms: 10_000,
    resolve_timeout_ms: 5_000,
    connect_timeout_ms: 10_000,
    idle_timeout_ms: 30_000,
};

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub(crate) enum AppOsJailEgressError {
    #[error("the skill's app_egress declaration is invalid")]
    InvalidDeclaration,
    #[error("the egress broker could not be started")]
    BrokerUnavailable,
    #[error("brokered egress is not supported on this platform")]
    #[cfg_attr(any(target_os = "macos", target_os = "linux"), allow(dead_code))]
    UnsupportedPlatform,
}

/// The HTTPS destinations a reviewed skill declares, normalized, sorted and
/// unique (at least one, at most [`MAX_APP_OS_JAIL_EGRESS_DESTINATIONS`]), or
/// exactly `["*"]` for a skill that contacts arbitrary sites (a web fetcher),
/// which counts as an any-host tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppOsJailEgressDeclaration {
    destinations: Vec<String>,
}

impl AppOsJailEgressDeclaration {
    /// The first declared host: the only one for the single `destination`
    /// form, which keeps its reviewed lock and attestation identities.
    pub(crate) fn destination(&self) -> &str {
        &self.destinations[0]
    }

    pub(crate) fn destinations(&self) -> &[String] {
        &self.destinations
    }

    /// `destinations: ["*"]`: the skill contacts arbitrary sites. It reaches
    /// the network only under the app's "any public host" grant.
    pub(crate) fn any_host(&self) -> bool {
        self.destinations == [APP_OS_JAIL_ANY_DECLARED_HOST]
    }
}

/// What the owner granted this app's network, as the broker needs it: the
/// hosts of the live effective network policy and the explicit "any public
/// host" grant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AppOsJailNetworkGrant {
    pub(crate) hosts: std::collections::BTreeSet<String>,
    pub(crate) any_public_host: bool,
}

impl AppOsJailNetworkGrant {
    /// From granted `destination:<host>` references; anything that is not a
    /// public DNS host (for example the any-host marker) is ignored.
    pub(crate) fn from_destination_refs<'a>(
        destinations: impl IntoIterator<Item = &'a str>,
        any_public_host: bool,
    ) -> Self {
        Self {
            hosts: destinations
                .into_iter()
                .filter_map(|reference| reference.strip_prefix("destination:"))
                .filter_map(normalize_destination)
                .collect(),
            any_public_host,
        }
    }
}

/// Which hosts one call's broker admits. Part of the call's attested
/// endpoint configuration, so the disclosure permit binds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub(crate) enum AppOsJailEgressAdmission {
    Hosts { hosts: Vec<String> },
    AnyPublicHost,
}

impl AppOsJailEgressAdmission {
    /// The admission for one call:
    ///
    /// - a tool that declares any site (`*`): any public host only under a
    ///   resolved "any public host" grant, else no network (also when no
    ///   grant was resolved);
    /// - no grant resolved: the declared hosts (disclosure then refuses any
    ///   the app is not granted), exactly as before multi-host grants;
    /// - declared hosts: those the app is granted, or all of them when none
    ///   is (disclosure refuses the call); "any public host" never widens a
    ///   tool that declares its hosts;
    /// - "any public host" granted: any public host, for an in-place skill
    ///   that declares none;
    /// - an in-place skill that declares none: every granted host;
    /// - otherwise no network at all.
    pub(crate) fn for_call(
        declared: Option<&AppOsJailEgressDeclaration>,
        grant: Option<&AppOsJailNetworkGrant>,
        undeclared_network: bool,
    ) -> Option<Self> {
        // A skill that declares arbitrary sites (`*`) is an any-host tool:
        // any public host only under a resolved "any public host" grant
        // (attested so disclosure checks that grant). With no grant resolved
        // it gets no network at all, never any site.
        if declared.is_some_and(AppOsJailEgressDeclaration::any_host) {
            return grant
                .is_some_and(|grant| grant.any_public_host)
                .then_some(Self::AnyPublicHost);
        }
        let Some(grant) = grant else {
            return declared.map(|declared| Self::Hosts {
                hosts: declared.destinations.clone(),
            });
        };
        if let Some(declared) = declared {
            let granted = declared
                .destinations
                .iter()
                .filter(|host| grant.hosts.contains(*host))
                .cloned()
                .collect::<Vec<_>>();
            return Some(Self::Hosts {
                hosts: if granted.is_empty() {
                    declared.destinations.clone()
                } else {
                    granted
                },
            });
        }
        if undeclared_network && grant.any_public_host {
            return Some(Self::AnyPublicHost);
        }
        (undeclared_network && !grant.hosts.is_empty()).then(|| Self::Hosts {
            hosts: grant
                .hosts
                .iter()
                .take(MAX_APP_OS_JAIL_EGRESS_ADMITTED_HOSTS)
                .cloned()
                .collect(),
        })
    }

    pub(crate) fn admits(&self, host: &str) -> bool {
        match self {
            Self::Hosts { hosts } => hosts.iter().any(|admitted| admitted == host),
            Self::AnyPublicHost => normalize_destination(host).as_deref() == Some(host),
        }
    }

    /// The one admitted host, if exactly one: a secret's vault grant can then
    /// be scoped to it.
    pub(crate) fn single_host(&self) -> Option<&str> {
        match self {
            Self::Hosts { hosts } if hosts.len() == 1 => Some(&hosts[0]),
            _ => None,
        }
    }

    /// The destination references disclosure checks, in order.
    pub(crate) fn destination_refs(&self) -> Vec<String> {
        match self {
            Self::Hosts { hosts } => hosts
                .iter()
                .map(|host| format!("destination:{host}"))
                .collect(),
            Self::AnyPublicHost => vec![APP_OS_JAIL_ANY_PUBLIC_HOST_REF.to_owned()],
        }
    }

    fn receipt_label(&self) -> String {
        match self {
            Self::Hosts { hosts } if hosts.len() == 1 => hosts[0].clone(),
            Self::Hosts { .. } => "multiple".to_owned(),
            Self::AnyPublicHost => "any-public-host".to_owned(),
        }
    }
}

#[derive(Deserialize)]
struct EgressHeader {
    #[serde(default)]
    metadata: Option<EgressMetadata>,
}

#[derive(Deserialize)]
struct EgressMetadata {
    #[serde(default)]
    magician: Option<EgressMagician>,
}

#[derive(Deserialize)]
struct EgressMagician {
    #[serde(default)]
    app_egress: Option<RawEgressDeclaration>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEgressDeclaration {
    schema_version: u32,
    #[serde(default)]
    destination: Option<String>,
    #[serde(default)]
    destinations: Option<Vec<String>>,
}

/// Read the optional `metadata.magician.app_egress` block from reviewed skill
/// source. Absent means no network; present but malformed is refused rather
/// than ignored, so a typo can never silently widen or drop a declaration.
pub(crate) fn parse_app_egress_declaration(
    source: &str,
) -> Result<Option<AppOsJailEgressDeclaration>, AppOsJailEgressError> {
    let header: EgressHeader =
        parse_skill_frontmatter(source).map_err(|_| AppOsJailEgressError::InvalidDeclaration)?;
    let Some(raw) = header
        .metadata
        .and_then(|metadata| metadata.magician)
        .and_then(|magician| magician.app_egress)
    else {
        return Ok(None);
    };
    if raw.schema_version != APP_OS_JAIL_EGRESS_SCHEMA_VERSION {
        return Err(AppOsJailEgressError::InvalidDeclaration);
    }
    let raw_destinations = match (raw.destination, raw.destinations) {
        (Some(destination), None) => vec![destination],
        (None, Some(destinations))
            if !destinations.is_empty()
                && destinations.len() <= MAX_APP_OS_JAIL_EGRESS_DESTINATIONS =>
        {
            destinations
        },
        _ => return Err(AppOsJailEgressError::InvalidDeclaration),
    };
    if raw_destinations
        .iter()
        .any(|raw| raw.trim() == APP_OS_JAIL_ANY_DECLARED_HOST)
    {
        return if raw_destinations.len() == 1 {
            Ok(Some(AppOsJailEgressDeclaration {
                destinations: vec![APP_OS_JAIL_ANY_DECLARED_HOST.to_owned()],
            }))
        } else {
            Err(AppOsJailEgressError::InvalidDeclaration)
        };
    }
    let mut destinations = std::collections::BTreeSet::new();
    for raw in &raw_destinations {
        let destination =
            normalize_destination(raw).ok_or(AppOsJailEgressError::InvalidDeclaration)?;
        if !destinations.insert(destination) {
            return Err(AppOsJailEgressError::InvalidDeclaration);
        }
    }
    Ok(Some(AppOsJailEgressDeclaration {
        destinations: destinations.into_iter().collect(),
    }))
}

/// A lowercase DNS name with at least two labels. IP literals, ports, paths,
/// wildcards and local names are refused: the grant names one public host.
fn normalize_destination(raw: &str) -> Option<String> {
    let host = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty()
        || host.len() > MAX_DESTINATION_BYTES
        || host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.parse::<std::net::IpAddr>().is_ok()
    {
        return None;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2
        || labels.iter().any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
        || labels
            .last()
            .is_some_and(|tld| tld.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    Some(host)
}

/// One host a call's broker tunnelled to: how often and how many bytes each
/// way. Never payload bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct AppOsJailEgressHostUsage {
    pub host: String,
    pub connections: u32,
    pub bytes_up: u64,
    pub bytes_down: u64,
}

/// What one call's broker did, recorded in the call's audit evidence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct AppOsJailEgressReceipt {
    pub profile: &'static str,
    /// The one admitted host, or `multiple` / `any-public-host`.
    pub destination: String,
    /// Every host contacted, sorted (bounded by the connection ceiling).
    pub contacted: Vec<AppOsJailEgressHostUsage>,
    pub connections_tunnelled: u32,
    pub connections_refused: u32,
    pub connections_failed: u32,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub byte_ceiling_reached: bool,
    /// The first few refusal reasons, e.g. `host_not_granted`. Never includes
    /// payload bytes.
    pub refusals: Vec<String>,
}

type Resolver = fn(&str, u16) -> io::Result<Vec<SocketAddr>>;

fn system_resolver(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
    (host, port).to_socket_addrs().map(Iterator::collect)
}

fn public_addresses(host: &str, addresses: &[SocketAddr]) -> bool {
    crate::magician_v2::content_sources::validate_public_socket_addresses(host, addresses).is_ok()
}

struct BrokerPolicy {
    admission: AppOsJailEgressAdmission,
    limits: AppOsJailEgressLimits,
    resolver: Resolver,
    address_check: fn(&str, &[SocketAddr]) -> bool,
    /// Always 443 outside tests; a resolved address on any other port is
    /// never dialled.
    dial_port: u16,
}

#[derive(Default)]
struct BrokerLedger {
    receipt: AppOsJailEgressReceipt,
    accepted: u32,
    active: u32,
}

impl BrokerLedger {
    fn refuse(&mut self, reason: &str) {
        self.receipt.connections_refused += 1;
        if self.receipt.refusals.len() < MAX_RECORDED_REFUSALS {
            self.receipt.refusals.push(reason.to_owned());
        }
    }
}

struct BrokerShared {
    policy: BrokerPolicy,
    stop: AtomicBool,
    bytes_up: AtomicU64,
    bytes_down: AtomicU64,
    ledger: Mutex<BrokerLedger>,
}

impl BrokerShared {
    fn ledger(&self) -> std::sync::MutexGuard<'_, BrokerLedger> {
        self.ledger
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

/// A running broker. Dropping it without [`finish`](Self::finish) still stops
/// and joins every thread.
pub(crate) struct AppOsJailEgressBroker {
    endpoint: GovernedEgressBrokerEndpoint,
    shared: Arc<BrokerShared>,
    accept: Option<JoinHandle<()>>,
    #[cfg(target_os = "linux")]
    _socket_directory: PrivateSocketDirectory,
}

/// A fresh 0700 directory under the system temp dir holding the broker
/// socket, removed when the broker ends.
#[cfg(target_os = "linux")]
struct PrivateSocketDirectory(std::path::PathBuf);

#[cfg(target_os = "linux")]
impl PrivateSocketDirectory {
    fn create() -> io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;

        let base = std::fs::canonicalize(std::env::temp_dir())?;
        let path = base.join(format!(
            "magician-app-egress-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }
}

#[cfg(target_os = "linux")]
impl Drop for PrivateSocketDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl AppOsJailEgressBroker {
    pub(crate) fn start(
        admission: &AppOsJailEgressAdmission,
    ) -> Result<Self, AppOsJailEgressError> {
        Self::start_with(BrokerPolicy {
            admission: admission.clone(),
            limits: APP_OS_JAIL_EGRESS_LIMITS,
            resolver: system_resolver,
            address_check: public_addresses,
            dial_port: APP_OS_JAIL_EGRESS_PORT,
        })
    }

    #[cfg(target_os = "macos")]
    fn start_with(policy: BrokerPolicy) -> Result<Self, AppOsJailEgressError> {
        use std::num::NonZeroU16;

        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|_| AppOsJailEgressError::BrokerUnavailable)?;
        let port = listener
            .local_addr()
            .ok()
            .and_then(|address| NonZeroU16::new(address.port()))
            .ok_or(AppOsJailEgressError::BrokerUnavailable)?;
        let shared = Arc::new(BrokerShared::new(policy));
        let accept = spawn_accept_loop(Listener::Tcp(listener), Arc::clone(&shared))?;
        Ok(Self {
            endpoint: GovernedEgressBrokerEndpoint::LoopbackTcp { port },
            shared,
            accept: Some(accept),
        })
    }

    #[cfg(target_os = "linux")]
    fn start_with(policy: BrokerPolicy) -> Result<Self, AppOsJailEgressError> {
        use std::os::unix::net::UnixListener;

        let directory = PrivateSocketDirectory::create()
            .map_err(|_| AppOsJailEgressError::BrokerUnavailable)?;
        let path = directory.0.join("broker.sock");
        let listener =
            UnixListener::bind(&path).map_err(|_| AppOsJailEgressError::BrokerUnavailable)?;
        let shared = Arc::new(BrokerShared::new(policy));
        let accept = spawn_accept_loop(Listener::Unix(listener), Arc::clone(&shared))?;
        Ok(Self {
            endpoint: GovernedEgressBrokerEndpoint::UnixSocket { path },
            shared,
            accept: Some(accept),
            _socket_directory: directory,
        })
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn start_with(_policy: BrokerPolicy) -> Result<Self, AppOsJailEgressError> {
        Err(AppOsJailEgressError::UnsupportedPlatform)
    }

    pub(crate) fn endpoint(&self) -> GovernedEgressBrokerEndpoint {
        self.endpoint.clone()
    }

    /// Stop accepting, end every open tunnel, and return the receipt.
    pub(crate) fn finish(mut self) -> AppOsJailEgressReceipt {
        self.stop_and_join();
        let mut receipt = self.shared.ledger().receipt.clone();
        receipt.profile = APP_OS_JAIL_EGRESS_PROFILE_V1;
        receipt.destination = self.shared.policy.admission.receipt_label();
        receipt.contacted.sort_by(|left, right| left.host.cmp(&right.host));
        receipt.bytes_up = self.shared.bytes_up.load(Ordering::SeqCst);
        receipt.bytes_down = self.shared.bytes_down.load(Ordering::SeqCst);
        receipt
    }

    fn stop_and_join(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

impl Drop for AppOsJailEgressBroker {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

impl BrokerShared {
    fn new(policy: BrokerPolicy) -> Self {
        Self {
            policy,
            stop: AtomicBool::new(false),
            bytes_up: AtomicU64::new(0),
            bytes_down: AtomicU64::new(0),
            ledger: Mutex::new(BrokerLedger::default()),
        }
    }
}

enum Listener {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Tcp(TcpListener),
    #[cfg(unix)]
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Unix(std::os::unix::net::UnixListener),
}

impl Listener {
    fn set_nonblocking(&self) -> io::Result<()> {
        match self {
            Self::Tcp(listener) => listener.set_nonblocking(true),
            #[cfg(unix)]
            Self::Unix(listener) => listener.set_nonblocking(true),
        }
    }

    fn accept(&self) -> io::Result<ClientStream> {
        match self {
            Self::Tcp(listener) => listener
                .accept()
                .map(|(stream, _)| ClientStream::Tcp(stream)),
            #[cfg(unix)]
            Self::Unix(listener) => listener
                .accept()
                .map(|(stream, _)| ClientStream::Unix(stream)),
        }
    }
}

/// The jail-facing side of one connection.
enum ClientStream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}

impl ClientStream {
    fn prepare(&self, read_timeout: Duration) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => {
                stream.set_nonblocking(false)?;
                stream.set_read_timeout(Some(read_timeout))
            },
            #[cfg(unix)]
            Self::Unix(stream) => {
                stream.set_nonblocking(false)?;
                stream.set_read_timeout(Some(read_timeout))
            },
        }
    }

    fn try_clone(&self) -> io::Result<Self> {
        match self {
            Self::Tcp(stream) => stream.try_clone().map(Self::Tcp),
            #[cfg(unix)]
            Self::Unix(stream) => stream.try_clone().map(Self::Unix),
        }
    }

    fn shutdown(&self, how: Shutdown) {
        let _ = match self {
            Self::Tcp(stream) => stream.shutdown(how),
            #[cfg(unix)]
            Self::Unix(stream) => stream.shutdown(how),
        };
    }
}

impl Read for ClientStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buffer),
        }
    }
}

impl Write for ClientStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.write(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.flush(),
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
        }
    }
}

fn spawn_accept_loop(
    listener: Listener,
    shared: Arc<BrokerShared>,
) -> Result<JoinHandle<()>, AppOsJailEgressError> {
    listener
        .set_nonblocking()
        .map_err(|_| AppOsJailEgressError::BrokerUnavailable)?;
    thread::Builder::new()
        .name("app-egress-broker".to_owned())
        .spawn(move || accept_loop(listener, shared))
        .map_err(|_| AppOsJailEgressError::BrokerUnavailable)
}

fn accept_loop(listener: Listener, shared: Arc<BrokerShared>) {
    let mut connections: Vec<JoinHandle<()>> = Vec::new();
    while !shared.stopped() {
        match listener.accept() {
            Ok(mut client) => {
                let admitted = {
                    let mut ledger = shared.ledger();
                    let limits = shared.policy.limits;
                    if ledger.accepted >= limits.max_connections {
                        ledger.refuse("connection_ceiling");
                        false
                    } else if ledger.active >= limits.max_concurrent_connections {
                        ledger.refuse("concurrency_ceiling");
                        false
                    } else {
                        ledger.accepted += 1;
                        ledger.active += 1;
                        true
                    }
                };
                if !admitted {
                    let _ = client.prepare(IO_TICK);
                    respond(&mut client, "503 Service Unavailable");
                    client.shutdown(Shutdown::Both);
                    continue;
                }
                let worker = Arc::clone(&shared);
                match thread::Builder::new()
                    .name("app-egress-tunnel".to_owned())
                    .spawn(move || {
                        serve_connection(client, &worker);
                        worker.ledger().active -= 1;
                    }) {
                    Ok(handle) => connections.push(handle),
                    Err(_) => {
                        let mut ledger = shared.ledger();
                        ledger.active -= 1;
                        ledger.receipt.connections_failed += 1;
                    },
                }
            },
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL);
            },
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {},
            Err(_) => thread::sleep(ACCEPT_POLL),
        }
        connections.retain(|handle| !handle.is_finished());
    }
    for handle in connections {
        let _ = handle.join();
    }
}

fn respond(client: &mut ClientStream, status: &str) {
    let _ = client.write_all(
        format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes(),
    );
    let _ = client.flush();
}

enum Outcome {
    Refused(&'static str, &'static str),
    Failed(&'static str),
}

fn serve_connection(mut client: ClientStream, shared: &BrokerShared) {
    let limits = shared.policy.limits;
    let result = client
        .prepare(IO_TICK)
        .map_err(|_| Outcome::Failed("502 Bad Gateway"))
        .and_then(|()| read_request_head(&mut client, shared, limits))
        .and_then(|(authority, leftover)| {
            admit_authority(&authority, shared).map(|host| (host, leftover))
        })
        .and_then(|(host, leftover)| {
            open_upstream(&host, shared, limits).map(|upstream| (upstream, leftover, host))
        });
    let (upstream, leftover, host) = match result {
        Ok(opened) => opened,
        Err(Outcome::Refused(status, reason)) => {
            shared.ledger().refuse(reason);
            respond(&mut client, status);
            client.shutdown(Shutdown::Both);
            return;
        },
        Err(Outcome::Failed(status)) => {
            shared.ledger().receipt.connections_failed += 1;
            respond(&mut client, status);
            client.shutdown(Shutdown::Both);
            return;
        },
    };
    if client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .and_then(|()| client.flush())
        .is_err()
    {
        shared.ledger().receipt.connections_failed += 1;
        let _ = upstream.shutdown(Shutdown::Both);
        return;
    }
    shared.ledger().receipt.connections_tunnelled += 1;
    let (up, down) = relay(client, upstream, leftover, shared);
    let mut ledger = shared.ledger();
    let contacted = &mut ledger.receipt.contacted;
    let index = match contacted.iter().position(|usage| usage.host == host) {
        Some(index) => index,
        None => {
            contacted.push(AppOsJailEgressHostUsage {
                host,
                ..AppOsJailEgressHostUsage::default()
            });
            contacted.len() - 1
        },
    };
    let usage = &mut contacted[index];
    usage.connections += 1;
    usage.bytes_up = usage.bytes_up.saturating_add(up);
    usage.bytes_down = usage.bytes_down.saturating_add(down);
}

/// Read the proxy request head. Only the request line matters; headers are
/// bounded and discarded. Bytes after the head belong to the tunnel.
fn read_request_head(
    client: &mut ClientStream,
    shared: &BrokerShared,
    limits: AppOsJailEgressLimits,
) -> Result<(String, Vec<u8>), Outcome> {
    let deadline = Instant::now() + Duration::from_millis(limits.request_head_timeout_ms);
    let mut head = Vec::with_capacity(512);
    let mut chunk = [0_u8; 1024];
    let end = loop {
        if let Some(position) = find_head_end(&head) {
            break position;
        }
        if head.len() >= MAX_REQUEST_HEAD_BYTES {
            return Err(Outcome::Refused(
                "431 Request Header Fields Too Large",
                "oversized_request_head",
            ));
        }
        if shared.stopped() || Instant::now() >= deadline {
            return Err(Outcome::Refused(
                "408 Request Timeout",
                "request_head_timeout",
            ));
        }
        match client.read(&mut chunk) {
            Ok(0) => {
                return Err(Outcome::Refused(
                    "400 Bad Request",
                    "incomplete_request_head",
                ))
            },
            Ok(read) => head.extend_from_slice(&chunk[..read]),
            Err(error) if is_tick(&error) => {},
            Err(_) => return Err(Outcome::Failed("400 Bad Request")),
        }
    };
    let leftover = head.split_off(end);
    let line_end = head
        .windows(2)
        .position(|pair| pair == b"\r\n")
        .unwrap_or(head.len());
    let line = std::str::from_utf8(&head[..line_end])
        .map_err(|_| Outcome::Refused("400 Bad Request", "malformed_request_line"))?;
    let mut parts = line.split(' ');
    let (Some(method), Some(authority), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(Outcome::Refused(
            "400 Bad Request",
            "malformed_request_line",
        ));
    };
    if !version.starts_with("HTTP/1.") {
        return Err(Outcome::Refused(
            "400 Bad Request",
            "malformed_request_line",
        ));
    }
    if method != "CONNECT" {
        return Err(Outcome::Refused("403 Forbidden", "https_connect_only"));
    }
    Ok((authority.to_owned(), leftover))
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

/// The authority must be exactly `<admitted host>:443`.
fn admit_authority(authority: &str, shared: &BrokerShared) -> Result<String, Outcome> {
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or(Outcome::Refused("403 Forbidden", "port_not_granted"))?;
    if port.parse::<u16>().ok() != Some(APP_OS_JAIL_EGRESS_PORT) {
        return Err(Outcome::Refused("403 Forbidden", "port_not_granted"));
    }
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if !shared.policy.admission.admits(&host) {
        return Err(Outcome::Refused("403 Forbidden", "host_not_granted"));
    }
    Ok(host)
}

/// Resolve here (bounded), require every address to be public, and connect
/// to the first that answers.
fn open_upstream(
    host: &str,
    shared: &BrokerShared,
    limits: AppOsJailEgressLimits,
) -> Result<TcpStream, Outcome> {
    let (sender, receiver) = mpsc::channel();
    let resolver = shared.policy.resolver;
    let lookup_host = host.to_owned();
    thread::Builder::new()
        .name("app-egress-resolve".to_owned())
        .spawn(move || {
            let _ = sender.send(resolver(&lookup_host, APP_OS_JAIL_EGRESS_PORT));
        })
        .map_err(|_| Outcome::Failed("502 Bad Gateway"))?;
    let mut addresses =
        match receiver.recv_timeout(Duration::from_millis(limits.resolve_timeout_ms)) {
            Ok(Ok(addresses)) if !addresses.is_empty() => addresses,
            Ok(_) => return Err(Outcome::Failed("502 Bad Gateway")),
            Err(_) => return Err(Outcome::Failed("504 Gateway Timeout")),
        };
    addresses.truncate(MAX_RESOLVED_ADDRESSES);
    if !(shared.policy.address_check)(host, &addresses) {
        return Err(Outcome::Refused("403 Forbidden", "non_public_address"));
    }
    let timeout = Duration::from_millis(limits.connect_timeout_ms);
    for address in addresses {
        if shared.stopped() {
            break;
        }
        if address.port() != shared.policy.dial_port {
            continue;
        }
        if let Ok(stream) = TcpStream::connect_timeout(&address, timeout) {
            return Ok(stream);
        }
    }
    Err(Outcome::Failed("502 Bad Gateway"))
}

fn is_tick(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

/// Copy both directions until either side closes, the call stops, a byte
/// ceiling is reached, or the tunnel idles past its limit. Returns the bytes
/// relayed up and down on this connection.
fn relay(
    client: ClientStream,
    upstream: TcpStream,
    leftover: Vec<u8>,
    shared: &BrokerShared,
) -> (u64, u64) {
    let limits = shared.policy.limits;
    let (Ok(client_reader), Ok(upstream_reader)) = (client.try_clone(), upstream.try_clone())
    else {
        shared.ledger().receipt.connections_failed += 1;
        return (0, 0);
    };
    if upstream.set_read_timeout(Some(IO_TICK)).is_err() {
        shared.ledger().receipt.connections_failed += 1;
        return (0, 0);
    }
    let last_activity = Arc::new(Mutex::new(Instant::now()));
    let ended = Arc::new(AtomicBool::new(false));
    let relayed_up = AtomicU64::new(0);
    let relayed_down = AtomicU64::new(0);
    let mut upstream_writer = upstream;
    if !leftover.is_empty() {
        if charge(&shared.bytes_up, leftover.len(), limits.max_bytes_up).is_err()
            || upstream_writer.write_all(&leftover).is_err()
        {
            shared.ledger().receipt.byte_ceiling_reached = true;
            return (0, 0);
        }
        relayed_up.fetch_add(leftover.len() as u64, Ordering::SeqCst);
    }
    let relayed_up_ref = &relayed_up;
    let relayed_down_ref = &relayed_down;
    thread::scope(|scope| {
        let up_activity = Arc::clone(&last_activity);
        let up_ended = Arc::clone(&ended);
        scope.spawn(move || {
            pump(
                client_reader,
                &mut upstream_writer,
                (&shared.bytes_up, relayed_up_ref),
                limits.max_bytes_up,
                shared,
                &up_activity,
                &up_ended,
            );
            let _ = upstream_writer.shutdown(Shutdown::Write);
        });
        let mut client_writer = client;
        pump(
            upstream_reader,
            &mut client_writer,
            (&shared.bytes_down, relayed_down_ref),
            limits.max_bytes_down,
            shared,
            &last_activity,
            &ended,
        );
        client_writer.shutdown(Shutdown::Both);
    });
    (
        relayed_up.load(Ordering::SeqCst),
        relayed_down.load(Ordering::SeqCst),
    )
}

fn charge(counter: &AtomicU64, bytes: usize, ceiling: u64) -> Result<(), ()> {
    let bytes = u64::try_from(bytes).map_err(|_| ())?;
    let previous = counter.fetch_add(bytes, Ordering::SeqCst);
    if previous.saturating_add(bytes) > ceiling {
        return Err(());
    }
    Ok(())
}

fn pump(
    mut source: impl Read,
    sink: &mut impl Write,
    (counter, relayed): (&AtomicU64, &AtomicU64),
    ceiling: u64,
    shared: &BrokerShared,
    last_activity: &Mutex<Instant>,
    ended: &AtomicBool,
) {
    let idle = Duration::from_millis(shared.policy.limits.idle_timeout_ms);
    let mut buffer = vec![0_u8; RELAY_BUFFER_BYTES];
    loop {
        if shared.stopped() || ended.load(Ordering::SeqCst) {
            break;
        }
        match source.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                if charge(counter, read, ceiling).is_err() {
                    shared.ledger().receipt.byte_ceiling_reached = true;
                    ended.store(true, Ordering::SeqCst);
                    break;
                }
                if sink.write_all(&buffer[..read]).is_err() {
                    ended.store(true, Ordering::SeqCst);
                    break;
                }
                relayed.fetch_add(read as u64, Ordering::SeqCst);
                *last_activity
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = Instant::now();
            },
            Err(error) if is_tick(&error) => {
                let quiet = last_activity
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .elapsed();
                if quiet >= idle {
                    ended.store(true, Ordering::SeqCst);
                    break;
                }
            },
            Err(_) => {
                ended.store(true, Ordering::SeqCst);
                break;
            },
        }
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;

    fn declaration(destination: &str) -> String {
        format!(
            "---\nname: demo\ndescription: demo\nmetadata:\n  magician:\n    other: 1\n    app_egress:\n      schema_version: 1\n      destination: {destination}\n---\nbody\n"
        )
    }

    #[test]
    fn a_skill_declares_one_public_https_host_or_nothing() {
        let parsed = parse_app_egress_declaration(&declaration("Export.ArXiv.org.")).unwrap();
        assert_eq!(parsed.unwrap().destination(), "export.arxiv.org");
        assert_eq!(
            parse_app_egress_declaration("---\nname: demo\ndescription: d\n---\n").unwrap(),
            None
        );
        for refused in [
            "localhost",
            "printer.local",
            "10.0.0.1",
            "api.example.com:8443",
            "*.example.com",
            "example",
            "https://example.com",
            "a..b.com",
            "-a.example.com",
            "example.123",
        ] {
            assert_eq!(
                parse_app_egress_declaration(&declaration(refused)),
                Err(AppOsJailEgressError::InvalidDeclaration),
                "{refused}"
            );
        }
        let unknown = "---\nname: demo\ndescription: d\nmetadata:\n  magician:\n    app_egress:\n      schema_version: 1\n      destination: example.com\n      port: 80\n---\n";
        assert_eq!(
            parse_app_egress_declaration(unknown),
            Err(AppOsJailEgressError::InvalidDeclaration)
        );
        let future = "---\nname: demo\ndescription: d\nmetadata:\n  magician:\n    app_egress:\n      schema_version: 2\n      destination: example.com\n---\n";
        assert_eq!(
            parse_app_egress_declaration(future),
            Err(AppOsJailEgressError::InvalidDeclaration)
        );
    }

    /// A fake "public internet": the resolver maps the granted host to a
    /// local echo server and the address check admits loopback, standing in
    /// for a real public address.
    struct Upstream {
        port: u16,
    }

    impl Upstream {
        fn echo() -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    thread::spawn(move || {
                        let mut buffer = [0_u8; 4096];
                        while let Ok(read) = stream.read(&mut buffer) {
                            if read == 0 || stream.write_all(&buffer[..read]).is_err() {
                                break;
                            }
                        }
                    });
                }
            });
            Self { port }
        }
    }

    static ECHO_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

    fn echo_resolver(_host: &str, _port: u16) -> io::Result<Vec<SocketAddr>> {
        Ok(vec![SocketAddr::from((
            [127, 0, 0, 1],
            ECHO_PORT.load(Ordering::SeqCst),
        ))])
    }

    fn admit_any(_host: &str, _addresses: &[SocketAddr]) -> bool {
        true
    }

    fn broker_with(
        limits: AppOsJailEgressLimits,
        check: fn(&str, &[SocketAddr]) -> bool,
        dial_port: u16,
    ) -> AppOsJailEgressBroker {
        broker_admitting(
            AppOsJailEgressAdmission::Hosts {
                hosts: vec!["api.example.com".to_owned()],
            },
            limits,
            check,
            dial_port,
        )
    }

    fn broker_admitting(
        admission: AppOsJailEgressAdmission,
        limits: AppOsJailEgressLimits,
        check: fn(&str, &[SocketAddr]) -> bool,
        dial_port: u16,
    ) -> AppOsJailEgressBroker {
        AppOsJailEgressBroker::start_with(BrokerPolicy {
            admission,
            limits,
            resolver: echo_resolver,
            address_check: check,
            dial_port,
        })
        .unwrap()
    }

    fn broker(
        limits: AppOsJailEgressLimits,
        check: fn(&str, &[SocketAddr]) -> bool,
    ) -> AppOsJailEgressBroker {
        broker_with(limits, check, APP_OS_JAIL_EGRESS_PORT)
    }

    /// Tunnel tests share one echo upstream, so they run one at a time.
    static TUNNEL: Mutex<()> = Mutex::new(());

    fn tunnel_broker(
        limits: AppOsJailEgressLimits,
    ) -> (std::sync::MutexGuard<'static, ()>, AppOsJailEgressBroker) {
        let guard = TUNNEL.lock().unwrap_or_else(|poison| poison.into_inner());
        let upstream = Upstream::echo();
        ECHO_PORT.store(upstream.port, Ordering::SeqCst);
        let broker = broker_with(limits, admit_any, upstream.port);
        (guard, broker)
    }

    fn connect(broker: &AppOsJailEgressBroker) -> ClientStream {
        let stream = match broker.endpoint() {
            GovernedEgressBrokerEndpoint::LoopbackTcp { port } => {
                ClientStream::Tcp(TcpStream::connect(("127.0.0.1", port.get())).unwrap())
            },
            GovernedEgressBrokerEndpoint::UnixSocket { path } => {
                ClientStream::Unix(std::os::unix::net::UnixStream::connect(path).unwrap())
            },
        };
        stream.prepare(Duration::from_secs(5)).unwrap();
        stream
    }

    fn status_line(stream: &mut ClientStream) -> String {
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while find_head_end(&head).is_none() {
            match stream.read(&mut byte) {
                Ok(0) | Err(_) => break,
                Ok(_) => head.push(byte[0]),
            }
        }
        String::from_utf8_lossy(&head)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned()
    }

    #[test]
    fn a_granted_connect_tunnels_bytes_both_ways_and_counts_them() {
        let (_guard, broker) = tunnel_broker(APP_OS_JAIL_EGRESS_LIMITS);
        let mut client = connect(&broker);
        // The first tunnel bytes ride in the same write as the request head.
        client
            .write_all(
                b"CONNECT API.example.com:443 HTTP/1.1\r\nHost: api.example.com:443\r\n\r\nhello",
            )
            .unwrap();
        assert_eq!(
            status_line(&mut client),
            "HTTP/1.1 200 Connection Established"
        );
        let mut echoed = [0_u8; 5];
        client.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"hello");
        client.write_all(b" world").unwrap();
        let mut more = [0_u8; 6];
        client.read_exact(&mut more).unwrap();
        assert_eq!(&more, b" world");
        client.shutdown(Shutdown::Both);
        let receipt = broker.finish();
        assert_eq!(receipt.connections_tunnelled, 1);
        assert_eq!(receipt.bytes_up, 11);
        assert_eq!(receipt.bytes_down, 11);
        assert!(!receipt.byte_ceiling_reached);
        assert_eq!(receipt.profile, APP_OS_JAIL_EGRESS_PROFILE_V1);
    }

    #[test]
    fn the_upload_ceiling_ends_the_tunnel() {
        let limits = AppOsJailEgressLimits {
            max_bytes_up: 8,
            ..APP_OS_JAIL_EGRESS_LIMITS
        };
        let (_guard, broker) = tunnel_broker(limits);
        let mut client = connect(&broker);
        client
            .write_all(b"CONNECT api.example.com:443 HTTP/1.1\r\n\r\n")
            .unwrap();
        assert_eq!(
            status_line(&mut client),
            "HTTP/1.1 200 Connection Established"
        );
        let _ = client.write_all(&[7_u8; 64]);
        let mut sink = Vec::new();
        let _ = client.read_to_end(&mut sink);
        assert!(sink.len() <= 8, "nothing past the ceiling is relayed");
        let receipt = broker.finish();
        assert!(receipt.byte_ceiling_reached);
        assert_eq!(receipt.bytes_down, 0);
    }

    #[test]
    fn the_broker_dials_only_the_https_port() {
        let (_guard, _unused) = tunnel_broker(APP_OS_JAIL_EGRESS_LIMITS);
        // Production policy: the echo upstream is not on 443, so the
        // resolved address is skipped and nothing is dialled.
        let broker = broker(APP_OS_JAIL_EGRESS_LIMITS, admit_any);
        let mut client = connect(&broker);
        client
            .write_all(b"CONNECT api.example.com:443 HTTP/1.1\r\n\r\n")
            .unwrap();
        assert_eq!(status_line(&mut client), "HTTP/1.1 502 Bad Gateway");
        let receipt = broker.finish();
        assert_eq!(receipt.connections_failed, 1);
        assert_eq!(receipt.connections_tunnelled, 0);
    }

    #[test]
    fn other_hosts_ports_and_methods_are_refused_before_any_lookup() {
        let broker = broker(APP_OS_JAIL_EGRESS_LIMITS, admit_any);
        for (request, expected) in [
            (
                "CONNECT evil.example.net:443 HTTP/1.1\r\n\r\n",
                "HTTP/1.1 403 Forbidden",
            ),
            (
                "CONNECT api.example.com:80 HTTP/1.1\r\n\r\n",
                "HTTP/1.1 403 Forbidden",
            ),
            (
                "GET http://api.example.com/ HTTP/1.1\r\nHost: api.example.com\r\n\r\n",
                "HTTP/1.1 403 Forbidden",
            ),
            (
                "CONNECT api.example.com:443\r\n\r\n",
                "HTTP/1.1 400 Bad Request",
            ),
        ] {
            let mut client = connect(&broker);
            client.write_all(request.as_bytes()).unwrap();
            assert_eq!(status_line(&mut client), expected, "{request:?}");
        }
        let receipt = broker.finish();
        assert_eq!(receipt.connections_refused, 4);
        assert_eq!(
            receipt.refusals,
            [
                "host_not_granted",
                "port_not_granted",
                "https_connect_only",
                "malformed_request_line"
            ]
        );
        assert_eq!(receipt.destination, "api.example.com");
    }

    #[test]
    fn a_granted_host_resolving_to_a_private_address_is_refused() {
        let broker = broker(APP_OS_JAIL_EGRESS_LIMITS, public_addresses);
        let mut client = connect(&broker);
        client
            .write_all(b"CONNECT api.example.com:443 HTTP/1.1\r\n\r\n")
            .unwrap();
        assert_eq!(status_line(&mut client), "HTTP/1.1 403 Forbidden");
        assert_eq!(broker.finish().refusals, ["non_public_address"]);
    }

    #[test]
    fn the_connection_ceiling_refuses_the_extra_connection() {
        let limits = AppOsJailEgressLimits {
            max_connections: 1,
            ..APP_OS_JAIL_EGRESS_LIMITS
        };
        let broker = broker(limits, admit_any);
        let mut first = connect(&broker);
        first
            .write_all(b"CONNECT evil.example.net:443 HTTP/1.1\r\n\r\n")
            .unwrap();
        assert_eq!(status_line(&mut first), "HTTP/1.1 403 Forbidden");
        let mut second = connect(&broker);
        assert_eq!(status_line(&mut second), "HTTP/1.1 503 Service Unavailable");
        let receipt = broker.finish();
        assert_eq!(receipt.refusals, ["host_not_granted", "connection_ceiling"]);
    }

    /// Live: the real broker, real DNS and the real arXiv API. Run with
    /// `--ignored`; needs network.
    #[test]
    #[ignore = "reaches export.arxiv.org"]
    fn live_arxiv_through_the_broker_with_curl_and_python() {
        let declaration = parse_app_egress_declaration(&declaration("export.arxiv.org"))
            .unwrap()
            .unwrap();
        let admission = AppOsJailEgressAdmission::for_call(Some(&declaration), None, false).unwrap();
        let broker = AppOsJailEgressBroker::start(&admission).unwrap();
        let GovernedEgressBrokerEndpoint::LoopbackTcp { port } = broker.endpoint() else {
            return;
        };
        let proxy = format!("http://127.0.0.1:{}", port.get());
        let url = "https://export.arxiv.org/api/query?search_query=all:agents&max_results=1";
        let curl = std::process::Command::new("/usr/bin/curl")
            .args([
                "-sS",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                "--proxy",
                &proxy,
                url,
            ])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&curl.stdout), "200", "{curl:?}");
        let script = format!(
            "import urllib.request;print(urllib.request.urlopen('{url}',timeout=20).status)"
        );
        let python = std::process::Command::new("/usr/bin/python3")
            .args(["-c", &script])
            .env("HTTPS_PROXY", &proxy)
            .env("https_proxy", &proxy)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&python.stdout).trim(),
            "200",
            "{python:?}"
        );
        let refused = std::process::Command::new("/usr/bin/curl")
            .args([
                "-sS",
                "-o",
                "/dev/null",
                "--proxy",
                &proxy,
                "https://example.com/",
            ])
            .output()
            .unwrap();
        assert!(!refused.status.success());
        let receipt = broker.finish();
        assert_eq!(receipt.connections_tunnelled, 2);
        assert_eq!(receipt.refusals, ["host_not_granted"]);
        assert!(receipt.bytes_down > 0 && receipt.bytes_up > 0);
    }

    fn declaration_list(destinations: &[&str]) -> String {
        let list = destinations
            .iter()
            .map(|destination| format!("        - {destination}\n"))
            .collect::<String>();
        format!(
            "---\nname: demo\ndescription: demo\nmetadata:\n  magician:\n    app_egress:\n      schema_version: 1\n      destinations:\n{list}---\nbody\n"
        )
    }

    #[test]
    fn a_skill_may_declare_a_bounded_host_list() {
        let parsed =
            parse_app_egress_declaration(&declaration_list(&["b.example.com", "A.example.com."]))
                .unwrap()
                .unwrap();
        assert_eq!(parsed.destinations(), ["a.example.com", "b.example.com"]);
        assert!(!parsed.any_host());
        let any = parse_app_egress_declaration(&declaration_list(&["\"*\""]))
            .unwrap()
            .unwrap();
        assert!(any.any_host());
        for refused in [
            declaration_list(&[]),
            declaration_list(&["a.example.com", "a.example.com"]),
            declaration_list(&["a.example.com", "localhost"]),
            declaration_list(
                &(0..=MAX_APP_OS_JAIL_EGRESS_DESTINATIONS)
                    .map(|index| format!("h{index}.example.com"))
                    .collect::<Vec<_>>()
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            ),
            // `*` only as the sole entry.
            declaration_list(&["*", "a.example.com"]),
            // Both forms at once is ambiguous.
            "---\nname: d\ndescription: d\nmetadata:\n  magician:\n    app_egress:\n      schema_version: 1\n      destination: a.example.com\n      destinations: [b.example.com]\n---\n".to_owned(),
        ] {
            assert_eq!(
                parse_app_egress_declaration(&refused),
                Err(AppOsJailEgressError::InvalidDeclaration),
                "{refused}"
            );
        }
    }

    #[test]
    fn a_call_admits_declared_and_granted_hosts_or_any_public_host() {
        let declared =
            parse_app_egress_declaration(&declaration_list(&["a.example.com", "b.example.com"]))
                .unwrap()
                .unwrap();
        let grant = |hosts: &[&str], any: bool| {
            AppOsJailNetworkGrant::from_destination_refs(
                hosts
                    .iter()
                    .map(|host| format!("destination:{host}"))
                    .collect::<Vec<_>>()
                    .iter()
                    .map(String::as_str),
                any,
            )
        };
        // No grant resolved: the declaration, as before.
        assert_eq!(
            AppOsJailEgressAdmission::for_call(Some(&declared), None, false),
            Some(AppOsJailEgressAdmission::Hosts {
                hosts: vec!["a.example.com".into(), "b.example.com".into()]
            })
        );
        // Declared ∩ granted.
        let admission = AppOsJailEgressAdmission::for_call(
            Some(&declared),
            Some(&grant(&["b.example.com", "c.example.com"], false)),
            false,
        )
        .unwrap();
        assert_eq!(
            admission,
            AppOsJailEgressAdmission::Hosts {
                hosts: vec!["b.example.com".into()]
            }
        );
        assert!(admission.admits("b.example.com"));
        assert!(!admission.admits("a.example.com"));
        assert!(!admission.admits("c.example.com"));
        assert_eq!(admission.single_host(), Some("b.example.com"));
        // "Any public host" never widens a tool that declares its hosts.
        assert_eq!(
            AppOsJailEgressAdmission::for_call(
                Some(&declared),
                Some(&grant(&["b.example.com"], true)),
                true,
            ),
            Some(AppOsJailEgressAdmission::Hosts {
                hosts: vec!["b.example.com".into()]
            })
        );
        // Nothing granted: every declared host is attested, so disclosure refuses.
        assert_eq!(
            AppOsJailEgressAdmission::for_call(Some(&declared), Some(&grant(&[], false)), false)
                .unwrap()
                .destination_refs(),
            ["destination:a.example.com", "destination:b.example.com"]
        );
        // An in-place skill that declares nothing: every granted host.
        let undeclared = AppOsJailEgressAdmission::for_call(
            None,
            Some(&grant(&["api.minimax.io", "not a host"], false)),
            true,
        )
        .unwrap();
        assert_eq!(
            undeclared,
            AppOsJailEgressAdmission::Hosts {
                hosts: vec!["api.minimax.io".into()]
            }
        );
        // A copied single-file skill that declares nothing never gets network.
        assert_eq!(
            AppOsJailEgressAdmission::for_call(None, Some(&grant(&["a.example.com"], true)), false),
            None
        );
        // No grant for an in-place skill: no network.
        assert_eq!(
            AppOsJailEgressAdmission::for_call(None, Some(&grant(&[], false)), true),
            None
        );
        // `destinations: ["*"]` needs the app's any-host grant: without it
        // there is no network at all.
        let star = parse_app_egress_declaration(&declaration_list(&["\"*\""]))
            .unwrap()
            .unwrap();
        assert_eq!(
            AppOsJailEgressAdmission::for_call(
                Some(&star),
                Some(&grant(&["a.example.com"], false)),
                false
            ),
            None
        );
        assert_eq!(
            AppOsJailEgressAdmission::for_call(Some(&star), Some(&grant(&[], true)), false),
            Some(AppOsJailEgressAdmission::AnyPublicHost)
        );
        // No grant resolved: no network, never any site.
        assert_eq!(
            AppOsJailEgressAdmission::for_call(Some(&star), None, false),
            None
        );
        assert_eq!(
            AppOsJailEgressAdmission::for_call(Some(&star), None, true),
            None
        );
        // Any public host: DNS names only; the broker still resolves and
        // refuses private addresses.
        let any = AppOsJailEgressAdmission::for_call(None, Some(&grant(&[], true)), true).unwrap();
        assert_eq!(any, AppOsJailEgressAdmission::AnyPublicHost);
        assert!(any.admits("example.org"));
        assert!(!any.admits("localhost"));
        assert!(!any.admits("10.0.0.1"));
        assert!(!any.admits("printer.local"));
        assert_eq!(any.single_host(), None);
        assert_eq!(any.destination_refs(), [APP_OS_JAIL_ANY_PUBLIC_HOST_REF]);
    }

    #[test]
    fn the_broker_admits_only_its_admitted_hosts_and_records_each_one() {
        let (_guard, _unused) = tunnel_broker(APP_OS_JAIL_EGRESS_LIMITS);
        let port = ECHO_PORT.load(Ordering::SeqCst);
        let broker = broker_admitting(
            AppOsJailEgressAdmission::Hosts {
                hosts: vec!["a.example.com".into(), "b.example.com".into()],
            },
            APP_OS_JAIL_EGRESS_LIMITS,
            admit_any,
            port,
        );
        for (host, expected) in [
            ("a.example.com", "HTTP/1.1 200 Connection Established"),
            ("b.example.com", "HTTP/1.1 200 Connection Established"),
            ("b.example.com", "HTTP/1.1 200 Connection Established"),
            ("c.example.com", "HTTP/1.1 403 Forbidden"),
        ] {
            let mut client = connect(&broker);
            client
                .write_all(format!("CONNECT {host}:443 HTTP/1.1\r\n\r\nping").as_bytes())
                .unwrap();
            assert_eq!(status_line(&mut client), expected, "{host}");
            if expected.ends_with("Established") {
                let mut echoed = [0_u8; 4];
                client.read_exact(&mut echoed).unwrap();
            }
            client.shutdown(Shutdown::Both);
        }
        let receipt = broker.finish();
        assert_eq!(receipt.destination, "multiple");
        assert_eq!(receipt.refusals, ["host_not_granted"]);
        assert_eq!(
            receipt
                .contacted
                .iter()
                .map(|usage| (usage.host.as_str(), usage.connections, usage.bytes_up))
                .collect::<Vec<_>>(),
            [("a.example.com", 1, 4), ("b.example.com", 2, 8)]
        );

        let any = broker_admitting(
            AppOsJailEgressAdmission::AnyPublicHost,
            APP_OS_JAIL_EGRESS_LIMITS,
            admit_any,
            port,
        );
        for (host, expected) in [
            ("anything.example.net", "HTTP/1.1 200 Connection Established"),
            ("127.0.0.1", "HTTP/1.1 403 Forbidden"),
        ] {
            let mut client = connect(&any);
            client
                .write_all(format!("CONNECT {host}:443 HTTP/1.1\r\n\r\n").as_bytes())
                .unwrap();
            assert_eq!(status_line(&mut client), expected, "{host}");
            client.shutdown(Shutdown::Both);
        }
        let receipt = any.finish();
        assert_eq!(receipt.destination, "any-public-host");
        assert_eq!(receipt.contacted.len(), 1);
        assert_eq!(receipt.contacted[0].host, "anything.example.net");
    }

    #[test]
    fn any_public_host_still_refuses_a_private_address() {
        let broker = broker_admitting(
            AppOsJailEgressAdmission::AnyPublicHost,
            APP_OS_JAIL_EGRESS_LIMITS,
            public_addresses,
            APP_OS_JAIL_EGRESS_PORT,
        );
        let mut client = connect(&broker);
        client
            .write_all(b"CONNECT internal.example.com:443 HTTP/1.1\r\n\r\n")
            .unwrap();
        assert_eq!(status_line(&mut client), "HTTP/1.1 403 Forbidden");
        assert_eq!(broker.finish().refusals, ["non_public_address"]);
    }

    #[test]
    fn finish_ends_a_client_that_never_sends_a_request() {
        let broker = broker(APP_OS_JAIL_EGRESS_LIMITS, admit_any);
        let _idle = connect(&broker);
        thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        let receipt = broker.finish();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(receipt.refusals, ["request_head_timeout"]);
    }
}
