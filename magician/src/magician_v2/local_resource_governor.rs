//! Process-local resource governor plus Magician-host RSS probe install.
//!
//! Admission, counters, and the RSS tripwire live in `magician-core` (no
//! `sysinfo` dependency). This module re-exports that API and installs a
//! process RSS probe after config load. Magician-bin already calls
//! [`configure_live_agent_limit`] from the resolved runtime plan. PR8 should
//! also call [`configure_per_agent_outstanding_limit`] and
//! [`configure_rss_tripwire`]. Until tripwire bytes are set, RSS is measured
//! but does not reject.

pub use magician_core::local_resource_governor::*;

/// Install a `sysinfo` RSS probe for the governor tripwire.
///
/// Safe to call more than once; the first probe wins (`OnceLock` in
/// magician-core). Call after Magician config is loaded. PR8 owns YAML and
/// `magician-bin` boot-plan wiring; this function is the host-side install
/// site.
pub fn install_process_rss_probe() {
    configure_rss_probe(current_process_rss_bytes);
}

fn current_process_rss_bytes() -> u64 {
    use sysinfo::{Pid, System};
    let mut system = System::new();
    let pid = Pid::from_u32(std::process::id());
    if !system.refresh_process(pid) {
        return 0;
    }
    system
        .process(pid)
        .map(|process| process.memory())
        .unwrap_or(0)
}
