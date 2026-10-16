//! Where the shell and `http` tools may connect (`execution.network_policy`).
//!
//! - `allow`: no gate (the default for a config without the block).
//! - `public`: the internet, but not loopback, private, link-local, CGNAT,
//!   cloud-metadata or other non-public addresses. `allow_private` lists
//!   `host` / `host:port` exceptions. The shell reaches the network only
//!   through the egress proxy (`egress_proxy`), which applies this; the `http`
//!   tool applies it in process through a filtering DNS resolver.
//! - `deny`: no network.
//!
//! A destination is public only when every address it resolves to is public,
//! and the connection goes to an address that was checked, so a DNS answer
//! cannot rebind it. God mode may lift a refusal for one call
//! (`god_mode::GodAction::Network`).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, OnceLock, RwLock};

use crate::config::{NetworkMode, NetworkPolicySettings};

fn cell() -> &'static RwLock<NetworkPolicySettings> {
    static CELL: OnceLock<RwLock<NetworkPolicySettings>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(NetworkPolicySettings::default()))
}

/// Publish `execution.network_policy`: at boot, on config reload.
pub fn configure(settings: &NetworkPolicySettings) {
    *cell().write().unwrap_or_else(|poisoned| poisoned.into_inner()) = settings.clone();
}

pub fn settings() -> NetworkPolicySettings {
    #[cfg(test)]
    if let Some(local) = TEST_OVERRIDE.with(|cell| cell.borrow().clone()) {
        return local;
    }
    cell().read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}

#[cfg(test)]
thread_local! {
    static TEST_OVERRIDE: std::cell::RefCell<Option<NetworkPolicySettings>> =
        const { std::cell::RefCell::new(None) };
}

/// Tests: a policy for this thread only, so it cannot leak into a test
/// running beside it. Cleared when the guard drops.
#[cfg(test)]
pub(crate) fn override_for_test(settings: NetworkPolicySettings) -> TestOverride {
    TEST_OVERRIDE.with(|cell| *cell.borrow_mut() = Some(settings));
    TestOverride
}

#[cfg(test)]
pub(crate) struct TestOverride;

#[cfg(test)]
impl Drop for TestOverride {
    fn drop(&mut self) {
        TEST_OVERRIDE.with(|cell| *cell.borrow_mut() = None);
    }
}

pub fn mode() -> NetworkMode {
    settings().mode
}

/// Whether `ip` is a public internet address.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || a == 0
        // CGNAT 100.64.0.0/10.
        || (a == 100 && (b & 0xc0) == 64)
        // IETF protocol assignments 192.0.0.0/24.
        || (a == 192 && b == 0 && c == 0)
        // Benchmarking 198.18.0.0/15.
        || (a == 198 && (b & 0xfe) == 18)
        // Reserved 240.0.0.0/4.
        || a >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let first = ip.segments()[0];
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        // Unique local fc00::/7.
        || (first & 0xfe00) == 0xfc00
        // Link-local fe80::/10 and the deprecated site-local fec0::/10.
        || (first & 0xffc0) == 0xfe80
        || (first & 0xffc0) == 0xfec0
        // Documentation 2001:db8::/32.
        || (first == 0x2001 && ip.segments()[1] == 0x0db8)
        // NAT64 well-known prefix 64:ff9b::/96 can reach private v4.
        || (first == 0x0064 && ip.segments()[1] == 0xff9b))
}

/// Whether `host[:port]` is on `allow_private`.
pub fn allowed_private(host: &str, port: u16) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    settings().allow_private.iter().any(|entry| {
        let entry = entry.trim().to_ascii_lowercase();
        match entry.rsplit_once(':') {
            Some((entry_host, entry_port)) if entry_port.parse::<u16>().is_ok() => {
                entry_host.trim_start_matches('[').trim_end_matches(']') == host
                    && entry_port.parse::<u16>() == Ok(port)
            },
            _ => entry == host,
        }
    })
}

