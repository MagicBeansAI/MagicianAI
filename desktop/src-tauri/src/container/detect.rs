use std::sync::Arc;
use tracing::{info, warn};

use super::apple::AppleContainerRuntime;
use super::docker::DockerRuntime;
use super::ContainerRuntime;

/// Which container runtime was detected on this system.
#[derive(Debug, Clone, PartialEq)]
pub enum DetectedRuntime {
    /// Native Apple container CLI (macOS >= 26 + Apple Silicon)
    AppleContainer,
    /// Docker CLI (via Docker Desktop, Colima, or native)
    Docker,
}

/// Detect the best container runtime for the current platform.
///
/// Priority:
/// 1. Apple Container (if macOS >= 26 AND aarch64)
/// 2. Docker (fallback for all platforms)
pub async fn detect_runtime() -> Result<(DetectedRuntime, Arc<dyn ContainerRuntime>), String> {
    // Check Apple Container first (macOS 26+ on Apple Silicon)
    if is_apple_container_eligible() {
        let apple = AppleContainerRuntime::new();
        if apple.is_available().await {
            info!("Detected Apple Container runtime (native)");
            return Ok((DetectedRuntime::AppleContainer, Arc::new(apple)));
        }
        // Apple container eligible but CLI not installed yet -- we can install it later
        info!("Apple Container eligible but CLI not yet installed; will use Apple runtime with install step");
        return Ok((DetectedRuntime::AppleContainer, Arc::new(apple)));
    }

    // Fallback to Docker. `docker info` covers Docker Desktop, a Colima Docker
    // context, and native Docker Engine without guessing from process names.
    let docker = DockerRuntime::new();
    if docker.is_available().await {
        info!("Detected Docker runtime");
        return Ok((DetectedRuntime::Docker, Arc::new(docker)));
    }

    // macOS owns the runtime setup experience. Ineligible Macs use Colima;
    // eligible Apple Silicon Macs have already selected Apple Container above.
    if std::env::consts::OS == "macos" {
        info!("Docker is not running; macOS setup will install/start Colima");
        return Ok((DetectedRuntime::Docker, Arc::new(docker)));
    }

    Err(container_runtime_prerequisite_error())
}

pub(crate) fn container_runtime_prerequisite_error() -> String {
    container_runtime_prerequisite_error_for(
        std::env::consts::OS,
        DockerRuntime::cli_is_installed(),
    )
}

fn container_runtime_prerequisite_error_for(os: &str, cli_installed: bool) -> String {
    match (os, cli_installed) {
        ("windows", true) => "Docker Desktop is installed, but its Docker engine is not running. Start Docker Desktop, complete any WSL or virtualization requirement, and retry Magican setup.".to_string(),
        ("windows", false) => "Docker Desktop is required. Install it, complete its WSL and virtualization setup, start it, and retry Magican setup.".to_string(),
        ("linux", true) => "The Docker CLI is installed, but the Docker daemon is not available to this user. Start Docker and verify `docker info` works without sudo, then retry Magican setup.".to_string(),
        ("linux", false) => "Docker Engine is required. Install it, grant this user Docker access, start the daemon, and retry Magican setup.".to_string(),
        _ => "A running Docker-compatible engine is required before Magican can install its local container.".to_string(),
    }
}

/// Check whether this machine qualifies for native Apple container support.
/// Requires macOS major version >= 26 AND aarch64 (Apple Silicon).
fn is_apple_container_eligible() -> bool {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let major = macos_major_version();
    if os != "macos" {
        return false;
    }
    if arch != "aarch64" {
        warn!(
            "macOS detected but not Apple Silicon (arch={}), skipping Apple Container",
            arch
        );
        return false;
    }
    let eligible = apple_container_eligible_for(os, arch, major);
    match major {
        Some(v) if v >= 26 => {
            info!(
                "macOS {} detected on aarch64 -- Apple Container eligible",
                v
            );
        },
        Some(v) => {
            info!("macOS {} < 26, Apple Container not eligible", v);
        },
        None => {
            warn!("Could not determine macOS version");
        },
    }
    eligible
}

fn apple_container_eligible_for(os: &str, arch: &str, major: Option<u32>) -> bool {
    os == "macos" && arch == "aarch64" && major.is_some_and(|version| version >= 26)
}

/// Parse the macOS major version from `sw_vers -productVersion`.
/// Returns None on non-macOS or parse failure.
fn macos_major_version() -> Option<u32> {
    let output = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let version_str = String::from_utf8_lossy(&output.stdout);
    let major = version_str.trim().split('.').next()?;
    major.parse().ok()
}

/// Expose eligibility check for unit tests (non-pub in normal builds).
#[cfg(test)]
pub(crate) fn is_apple_container_eligible_for_test() -> bool {
    is_apple_container_eligible()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_macos_major_version_parses() {
        // This test only makes sense on macOS, but should not panic anywhere
        if std::env::consts::OS == "macos" {
            let version = macos_major_version();
            assert!(version.is_some(), "Should parse macOS version on macOS");
            assert!(version.unwrap() >= 10, "macOS version should be >= 10");
        }
    }

    #[test]
    fn prerequisite_errors_distinguish_missing_and_stopped_runtimes() {
        assert!(container_runtime_prerequisite_error_for("windows", false)
            .starts_with("Docker Desktop is required"));
        assert!(container_runtime_prerequisite_error_for("windows", true)
            .contains("engine is not running"));
        assert!(container_runtime_prerequisite_error_for("linux", false)
            .starts_with("Docker Engine is required"));
        assert!(container_runtime_prerequisite_error_for("linux", true)
            .contains("daemon is not available"));
    }

    #[test]
    fn apple_container_requires_supported_macos_and_apple_silicon() {
        assert!(apple_container_eligible_for("macos", "aarch64", Some(26)));
        assert!(apple_container_eligible_for("macos", "aarch64", Some(27)));
        assert!(!apple_container_eligible_for("macos", "aarch64", Some(25)));
        assert!(!apple_container_eligible_for("macos", "x86_64", Some(27)));
        assert!(!apple_container_eligible_for("linux", "aarch64", Some(27)));
        assert!(!apple_container_eligible_for("macos", "aarch64", None));
    }
}
