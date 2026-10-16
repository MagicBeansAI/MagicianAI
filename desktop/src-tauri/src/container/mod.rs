pub mod apple;
pub mod detect;
pub mod docker;
mod keyring;

#[cfg(all(test, target_os = "macos"))]
mod managed_acceptance;
#[cfg(test)]
mod tests;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Progress callback for long-running operations (install, pull, start).
pub type ProgressCallback = Box<dyn Fn(f32, &str) + Send + Sync>;

/// Information about a running or stopped container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerInfo {
    pub name: String,
    pub image: String,
    pub status: ContainerStatus,
    pub ports: Vec<PortMapping>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ContainerStatus {
    Running,
    Stopped,
    Restarting,
    NotFound,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortMapping {
    pub host: u16,
    pub container: u16,
}

/// Configuration for creating and running a container.
#[derive(Debug, Clone)]
pub struct ContainerConfig {
    pub image: String,
    pub name: String,
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    pub log_dir: PathBuf,
    pub ports: Vec<(u16, u16)>,
    pub memory_limit: Option<String>,
    pub cpu_limit: Option<f64>,
    pub env_vars: HashMap<String, String>,
}

pub(crate) const APPLE_CONTAINER_HOST: &str = "host.container.internal";
pub(crate) const DOCKER_CONTAINER_HOST: &str = "host.docker.internal";

pub(crate) fn runtime_env_for_container(
    config: &ContainerConfig,
    container_host: &str,
) -> Result<HashMap<String, String>, String> {
    let mut env = config.env_vars.clone();
    env.insert(
        "MAGICIAN_CONTAINER_HOST".to_string(),
        container_host.to_string(),
    );
    // The desktop owns a private exec relay listening on *guest* loopback.
    // Host aliases cannot reach the identity-bound, loopback-only Mac gateway.
    Ok(env)
}

const PACKAGED_MAGICIAN_CONFIG: &str = include_str!("../../../../magician-config.yaml");
/// The router's `profiles` and `operation_mapping`, which live beside the config
/// rather than inside it. Seeded as its own file because the config is
/// unloadable without it: the loader splices this in and treats a missing
/// sibling as fatal, so a runtime root seeded with the config alone would fail
/// to boot on first run.
const PACKAGED_ROUTER_TABLES: &str = include_str!("../../../../llm-router.yaml");
const PACKAGED_HARNESS_RELIABILITY_PROGRAM: &str = include_str!(
    "../../../../magician_data_v3/scopes/anonymous/default/programs/harness_reliability.md"
);
const PACKAGED_DEFAULT_AGENT_DEFINITIONS: &[(&str, &str)] = &[
    (
        "harness-sre",
        include_str!(
            "../../../../magician_data_v3/scopes/anonymous/default/agent_runtime/agents/harness-sre/definition.agent.yaml"
        ),
    ),
    (
        "cto",
        include_str!(
            "../../../../magician_data_v3/scopes/anonymous/default/agent_runtime/agents/cto/definition.agent.yaml"
        ),
    ),
    (
        "internal-system-analyst",
        include_str!(
            "../../../../magician_data_v3/scopes/anonymous/default/agent_runtime/agents/internal-system-analyst/definition.agent.yaml"
        ),
    ),
];

impl ContainerConfig {
    /// Build a ContainerConfig from the desktop app config.
    pub fn from_desktop_config(cfg: &crate::config::MagicianDesktopConfig) -> Self {
        let data_dir = crate::runtime_paths::runtime_root_dir();
        Self {
            image: cfg.general.container_image.clone(),
            name: cfg.general.container_name.clone(),
            data_dir: data_dir.clone(),
            config_dir: data_dir.join("config"),
            log_dir: data_dir.join("logs"),
            ports: vec![
                (cfg.network.magician_port, 3002),
                (cfg.network.magicutor_port, 3003),
            ],
            memory_limit: Some(format!("{}g", cfg.container.memory_gb)),
            cpu_limit: Some(cfg.container.cpu_cores),
            env_vars: {
                let mut env = HashMap::new();
                env.insert("MAGICIAN_ROOT_DIR".to_string(), "/data".to_string());
                if !cfg.api_keys.openai_api_key.is_empty() {
                    env.insert(
                        "OPENAI_API_KEY".to_string(),
                        cfg.api_keys.openai_api_key.clone(),
                    );
                }
                if !cfg.api_keys.anthropic_api_key.is_empty() {
                    env.insert(
                        "ANTHROPIC_API_KEY".to_string(),
                        cfg.api_keys.anthropic_api_key.clone(),
                    );
                }
                if cfg.host_gateway.enabled {
                    env.insert(
                        "MAGICIAN_HOST_GATEWAY_URL".to_string(),
                        "http://127.0.0.1:3017".to_string(),
                    );
                }
                env
            },
        }
    }
}

/// Seed the host runtime root with packaged backend defaults when missing.
/// This mirrors the script/container entrypoints, but runs from the host side so
/// desktop-managed bind mounts do not depend on container write permissions.
pub(crate) fn seed_runtime_config_if_missing(runtime_root: &Path) -> Result<(), String> {
    seed_runtime_file_if_missing(
        runtime_root.join("magician-config.yaml"),
        PACKAGED_MAGICIAN_CONFIG,
        "runtime config",
    )?;
    // Seeded unconditionally alongside the config, not only when the config was
    // written: an existing runtime root from before the tables were split out
    // has a config but no sibling, and would stop booting on upgrade.
    seed_runtime_file_if_missing(
        runtime_root.join("llm-router.yaml"),
        PACKAGED_ROUTER_TABLES,
        "runtime LLM router tables",
    )?;

    let default_scope_root = runtime_root
        .join("scopes")
        .join("anonymous")
        .join("default");
    seed_runtime_file_if_missing(
        default_scope_root
            .join("programs")
            .join("harness_reliability.md"),
        PACKAGED_HARNESS_RELIABILITY_PROGRAM,
        "default harness reliability program",
    )?;

    for &(agent_id, definition) in PACKAGED_DEFAULT_AGENT_DEFINITIONS {
        seed_runtime_file_if_missing(
            default_scope_root
                .join("agent_runtime")
                .join("agents")
                .join(agent_id)
                .join("definition.agent.yaml"),
            definition,
            "default agent definition",
        )?;
    }

    Ok(())
}

fn seed_runtime_file_if_missing(dest: PathBuf, contents: &str, label: &str) -> Result<(), String> {
    if dest.exists() {
        return Ok(());
    }

    let parent = dest.parent().ok_or_else(|| {
        format!(
            "Failed to seed {label} {}: destination has no parent",
            dest.display()
        )
    })?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;

    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&dest)
    {
        Ok(mut file) => file
            .write_all(contents.as_bytes())
            .map_err(|e| format!("Failed to seed {label} {}: {}", dest.display(), e)),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(format!(
            "Failed to create {label} {}: {}",
            dest.display(),
            e
        )),
    }
}

