use async_trait::async_trait;
use std::ffi::OsStr;
use std::path::PathBuf;
use tokio::process::Command;
use tracing::{debug, info};

use super::{
    runtime_env_for_container, seed_runtime_config_if_missing, ContainerConfig, ContainerInfo,
    ContainerRuntime, ContainerStatus, PortMapping, ProgressCallback, DOCKER_CONTAINER_HOST,
};

/// Docker CLI runtime adapter.
///
/// Works with Docker Desktop, Colima, or any Docker-compatible daemon.
pub struct DockerRuntime;

impl DockerRuntime {
    pub fn new() -> Self {
        Self
    }

    pub(crate) fn cli_is_installed() -> bool {
        docker_program().is_file()
    }

    /// Execute a `docker` CLI command and return stdout.
    async fn exec(&self, args: &[&str]) -> Result<String, String> {
        debug!("docker {}", args.join(" "));
        let output = Command::new(docker_program())
            .args(args)
            .output()
            .await
            .map_err(|e| format!("Failed to execute `docker {}`: {}", args.join(" "), e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "`docker {}` failed (exit {}): {}",
                args.join(" "),
                output.status.code().unwrap_or(-1),
                stderr.trim()
            ));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

#[async_trait]
impl ContainerRuntime for DockerRuntime {
    fn host_relay_command(&self, name: &str) -> Result<Command, String> {
        let mut command = Command::new(docker_program());
        command.args(["exec", "--interactive", name]);
        Ok(command)
    }

    async fn start_existing(&self, name: &str) -> Result<(), String> {
        self.exec(&["start", name]).await?;
        info!("Started existing container '{}'", name);
        Ok(())
    }

    fn name(&self) -> &str {
        "Docker"
    }

    async fn is_available(&self) -> bool {
        let mut command = Command::new(docker_program());
        command.arg("info").kill_on_drop(true);
        let available = matches!(
            tokio::time::timeout(std::time::Duration::from_secs(5), command.output()).await,
            Ok(Ok(output)) if output.status.success()
        );
        if available {
            info!("Docker daemon is available");
        }
        available
    }

    async fn install(&self, progress: Option<&ProgressCallback>) -> Result<(), String> {
        if let Some(cb) = progress {
            cb(0.1, "Installing Docker runtime...");
        }

        #[cfg(target_os = "macos")]
        {
            // macOS: install Colima + Docker CLI via Homebrew
            // Ensure Homebrew is available first
            if !super::is_homebrew_available().await {
                if let Some(cb) = progress {
                    cb(
                        0.15,
                        "Installing Homebrew (you may be prompted for your password)...",
                    );
                }
                super::install_homebrew(progress).await?;
            }

            if let Some(cb) = progress {
                cb(0.3, "Installing Colima and Docker CLI via Homebrew...");
            }

            let brew_path = super::find_brew_path();

            let brew_output = Command::new(&brew_path)
                .args(["install", "colima", "docker"])
                .output()
                .await
                .map_err(|e| format!("Failed to run brew: {}", e))?;

            if !brew_output.status.success() {
                let stderr = String::from_utf8_lossy(&brew_output.stderr);
                return Err(format!(
                    "Failed to install Colima/Docker via Homebrew: {}",
                    stderr
                ));
            }

            if let Some(cb) = progress {
                cb(0.7, "Starting Colima VM...");
            }

            // A Finder/LaunchServices process does not inherit the user's
            // interactive shell PATH. Resolve the freshly installed binary
            // beside Homebrew instead of relying on `colima` being discoverable.
            let colima_output = Command::new(colima_program())
                .args(["start", "--cpu", "2", "--memory", "4"])
                .output()
                .await
                .map_err(|e| format!("Failed to start Colima: {}", e))?;

            if !colima_output.status.success() {
                let stderr = String::from_utf8_lossy(&colima_output.stderr);
                return Err(format!("Failed to start Colima: {}", stderr));
            }

            info!("Installed and started Colima with Docker");

            if let Some(cb) = progress {
                cb(1.0, "Docker runtime installed");
            }
            Ok(())
        }

        #[cfg(not(target_os = "macos"))]
        {
            Err(if std::env::consts::OS == "windows" {
                "Install and start Docker Desktop, finish its WSL and virtualization setup, then retry Magican setup."
                    .to_string()
            } else if std::env::consts::OS == "linux" {
                "Install Docker Engine, grant this user Docker access, start the daemon, then retry Magican setup."
                    .to_string()
            } else {
                format!(
                    "Automatic Docker installation is unavailable on {}",
                    std::env::consts::OS
                )
            })
        }
    }

    async fn pull_image(
        &self,
        image: &str,
        progress: Option<&ProgressCallback>,
    ) -> Result<(), String> {
        if let Some(cb) = progress {
            cb(0.0, &format!("Pulling image {}...", image));
        }

        self.exec(&["pull", image]).await?;

        if let Some(cb) = progress {
            cb(1.0, "Image pull complete");
        }
        info!("Pulled image: {}", image);
        Ok(())
    }

    async fn image_exists(&self, image: &str) -> Result<bool, String> {
        match Command::new(docker_program())
            .args(["image", "inspect", image])
            .output()
            .await
        {
            Ok(output) => Ok(output.status.success()),
            Err(e) => Err(format!("Failed to check image: {}", e)),
        }
    }

    async fn prepare_keyring(&self, config: &ContainerConfig) -> Result<Vec<String>, String> {
        let launch_args = super::keyring::prepare(config, "docker", "docker").await?;
        #[cfg(target_os = "windows")]
        {
            self.validate_windows_managed_container(config, &launch_args)
                .await?;
            self.verify_windows_keyring_image(config, &launch_args)
                .await?;
        }
        Ok(launch_args)
    }

    async fn start(&self, config: &ContainerConfig) -> Result<(), String> {
        std::fs::create_dir_all(&config.data_dir).map_err(|e| {
            format!(
                "Failed to create runtime root {}: {}",
                config.data_dir.display(),
                e
            )
        })?;
        seed_runtime_config_if_missing(&config.data_dir)?;
        let keyring_args = self.prepare_keyring(config).await?;

        let mut args = vec![
            "run".to_string(),
            "-d".to_string(),
            "--name".to_string(),
            config.name.clone(),
            "--restart".to_string(),
            "unless-stopped".to_string(),
        ];

        let linux_host_network = std::env::consts::OS == "linux";
        if linux_host_network {
            args.push("--network".to_string());
            args.push("host".to_string());
        }

        // Canonical runtime root and private persistent credential mounts.
        args.extend(keyring_args);

        // Port mappings
        if !linux_host_network {
            for (host, container) in &config.ports {
                args.push("-p".to_string());
                args.push(format!("{}:{}", host, container));
            }
        }

        // CPU limit
        if let Some(cpus) = config.cpu_limit {
            args.push("--cpus".to_string());
            args.push(format!("{}", cpus));
        }

        // Memory limit
        if let Some(ref mem) = config.memory_limit {
            args.push("--memory".to_string());
            args.push(mem.clone());
        }

        // Environment variables
        let mut env_vars = if linux_host_network {
            config.env_vars.clone()
        } else {
            runtime_env_for_container(config, DOCKER_CONTAINER_HOST)?
        };
        if linux_host_network {
            for (host, container) in &config.ports {
                match *container {
                    3002 => {
                        env_vars.insert("MAGICIAN_PORT".to_string(), host.to_string());
                    },
                    3003 => {
                        env_vars.insert("MAGICUTOR_PORT".to_string(), host.to_string());
                    },
                    _ => {},
                }
            }
        }
        for (key, value) in &env_vars {
            args.push("-e".to_string());
            args.push(format!("{}={}", key, value));
        }

        // Image
        args.push(config.image.clone());

        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.exec(&args_refs).await?;

        info!(
            "Started container '{}' from image '{}'",
            config.name, config.image
        );
        Ok(())
    }

    async fn stop(&self, name: &str) -> Result<(), String> {
        self.exec(&["stop", name]).await?;
        info!("Stopped container '{}'", name);
        Ok(())
    }

    async fn remove(&self, name: &str) -> Result<(), String> {
        self.exec(&["rm", name]).await?;
        info!("Removed container '{}'", name);
        Ok(())
    }

    async fn container_info(&self, name: &str) -> Result<ContainerInfo, String> {
        let output = Command::new(docker_program())
            .args([
                "inspect",
                "--format",
                "{{.State.Status}}|{{.Config.Image}}|{{.Created}}",
                name,
            ])
            .output()
            .await
            .map_err(|e| format!("Failed to inspect container: {}", e))?;

        if !output.status.success() {
            return Ok(ContainerInfo {
                name: name.to_string(),
                image: String::new(),
                status: ContainerStatus::NotFound,
                ports: vec![],
                created_at: None,
            });
        }

        let raw = String::from_utf8_lossy(&output.stdout);
        let parts: Vec<&str> = raw.trim().splitn(3, '|').collect();

        let status = match parts.first().map(|s| s.trim()) {
            Some("running") => ContainerStatus::Running,
            Some("restarting") => ContainerStatus::Restarting,
            Some("exited") | Some("dead") => ContainerStatus::Stopped,
            _ => ContainerStatus::Stopped,
        };

        let image = parts.get(1).unwrap_or(&"").trim().to_string();
        let created_at = parts.get(2).map(|s| s.trim().to_string());

        let ports = self.parse_ports(name).await.unwrap_or_default();

        Ok(ContainerInfo {
            name: name.to_string(),
            image,
            status,
            ports,
            created_at,
        })
    }

    async fn logs(&self, name: &str, lines: usize) -> Result<String, String> {
        self.exec(&["logs", "--tail", &lines.to_string(), name])
            .await
    }

    async fn exec_in_container(&self, name: &str, command: &[&str]) -> Result<String, String> {
        let mut args = Vec::with_capacity(command.len() + 2);
        args.push("exec");
        args.push(name);
        args.extend_from_slice(command);
        self.exec(&args).await
    }

    async fn image_digest(&self, image: &str) -> Result<String, String> {
        let output = self
            .exec(&[
                "image",
                "inspect",
                "--format",
                "{{index .RepoDigests 0}}",
                image,
            ])
            .await?;
        Ok(output.trim().to_string())
    }

    async fn tag_image(&self, image: &str, new_tag: &str) -> Result<(), String> {
        self.exec(&["tag", image, new_tag]).await?;
        info!("Tagged image '{}' as '{}'", image, new_tag);
        Ok(())
    }

    async fn remote_image_digest(&self, image: &str) -> Result<String, String> {
        // Use `docker manifest inspect` to check remote digest without pulling
        let output = Command::new(docker_program())
            .args(["manifest", "inspect", image])
            .output()
            .await
            .map_err(|e| format!("Failed to inspect remote manifest: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "Failed to inspect remote manifest for '{}': {}",
                image,
                stderr.trim()
            ));
        }

        let manifest = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(&manifest)
            .map_err(|e| format!("Failed to parse manifest JSON for '{}': {}", image, e))?;

        // Try top-level "digest" first, then first entry in "manifests" array
        if let Some(digest) = json.get("digest").and_then(|v| v.as_str()) {
            return Ok(digest.to_string());
        }
        if let Some(digest) = json
            .get("manifests")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|m| m.get("digest"))
            .and_then(|v| v.as_str())
        {
            return Ok(digest.to_string());
        }

        Err(format!("No digest found in manifest JSON for '{}'", image))
    }
}

