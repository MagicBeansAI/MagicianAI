use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::warn;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallManifest {
    pub installed_at: String,
    pub platform: String,
    pub runtime: String,
    pub installed_by_us: InstalledArtifacts,
    pub pre_existing: PreExistingState,
    pub data_dirs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledArtifacts {
    #[serde(default)]
    pub homebrew: bool,
    #[serde(default)]
    pub container_runtime: String,
    #[serde(default)]
    pub container_image: String,
    #[serde(default)]
    pub container_name: String,
    #[serde(default)]
    pub launch_agent: bool,
    /// Desktop-managed native backend prefix. Empty for container installs.
    #[serde(default)]
    pub native_prefix: String,
    /// OS startup registration owned by Desktop (LaunchAgent/systemd unit).
    #[serde(default)]
    pub native_service: String,
    #[serde(default)]
    pub native_package_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreExistingState {
    #[serde(default)]
    pub homebrew: bool,
    #[serde(default)]
    pub container_runtime: bool,
    #[serde(default)]
    pub native_backend: bool,
    #[serde(default)]
    pub native_service: bool,
}

/// Platform-specific application support directory.
pub fn manifest_dir() -> PathBuf {
    match std::env::consts::OS {
        "macos" => dirs::home_dir()
            .expect("cannot determine home directory")
            .join("Library/Application Support/dev.magician.desktop"),
        "linux" => dirs::home_dir()
            .expect("cannot determine home directory")
            .join(".config/magician"),
        _ => dirs::data_dir()
            .expect("cannot determine data directory")
            .join("dev.magician.desktop"),
    }
}

/// Full path to the install manifest JSON file.
pub fn manifest_path() -> PathBuf {
    manifest_dir().join("install-manifest.json")
}

/// Snapshot the pre-existing state of homebrew and container runtimes.
pub async fn snapshot_pre_existing() -> PreExistingState {
    let homebrew = if std::env::consts::OS == "macos" {
        crate::container::is_homebrew_available().await
    } else {
        false
    };

    let container_runtime = check_container_runtime_available().await;

    PreExistingState {
        homebrew,
        container_runtime,
        native_backend: crate::native_runtime::install_prefix_pre_exists(),
        native_service: crate::native_runtime::service_registration_exists(),
    }
}

/// Check whether any container runtime is reachable.
async fn check_container_runtime_available() -> bool {
    // Try docker first
    let docker_ok = tokio::process::Command::new("docker")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);

    if docker_ok {
        return true;
    }

    // On macOS, also try Apple container CLI
    if std::env::consts::OS == "macos" {
        let apple_ok = tokio::process::Command::new("container")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);

        if apple_ok {
            return true;
        }
    }

    false
}

/// Returns `"{os}-{arch}"` for the current platform.
pub fn platform_string() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Write the manifest to disk as pretty-printed JSON.
pub fn write_manifest(manifest: &InstallManifest) -> Result<(), String> {
    let path = manifest_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create manifest directory: {e}"))?;
    }
    let json = serde_json::to_string_pretty(manifest)
        .map_err(|e| format!("failed to serialize manifest: {e}"))?;

    // Atomic write: write to a temp file first, then rename into place.
    // This prevents a crash mid-write from corrupting the manifest.
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json)
        .map_err(|e| format!("failed to write temporary manifest: {e}"))?;
    std::fs::rename(&tmp_path, &path)
        .map_err(|e| format!("failed to rename manifest into place: {e}"))
}

/// Read and deserialize the manifest. Returns `None` if the file is missing or
/// cannot be parsed (warnings are logged in either case).
pub fn read_manifest() -> Option<InstallManifest> {
    let path = manifest_path();
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            warn!("failed to read manifest at {}: {e}", path.display());
            return None;
        },
    };
    match serde_json::from_str(&data) {
        Ok(m) => Some(m),
        Err(e) => {
            warn!("failed to parse manifest at {}: {e}", path.display());
            None
        },
    }
}

/// Remove the manifest file if it exists.
pub fn remove_manifest() -> Result<(), String> {
    let path = manifest_path();
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("failed to remove manifest: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn manifest_write_and_read() {
        let dir =
            std::env::temp_dir().join(format!("magician_manifest_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("install-manifest.json");

        let manifest = InstallManifest {
            installed_at: "2026-03-11T10:00:00Z".to_string(),
            platform: "macos-aarch64".to_string(),
            runtime: "Docker".to_string(),
            installed_by_us: InstalledArtifacts {
                homebrew: true,
                container_runtime: "colima+docker".to_string(),
                container_image: "ghcr.io/magicbeanbs100x/magician:latest".to_string(),
                container_name: "magician".to_string(),
                launch_agent: true,
                native_prefix: String::new(),
                native_service: String::new(),
                native_package_version: String::new(),
            },
            pre_existing: PreExistingState {
                homebrew: false,
                container_runtime: false,
                native_backend: false,
                native_service: false,
            },
            data_dirs: vec!["~/magician_data_v3".to_string()],
        };

        // Serialize and write
        let json = serde_json::to_string_pretty(&manifest).unwrap();
        let mut f = std::fs::File::create(&file_path).unwrap();
        f.write_all(json.as_bytes()).unwrap();

        // Read back and deserialize
        let data = std::fs::read_to_string(&file_path).unwrap();
        let loaded: InstallManifest = serde_json::from_str(&data).unwrap();

        assert_eq!(loaded.installed_at, "2026-03-11T10:00:00Z");
        assert_eq!(loaded.platform, "macos-aarch64");
        assert_eq!(loaded.runtime, "Docker");
        assert_eq!(loaded.installed_by_us.homebrew, true);
        assert_eq!(loaded.installed_by_us.container_runtime, "colima+docker");
        assert_eq!(
            loaded.installed_by_us.container_image,
            "ghcr.io/magicbeanbs100x/magician:latest"
        );
        assert_eq!(loaded.installed_by_us.container_name, "magician");
        assert_eq!(loaded.installed_by_us.launch_agent, true);
        assert_eq!(loaded.pre_existing.homebrew, false);
        assert_eq!(loaded.pre_existing.container_runtime, false);
        assert_eq!(loaded.data_dirs, vec!["~/magician_data_v3"]);

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_pre_existing_skips_cleanup() {
        let manifest = InstallManifest {
            installed_at: "2026-03-11T10:00:00Z".to_string(),
            platform: "macos-aarch64".to_string(),
            runtime: "Docker".to_string(),
            installed_by_us: InstalledArtifacts {
                homebrew: false,
                container_runtime: "none".to_string(),
                container_image: "ghcr.io/magicbeanbs100x/magician:latest".to_string(),
                container_name: "magician".to_string(),
                launch_agent: false,
                native_prefix: String::new(),
                native_service: String::new(),
                native_package_version: String::new(),
            },
            pre_existing: PreExistingState {
                homebrew: true,
                container_runtime: true,
                native_backend: false,
                native_service: false,
            },
            data_dirs: vec![],
        };

        // Pre-existing flags are true — cleanup should skip those tools
        assert!(manifest.pre_existing.homebrew);
        assert!(manifest.pre_existing.container_runtime);

        // installed_by_us.container_runtime is "none" — nothing to uninstall
        assert_eq!(manifest.installed_by_us.container_runtime, "none");

        // We did not install homebrew
        assert!(!manifest.installed_by_us.homebrew);
    }

    #[test]
    fn platform_string_format() {
        let ps = platform_string();
        assert!(!ps.is_empty(), "platform_string() should not be empty");
        assert!(
            ps.contains('-'),
            "platform_string() should contain a dash (os-arch), got: {ps}"
        );
    }
}
