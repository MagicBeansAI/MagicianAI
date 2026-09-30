//! Pre-start port availability checks.
//!
//! Before starting the container, we attempt to bind each configured host port
//! to detect conflicts early. If a port is occupied, we identify the holding
//! process so the user can decide whether to kill it or change ports.

use serde::{Deserialize, Serialize};
use std::net::TcpListener;
use tokio::process::Command;
use tracing::{info, warn};

/// Information about a process occupying a port.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortConflict {
    pub port: u16,
    pub pid: Option<u32>,
    pub process_name: Option<String>,
    /// true when the port holder is our own container name (zombie/stale)
    pub is_own_container: bool,
}

/// Result of checking all required ports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortCheckResult {
    pub conflicts: Vec<PortConflict>,
}

impl PortCheckResult {
    pub fn all_clear(&self) -> bool {
        self.conflicts.is_empty()
    }
}

/// Check whether each host port in `ports` is available for binding.
/// Returns details about any conflicts found.
pub async fn check_ports(ports: &[(u16, u16)], container_name: &str) -> PortCheckResult {
    let mut conflicts = Vec::new();

    for &(host_port, _container_port) in ports {
        if !is_port_available(host_port) {
            let holder = identify_port_holder(host_port).await;
            let is_own = holder
                .as_ref()
                .and_then(|(_, name)| name.as_ref())
                .map(|n| {
                    n.contains(container_name)
                        || n.contains("docker-proxy")
                        || n.contains("com.apple.container")
                })
                .unwrap_or(false);

            let conflict = PortConflict {
                port: host_port,
                pid: holder.as_ref().map(|(pid, _)| *pid),
                process_name: holder.and_then(|(_, name)| name),
                is_own_container: is_own,
            };
            warn!(
                "Port {} is occupied by {:?} (pid: {:?}, own: {})",
                host_port, conflict.process_name, conflict.pid, conflict.is_own_container
            );
            conflicts.push(conflict);
        }
    }

    PortCheckResult { conflicts }
}

/// Try to bind a TCP listener to `127.0.0.1:port`. Returns true if the port is free.
fn is_port_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Identify which process holds a given port. Returns (pid, process_name).
async fn identify_port_holder(port: u16) -> Option<(u32, Option<String>)> {
    if cfg!(target_os = "macos") || cfg!(target_os = "linux") {
        identify_port_holder_unix(port).await
    } else {
        // Windows: netstat parsing would go here
        None
    }
}

/// Use `lsof` to find the process holding a port on macOS/Linux.
async fn identify_port_holder_unix(port: u16) -> Option<(u32, Option<String>)> {
    let output = Command::new("lsof")
        .args(["-i", &format!(":{}", port), "-t", "-sTCP:LISTEN"])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let pid: u32 = stdout.lines().next()?.trim().parse().ok()?;

    // Get process name from pid
    let name = get_process_name(pid).await;

    Some((pid, name))
}

/// Get the process name for a given PID.
async fn get_process_name(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .await
        .ok()?;

    if output.status.success() {
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

/// Kill a process by PID. Used when the user chooses to free a conflicted port.
pub async fn kill_port_holder(pid: u32) -> Result<(), String> {
    info!("Killing process {} to free port", pid);

    // Try SIGTERM first
    let output = Command::new("kill")
        .arg(pid.to_string())
        .output()
        .await
        .map_err(|e| format!("Failed to kill process {}: {}", pid, e))?;

    if !output.status.success() {
        // Try SIGKILL as fallback
        warn!("SIGTERM failed for pid {}, trying SIGKILL", pid);
        let output = Command::new("kill")
            .args(["-9", &pid.to_string()])
            .output()
            .await
            .map_err(|e| format!("Failed to force-kill process {}: {}", pid, e))?;

        if !output.status.success() {
            return Err(format!(
                "Could not kill process {} — you may need to stop it manually",
                pid
            ));
        }
    }

    // Brief wait for the port to be released
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    Ok(())
}

/// Attempt to free all conflicted ports by killing their holders.
/// Returns the list of ports that are still occupied after the attempt.
pub async fn free_conflicted_ports(conflicts: &[PortConflict]) -> Vec<u16> {
    let mut still_blocked = Vec::new();

    for conflict in conflicts {
        if let Some(pid) = conflict.pid {
            if let Err(e) = kill_port_holder(pid).await {
                warn!("Could not free port {}: {}", conflict.port, e);
                still_blocked.push(conflict.port);
            } else if !is_port_available(conflict.port) {
                warn!(
                    "Port {} still occupied after killing pid {}",
                    conflict.port, pid
                );
                still_blocked.push(conflict.port);
            } else {
                info!("Freed port {} (killed pid {})", conflict.port, pid);
            }
        } else {
            still_blocked.push(conflict.port);
        }
    }

    still_blocked
}