fn docker_program() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            let candidate = PathBuf::from(local_app_data)
                .join("Programs")
                .join("DockerDesktop")
                .join("resources")
                .join("bin")
                .join("docker.exe");
            if candidate.is_file() {
                return candidate;
            }
        }
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            let candidate = PathBuf::from(program_files)
                .join("Docker")
                .join("Docker")
                .join("resources")
                .join("bin")
                .join("docker.exe");
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        for candidate in [
            "/opt/homebrew/bin/docker",
            "/usr/local/bin/docker",
            "/Applications/Docker.app/Contents/Resources/bin/docker",
        ] {
            let candidate = PathBuf::from(candidate);
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    runtime_core::process::resolve_program(
        OsStr::new("docker"),
        std::env::var_os("PATH").as_deref(),
    )
}

#[cfg(target_os = "macos")]
fn colima_program() -> PathBuf {
    for candidate in ["/opt/homebrew/bin/colima", "/usr/local/bin/colima"] {
        let candidate = PathBuf::from(candidate);
        if candidate.is_file() {
            return candidate;
        }
    }
    runtime_core::process::resolve_program(
        OsStr::new("colima"),
        std::env::var_os("PATH").as_deref(),
    )
}

impl DockerRuntime {
    async fn parse_ports(&self, name: &str) -> Result<Vec<PortMapping>, String> {
        let output = Command::new(docker_program())
            .args(["port", name])
            .output()
            .await
            .map_err(|e| format!("Failed to get ports: {}", e))?;

        if !output.status.success() {
            return Ok(vec![]);
        }

        let raw = String::from_utf8_lossy(&output.stdout);
        let mut ports = Vec::new();

        for line in raw.lines() {
            // Format: "3002/tcp -> 0.0.0.0:3002"
            let parts: Vec<&str> = line.split("->").collect();
            if parts.len() == 2 {
                let container_port = parts[0]
                    .trim()
                    .split('/')
                    .next()
                    .and_then(|s| s.parse::<u16>().ok());
                let host_port = parts[1]
                    .trim()
                    .rsplit(':')
                    .next()
                    .and_then(|s| s.parse::<u16>().ok());
                if let (Some(cp), Some(hp)) = (container_port, host_port) {
                    ports.push(PortMapping {
                        host: hp,
                        container: cp,
                    });
                }
            }
        }

        Ok(ports)
    }