/// Why `addrs` (what `host` resolved to) may not be reached under the
/// current policy, or `None` when they may.
pub fn refusal(host: &str, port: u16, addrs: &[SocketAddr]) -> Option<String> {
    match mode() {
        NetworkMode::Allow => None,
        NetworkMode::Deny => Some(format!("network access is off (network_policy: deny); {host}:{port} was not contacted")),
        NetworkMode::Public => {
            if allowed_private(host, port) {
                return None;
            }
            if addrs.is_empty() {
                return Some(format!("{host} did not resolve"));
            }
            addrs.iter().find(|addr| !is_public_ip(addr.ip())).map(|addr| {
                format!(
                    "{host}:{port} resolves to a non-public address ({}); network_policy is public: loopback, \
                     private, link-local and metadata addresses are blocked",
                    addr.ip()
                )
            })
        },
    }
}

/// Resolve `host:port` and check it. `Ok` carries the vetted addresses.
pub async fn resolve_checked(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<SocketAddr> = match bare.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((bare, port))
            .await
            .map_err(|error| format!("{host} did not resolve: {error}"))?
            .collect(),
    };
    match refusal(host, port, &addrs) {
        Some(reason) => Err(reason),
        None => Ok(addrs),
    }
}

/// A reqwest DNS resolver that refuses names resolving to a non-public
/// address under `public`, for every connection the client makes (redirects
/// included). Ports are not known here; `allow_private` is applied by the
/// caller before it chooses this resolver.
#[derive(Debug, Default)]
pub struct PublicOnlyResolver;

impl reqwest::dns::Resolve for PublicOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> { Box::new(error) })?
                .collect();
            if let Some(reason) = refusal(&host, 0, &addrs) {
                return Err(reason.into());
            }
            let iter: reqwest::dns::Addrs = Box::new(addrs.into_iter());
            Ok(iter)
        })
    }
}

/// The resolver for the `http` tool under the current policy, if any.
pub fn http_resolver() -> Option<Arc<PublicOnlyResolver>> {
    (mode() == NetworkMode::Public).then(|| Arc::new(PublicOnlyResolver))
}

/// Serializes tests that publish the process-wide policy (the egress proxy
/// reads it on its own thread), and tests that need the default `allow`.
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn only_public_addresses_are_public() {
        for public in ["93.184.216.34", "1.1.1.1", "8.8.8.8", "2606:4700::1111"] {
            assert!(is_public_ip(ip(public)), "{public}");
        }
        for private in [
            "127.0.0.1", "10.1.2.3", "172.16.0.1", "192.168.1.1", "169.254.169.254", "100.64.0.1",
            "0.0.0.0", "255.255.255.255", "224.0.0.1", "198.18.0.1", "192.0.2.1", "240.0.0.1",
            "::1", "::", "fc00::1", "fd12::1", "fe80::1", "fec0::1", "2001:db8::1",
            "::ffff:127.0.0.1", "::ffff:10.0.0.1", "64:ff9b::a00:1",
        ] {
            assert!(!is_public_ip(ip(private)), "{private}");
        }
    }

    #[test]
    fn the_mode_and_exceptions_decide() {
        let loopback = [SocketAddr::new(ip("127.0.0.1"), 11434)];
        let public = [SocketAddr::new(ip("93.184.216.34"), 443)];
        let policy = |mode, allow_private: &[&str]| {
            override_for_test(NetworkPolicySettings {
                mode,
                allow_private: allow_private.iter().map(|s| s.to_string()).collect(),
            })
        };
        {
            let _allow = policy(NetworkMode::Allow, &[]);
            assert!(refusal("localhost", 11434, &loopback).is_none());
        }
        {
            let _public = policy(NetworkMode::Public, &[]);
            assert!(refusal("localhost", 11434, &loopback).is_some());
            assert!(refusal("example.com", 443, &public).is_none());
            assert!(
                refusal("mixed.example", 443, &[public[0], loopback[0]]).is_some(),
                "every address must be public"
            );
        }
        {
            let _exceptions = policy(NetworkMode::Public, &["localhost:11434", "nas.local"]);
            assert!(refusal("localhost", 11434, &loopback).is_none());
            assert!(refusal("localhost", 3002, &loopback).is_some(), "the port must match");
            assert!(refusal("NAS.local", 80, &loopback).is_none(), "a bare host covers every port");
        }
        {
            let _deny = policy(NetworkMode::Deny, &[]);
            assert!(refusal("example.com", 443, &public).is_some());
        }
        assert_eq!(mode(), settings().mode, "the override is gone once dropped");
    }
}