/// Check if Homebrew is installed on this system.
pub async fn is_homebrew_available() -> bool {
    let brew = find_brew_path();
    tokio::process::Command::new(&brew)
        .arg("--version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Find the brew binary path. After a fresh install, it may not be in PATH yet.
pub(crate) fn find_brew_path() -> String {
    // Apple Silicon
    if std::path::Path::new("/opt/homebrew/bin/brew").exists() {
        return "/opt/homebrew/bin/brew".to_string();
    }
    // Intel Mac
    if std::path::Path::new("/usr/local/bin/brew").exists() {
        return "/usr/local/bin/brew".to_string();
    }
    // Fall back to PATH
    "brew".to_string()
}

/// Install Homebrew on macOS using the native admin dialog for privilege escalation.
/// Shows the macOS system password prompt via osascript.
pub async fn install_homebrew(progress: Option<&ProgressCallback>) -> Result<(), String> {
    if std::env::consts::OS != "macos" {
        return Err("Homebrew bootstrap is only available on macOS".to_string());
    }
    if let Some(cb) = progress {
        cb(
            0.0,
            "Installing Homebrew (you may be prompted for your password)...",
        );
    }

    // Homebrew rejects a normal macOS install when the installer itself runs as
    // root. Run it as the signed-in user and let sudo use a bounded native
    // askpass helper for the few privileged filesystem operations it needs.
    let temporary = tempfile::tempdir()
        .map_err(|error| format!("Could not prepare the Homebrew installer: {error}"))?;
    let installer = temporary.path().join("install-homebrew.sh");
    let askpass = temporary.path().join("magican-sudo-askpass.sh");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|error| format!("Could not prepare the Homebrew download: {error}"))?;
    let response = client
        .get("https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh")
        .send()
        .await
        .map_err(|error| format!("Could not download the Homebrew installer: {error}"))?
        .error_for_status()
        .map_err(|error| format!("Could not download the Homebrew installer: {error}"))?;
    let contents = response
        .bytes()
        .await
        .map_err(|error| format!("Could not read the Homebrew installer: {error}"))?;
    if contents.is_empty() || contents.len() > 1_048_576 {
        return Err("The downloaded Homebrew installer had an unexpected size".to_string());
    }
    std::fs::write(&installer, &contents)
        .map_err(|error| format!("Could not stage the Homebrew installer: {error}"))?;
    std::fs::write(
        &askpass,
        r##"#!/bin/sh
exec /usr/bin/osascript <<'APPLESCRIPT'
text returned of (display dialog "Magican needs your macOS administrator password to install Homebrew." default answer "" with hidden answer buttons {"Cancel", "OK"} default button "OK" cancel button "Cancel" with icon caution)
APPLESCRIPT
"##,
    )
    .map_err(|error| format!("Could not stage the macOS authorization helper: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&installer, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("Could not protect the Homebrew installer: {error}"))?;
        std::fs::set_permissions(&askpass, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("Could not protect the authorization helper: {error}"))?;
    }

    let output = tokio::process::Command::new("/bin/bash")
        .arg(&installer)
        .env("NONINTERACTIVE", "1")
        .env("SUDO_ASKPASS", &askpass)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("Failed to launch Homebrew installer: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Check if user cancelled the password dialog
        if stderr.contains("User canceled")
            || stderr.contains("-128")
            || stderr.contains("no password was provided")
        {
            return Err("Installation cancelled. You can install Homebrew manually from https://brew.sh and restart Magican.".to_string());
        }
        return Err(format!(
            "Homebrew installation failed: {}. You can install manually from https://brew.sh",
            stderr.trim()
        ));
    }

    if let Some(cb) = progress {
        cb(1.0, "Homebrew installed successfully");
    }

    if is_homebrew_available().await {
        Ok(())
    } else {
        Err("Homebrew installation finished, but `brew` could not be verified in /opt/homebrew or /usr/local. Restart Magican after completing the Homebrew installer.".to_string())
    }
}

/// Run a shell command with macOS admin privileges via the native password dialog.
/// Returns the command's stdout on success, or a user-friendly error.
pub async fn run_with_admin(command: &str, description: &str) -> Result<String, String> {
    if std::env::consts::OS != "macos" {
        return Err("Admin privilege escalation via osascript is macOS-only".to_string());
    }

    // Escape the command and description for AppleScript
    let escaped = command.replace('\\', "\\\\").replace('"', "\\\"");
    let escaped_desc = description.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "do shell script \"{}\" with administrator privileges with prompt \"Magican needs to {}\"",
        escaped, escaped_desc
    );

    let output = tokio::process::Command::new("osascript")
        .args(["-e", &script])
        .output()
        .await
        .map_err(|e| format!("Failed to request admin privileges: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("User canceled") || stderr.contains("-128") {
            return Err(format!(
                "Cancelled by user. You can {} manually.",
                description
            ));
        }
        return Err(format!("Failed to {}: {}", description, stderr.trim()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Run a shell command with admin privileges on Linux via pkexec.
pub async fn run_with_admin_linux(command: &str, description: &str) -> Result<String, String> {
    let output = tokio::process::Command::new("pkexec")
        .args(["bash", "-c", command])
        .output()
        .await
        .map_err(|e| format!("Failed to request admin privileges: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Failed to {}: {}", description, stderr.trim()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Trait defining the container runtime interface.
/// Implemented by AppleContainerRuntime and DockerRuntime.
#[async_trait]
pub trait ContainerRuntime: Send + Sync {
    /// Private interactive exec channel to the selected managed container.
    /// Never allocate a TTY: its line discipline would corrupt the framing.
    fn host_relay_command(&self, _name: &str) -> Result<tokio::process::Command, String> {
        Err("This runtime does not support the private host relay".into())
    }

    /// Human-readable name of this runtime (e.g. "Apple Container", "Docker").
    fn name(&self) -> &str;

    /// Check if the runtime CLI is installed and available.
    async fn is_available(&self) -> bool;

    /// Install the runtime if not present. Returns Ok(()) on success.
    async fn install(&self, progress: Option<&ProgressCallback>) -> Result<(), String>;

    /// Pull/update a container image.
    async fn pull_image(
        &self,
        image: &str,
        progress: Option<&ProgressCallback>,
    ) -> Result<(), String>;

    /// Check if the named image exists locally.
    async fn image_exists(&self, image: &str) -> Result<bool, String>;

    /// Start a container with the given configuration.
    async fn start(&self, config: &ContainerConfig) -> Result<(), String>;

    /// Validate persistent credential custody and the image before any existing
    /// container is stopped or removed. Start also rechecks before creation.
    async fn prepare_keyring(&self, _config: &ContainerConfig) -> Result<Vec<String>, String> {
        Err("This runtime does not support managed keyring provisioning".into())
    }

    /// Start an existing container without replacing its writable layer or config.
    async fn start_existing(&self, name: &str) -> Result<(), String>;

    /// A service restart is not an image/configuration replacement.
    async fn restart_existing(&self, name: &str) -> Result<(), String> {
        match self.container_info(name).await?.status {
            ContainerStatus::Running => self.stop(name).await?,
            ContainerStatus::Stopped => {},
            status => {
                return Err(format!(
                    "Cannot restart existing container '{name}': {status:?}"
                ))
            },
        }
        self.start_existing(name).await
    }

    /// Stop a running container.
    async fn stop(&self, name: &str) -> Result<(), String>;

    /// Remove a container (must be stopped first).
    async fn remove(&self, name: &str) -> Result<(), String>;

    /// Get info about a container by name.
    async fn container_info(&self, name: &str) -> Result<ContainerInfo, String>;

    /// Get recent logs from a container.
    async fn logs(&self, name: &str, lines: usize) -> Result<String, String>;

    /// Execute a command inside a running container and return stdout.
    async fn exec_in_container(&self, name: &str, command: &[&str]) -> Result<String, String>;

    /// Get the local image digest (for update detection).
    async fn image_digest(&self, image: &str) -> Result<String, String>;

    /// Get the remote image digest (for update detection).
    async fn remote_image_digest(&self, image: &str) -> Result<String, String>;

    /// Tag a local image with a new tag (used for rollback support).
    async fn tag_image(&self, image: &str, new_tag: &str) -> Result<(), String>;
}

/// Shared by the tray and focused routing tests; errors reported in the
/// supervisor's response must not be mistaken for a successful CLI invocation.
pub(crate) async fn run_supervisor_command(
    runtime: &dyn ContainerRuntime,
    name: &str,
    command: &str,
) -> Result<(), String> {
    if !matches!(
        command,
        "restart-magician" | "stop-magician" | "restart-magicutor" | "stop-magicutor" | "status"
    ) {
        return Err(format!("Unsupported container service command: {command}"));
    }
    if runtime.container_info(name).await?.status != ContainerStatus::Running {
        return Err(format!("Container '{name}' is not running"));
    }
    // Older images give the CLI only five seconds to receive a response, but
    // the server allows ten seconds for a graceful stop. Capture the process
    // identity before a restart so a timed-out reply can be reconciled without
    // sending the mutation again (or reporting an unchanged process as success).
    let before = if command.starts_with("restart-") {
        Some(supervisor_status(runtime, name).await?)
    } else {
        None
    };
    match runtime
        .exec_in_container(name, &["/app/magic-supervisor", "client", command])
        .await
    {
        Ok(output) => parse_supervisor_response(&output).map(|_| ()),
        Err(error)
            if command != "status"
                && error.contains("Timed out waiting for supervisor response") =>
        {
            let reconciliation = async {
                loop {
                    if let Ok(after) = supervisor_status(runtime, name).await {
                        if supervisor_command_completed(command, before.as_ref(), &after) {
                            tracing::info!(
                                container = name,
                                command,
                                "Confirmed container service state after supervisor client timeout"
                            );
                            return;
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            };
            tokio::time::timeout(std::time::Duration::from_secs(30), reconciliation)
                .await
                .map_err(|_| format!("{error}; unable to confirm service state within 30 seconds"))
        },
        Err(error) => Err(error),
    }
}

async fn supervisor_status(
    runtime: &dyn ContainerRuntime,
    name: &str,
) -> Result<serde_json::Value, String> {
    let output = runtime
        .exec_in_container(name, &["/app/magic-supervisor", "client", "status"])
        .await?;
    parse_supervisor_response(&output)
}

fn supervisor_command_completed(
    command: &str,
    before: Option<&serde_json::Value>,
    after: &serde_json::Value,
) -> bool {
    let Some((action, service)) = command.split_once('-') else {
        return false;
    };
    let current = &after["data"][service];
    match action {
        "stop" => current["status"] == "stopped" && current["pid"].as_u64().unwrap_or(0) == 0,
        "restart" => {
            let old_pid = before.and_then(|v| v["data"][service]["pid"].as_u64());
            let new_pid = current["pid"].as_u64();
            current["status"] == "running"
                && new_pid.is_some_and(|pid| pid > 0)
                && new_pid != old_pid
        },
        _ => false,
    }
}

fn parse_supervisor_response(output: &str) -> Result<serde_json::Value, String> {
    let response: serde_json::Value = serde_json::from_str(
        output
            .trim()
            .strip_prefix("Response:")
            .unwrap_or(output.trim())
            .trim(),
    )
    .map_err(|error| format!("Invalid supervisor response: {error}"))?;
    if response.get("success").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(response
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Container supervisor command failed")
            .to_string());
    }
    Ok(response)
}
