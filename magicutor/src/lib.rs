pub mod bridge_protocol;
pub mod server;
pub mod types;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Main configuration for the Magicutor service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MagicutorConfig {
    pub server: ServerConfig,
    #[serde(default)]
    pub bridge: BridgeConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeConfig {
    pub enabled: bool,
    pub path: String,
    pub auth_token: String,
    /// How many browsers (profiles, Chromium forks) may hold the bridge at
    /// once. Further browsers are refused with 409 `bridge_full`.
    #[serde(default = "default_bridge_max_connections")]
    pub max_connections: usize,
    /// How long a `?choose=ask` CDP connect waits for the owner to pick a
    /// browser before using the default one, unless the URL names a timeout.
    #[serde(default = "default_browser_choice_timeout_secs")]
    pub browser_choice_timeout_secs: u64,
    /// Peers trusted like loopback (addresses or CIDRs, e.g. a container's
    /// gateway for published ports). Added to `MAGICUTOR_TRUSTED_PEERS`.
    /// Empty by default: only this machine is trusted without a token.
    #[serde(default)]
    pub trusted_peers: Vec<String>,
    /// Host names (besides `localhost` and IP literals) a trusted peer may
    /// address Magicutor by. Added to `MAGICUTOR_ALLOWED_HOSTS`. Guards
    /// against DNS rebinding; requests with the token are not affected.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// Where a task's browser tabs open. `auto`: a named tab group in the
    /// owner's own window, falling back to a group in a window of its own,
    /// then a plain window, when groups or background rendering are not
    /// available. `in_window` and `own_window` start the same fallback
    /// further along or pin it. The extension decides per task.
    #[serde(default = "default_automation_surface")]
    pub automation_surface: String,
}

fn default_bridge_max_connections() -> usize {
    server::bridge::MAX_BRIDGE_CONNECTIONS
}

fn default_browser_choice_timeout_secs() -> u64 {
    20
}

fn default_automation_surface() -> String {
    "auto".to_string()
}

fn normalize_data_root(base: impl AsRef<Path>) -> PathBuf {
    let base = base.as_ref();
    if base.file_name().and_then(|value| value.to_str()) == Some("magician_data_v3")
        || base.join("scopes").exists()
    {
        base.to_path_buf()
    } else {
        base.join("magician_data_v3")
    }
}

fn candidate_data_roots() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    candidates.push(PathBuf::from("/app/magician_data_v3"));

    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors().skip(1).take(6) {
            candidates.push(ancestor.join("magician_data_v3"));
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        for ancestor in cwd.ancestors().take(6) {
            candidates.push(ancestor.join("magician_data_v3"));
        }
    }

    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("magician_data_v3"));
    }

    candidates
}

/// Resolve the canonical V3 data root used by Magicutor-side persistence.
///
/// Precedence:
/// 1. `MAGICUTOR_DATA_ROOT`
/// 2. `MAGICIAN_STORAGE_PATH`
/// 3. First existing well-known absolute candidate (`/app`, repo/exe ancestors, home)
/// 4. `~/magician_data_v3`
/// 5. `/tmp/magician_data_v3`
pub fn resolve_data_root() -> PathBuf {
    if let Ok(explicit) = std::env::var("MAGICUTOR_DATA_ROOT") {
        return normalize_data_root(explicit);
    }

    if let Ok(explicit) = std::env::var("MAGICIAN_STORAGE_PATH") {
        return normalize_data_root(explicit);
    }

    if let Ok(explicit) = std::env::var("MAGICUTOR_ACTION_REGISTRY_PATH") {
        let explicit = PathBuf::from(explicit);
        if let Some(root) = explicit.ancestors().nth(3) {
            return normalize_data_root(root);
        }
    }

    if let Some(existing) = candidate_data_roots()
        .into_iter()
        .find(|path| path.exists())
    {
        return existing;
    }

    if let Some(home) = dirs::home_dir() {
        return home.join("magician_data_v3");
    }

    std::env::temp_dir().join("magician_data_v3")
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: "/bridge/native".to_string(),
            auth_token: "magicutor-bridge-dev-token".to_string(),
            max_connections: default_bridge_max_connections(),
            browser_choice_timeout_secs: default_browser_choice_timeout_secs(),
            trusted_peers: Vec::new(),
            allowed_hosts: Vec::new(),
            automation_surface: default_automation_surface(),
        }
    }
}

impl Default for MagicutorConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                port: 3003,
            },
            bridge: BridgeConfig::default(),
        }
    }
}

/// Load configuration from file or use defaults
///
/// Searches for config in the following order:
/// 1. MAGICUTOR_CONFIG_PATH environment variable (if set)
/// 2. Current working directory: ./magicutor/config/magicutor-config.yaml
/// 3. Relative to binary: <binary_dir>/magicutor/config/magicutor-config.yaml
/// 4. Parent of binary: <binary_dir>/../magicutor/config/magicutor-config.yaml
/// 5. Defaults if none found
pub fn load_config() -> Result<MagicutorConfig> {
    // Check if explicit path is provided via environment variable
    if let Ok(config_path) = std::env::var("MAGICUTOR_CONFIG_PATH") {
        if std::path::Path::new(&config_path).exists() {
            tracing::info!("Loading config from MAGICUTOR_CONFIG_PATH: {}", config_path);
            let content = std::fs::read_to_string(&config_path)?;
            let config: MagicutorConfig = serde_yaml::from_str(&content)?;
            return Ok(config);
        } else {
            tracing::warn!(
                "MAGICUTOR_CONFIG_PATH set but file not found: {}",
                config_path
            );
        }
    }

    // Build search paths
    let mut search_paths = vec![];

    // 1. Current working directory
    search_paths.push(std::path::PathBuf::from(
        "magicutor/config/magicutor-config.yaml",
    ));

    // 2-4. Paths relative to binary location
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            // Same directory as binary
            search_paths.push(exe_dir.join("magicutor/config/magicutor-config.yaml"));

            // Parent directory of binary
            if let Some(parent_dir) = exe_dir.parent() {
                search_paths.push(parent_dir.join("magicutor/config/magicutor-config.yaml"));
            }
        }
    }

    // Try each search path
    for path in &search_paths {
        if path.exists() {
            tracing::info!("Loading config from: {}", path.display());
            let content = std::fs::read_to_string(path)?;
            let config: MagicutorConfig = serde_yaml::from_str(&content)?;
            return Ok(config);
        }
    }

    // None found - use defaults
    tracing::warn!("Configuration file not found in any search path, using defaults");
    tracing::warn!("Searched paths:");
    for path in &search_paths {
        tracing::warn!("  - {}", path.display());
    }

    Ok(MagicutorConfig::default())
}