    #[cfg(target_os = "windows")]
    async fn validate_windows_managed_container(
        &self,
        config: &ContainerConfig,
        launch_args: &[String],
    ) -> Result<(), String> {
        use std::collections::BTreeMap;

        #[derive(serde::Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Mount {
            #[serde(rename = "Type")]
            kind: String,
            source: String,
            destination: String,
            #[serde(rename = "RW")]
            writable: bool,
        }

        #[derive(serde::Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct InspectConfig {
            env: Option<Vec<String>>,
        }

        #[derive(serde::Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Inspect {
            config: InspectConfig,
            mounts: Vec<Mount>,
        }

        fn normalized_source(value: &str) -> String {
            let value = value.replace('\\', "/");
            let value = if let Some(rest) = value.strip_prefix("//?/UNC/") {
                format!("//{rest}")
            } else {
                value.strip_prefix("//?/").unwrap_or(&value).to_string()
            };
            value.trim_end_matches('/').to_ascii_lowercase()
        }

        let names = self.exec(&["ps", "-a", "--format", "{{.Names}}"]).await?;
        if !names.lines().any(|name| name.trim() == config.name) {
            return Ok(());
        }

        let raw = self.exec(&["inspect", &config.name]).await.map_err(|_| {
            "Cannot inspect the existing container; it was left unchanged".to_string()
        })?;
        let mut inspected: Vec<Inspect> = serde_json::from_str(&raw).map_err(|_| {
            "Unrecognized existing container inspection; it was left unchanged".to_string()
        })?;
        if inspected.len() != 1 {
            return Err(
                "Existing container identity is ambiguous; it was left unchanged".to_string(),
            );
        }
        let inspected = inspected.remove(0);
        if inspected.mounts.iter().any(|mount| mount.kind != "bind") {
            return Err(
                "Existing container has unmanaged volumes; explicit migration is required before replacement"
                    .to_string(),
            );
        }

        let mut expected = BTreeMap::new();
        let mut index = 0;
        while index + 1 < launch_args.len() {
            if launch_args[index] == "-v" {
                let specification = &launch_args[index + 1];
                let (destination, read_only) = if let Some(prefix) =
                    specification.strip_suffix(":/run/secrets/magician-keyring-password:ro")
                {
                    ("/run/secrets/magician-keyring-password", (prefix, true))
                } else if let Some(prefix) = specification.strip_suffix(":/keyring") {
                    ("/keyring", (prefix, false))
                } else if let Some(prefix) = specification.strip_suffix(":/data") {
                    ("/data", (prefix, false))
                } else {
                    index += 2;
                    continue;
                };
                expected.insert(
                    destination.to_string(),
                    (normalized_source(read_only.0), !read_only.1),
                );
                index += 2;
                continue;
            }
            index += 1;
        }
        if expected.len() != 3 {
            return Err("Invalid managed Windows container mount plan".to_string());
        }

        let actual = inspected
            .mounts
            .into_iter()
            .map(|mount| {
                (
                    mount.destination,
                    (normalized_source(&mount.source), mount.writable),
                )
            })
            .collect::<BTreeMap<_, _>>();
        if actual != expected {
            return Err(
                "Existing container mounts differ from managed custody; explicit migration is required before replacement"
                    .to_string(),
            );
        }

        let env = inspected
            .config
            .env
            .unwrap_or_default()
            .into_iter()
            .filter_map(|entry| {
                entry
                    .split_once('=')
                    .map(|(key, value)| (key.to_string(), value.to_string()))
            })
            .collect::<BTreeMap<_, _>>();
        if env.get("MAGICIAN_ROOT_DIR").map(String::as_str) != Some("/data")
            || env.get("MAGICIAN_KEYRING_STATE_DIR").map(String::as_str) != Some("/keyring")
            || env
                .get("MAGICIAN_KEYRING_PASSWORD_FILE")
                .map(String::as_str)
                != Some("/run/secrets/magician-keyring-password")
        {
            return Err(
                "Existing container keyring configuration differs; it was left unchanged"
                    .to_string(),
            );
        }
        Ok(())
    }

    #[cfg(target_os = "windows")]
    async fn verify_windows_keyring_image(
        &self,
        config: &ContainerConfig,
        launch_args: &[String],
    ) -> Result<(), String> {
        const CHECK: &str = r#"import os,pathlib,shutil,stat
assert os.getuid()!=0
assert all(shutil.which(p) for p in ('dbus-daemon','dbus-send','gnome-keyring-daemon','secret-tool'))
assert pathlib.Path('/app/scripts/run-linux-keyring.py').is_file()
assert '--check-inputs' in pathlib.Path('/app/scripts/container-entrypoint.sh').read_text()
assert os.access('/keyring',os.R_OK|os.W_OK|os.X_OK)
p=pathlib.Path('/run/secrets/magician-keyring-password')
s=p.stat(); assert stat.S_ISREG(s.st_mode)
b=p.read_bytes(); assert 32<=len(b)<=4096 and b'\0' not in b
assert not os.access(p,os.W_OK)
assert os.access('/data',os.R_OK|os.W_OK|os.X_OK)"#;

        let check_name = format!("magician-keyring-check-{}", uuid::Uuid::new_v4().simple());
        let mut args = vec![
            "run".to_string(),
            "--rm".to_string(),
            "--name".to_string(),
            check_name.clone(),
            "--cpus".to_string(),
            "1".to_string(),
            "--memory".to_string(),
            "512m".to_string(),
        ];
        args.extend_from_slice(launch_args);
        args.extend([
            "--entrypoint".to_string(),
            "python3".to_string(),
            config.image.clone(),
            "-c".to_string(),
            CHECK.to_string(),
        ]);

        let mut command = Command::new(docker_program());
        command.args(&args).kill_on_drop(true);
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(60), command.output()).await;
        match result {
            Ok(Ok(output)) if output.status.success() => Ok(()),
            Ok(_) => Err(
                "The Magician image cannot safely access its private Windows keyring mounts; the existing container was left unchanged"
                    .to_string(),
            ),
            Err(_) => {
                let _ = self.exec(&["rm", "-f", &check_name]).await;
                Err(
                    "Windows keyring image verification timed out; the existing container was left unchanged"
                        .to_string(),
                )
            },
        }
    }
}
