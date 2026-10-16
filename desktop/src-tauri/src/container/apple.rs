use async_trait::async_trait;
use reqwest::header::{ACCEPT, WWW_AUTHENTICATE};
use reqwest::{header::HeaderName, Method, Url};
use serde_json::Value;
use tokio::process::Command;
use tracing::{debug, info};

use super::{
    runtime_env_for_container, seed_runtime_config_if_missing, ContainerConfig, ContainerInfo,
    ContainerRuntime, ContainerStatus, PortMapping, ProgressCallback, APPLE_CONTAINER_HOST,
};

const APPLE_HOST_FORWARDING_ADDRESS: &str = "203.0.113.113";

/// Apple Container CLI runtime adapter.
///
/// Uses the `container` CLI available on macOS >= 26 with Apple Silicon.
/// Most run-time flags are Docker-like (`-v`, `-p`), but image management and
/// inspection use Apple Container's current command surface.
pub struct AppleContainerRuntime;

impl AppleContainerRuntime {
    pub fn new() -> Self {
        Self
    }

    /// Execute a `container` CLI command and return stdout.
    async fn exec(&self, args: &[&str]) -> Result<String, String> {
        debug!("container {}", args.join(" "));
        let output = Command::new(find_container_cli_path())
            .args(args)
            .output()
            .await
            .map_err(|e| format!("Failed to execute `container {}`: {}", args.join(" "), e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "`container {}` failed (exit {}): {}",
                args.join(" "),
                output.status.code().unwrap_or(-1),
                stderr.trim()
            ));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    async fn cli_is_installed(&self) -> bool {
        match Command::new(find_container_cli_path())
            .arg("--version")
            .output()
            .await
        {
            Ok(output) => {
                let available = output.status.success();
                if available {
                    let version = String::from_utf8_lossy(&output.stdout);
                    info!("Apple container CLI available: {}", version.trim());
                }
                available
            },
            Err(_) => false,
        }
    }

    async fn system_is_running(&self) -> bool {
        matches!(
            Command::new(find_container_cli_path())
                .args(["system", "status"])
                .output()
                .await,
            Ok(output) if output.status.success()
        )
    }

    async fn ensure_system_started(
        &self,
        progress: Option<&ProgressCallback>,
    ) -> Result<(), String> {
        if self.system_is_running().await {
            return Ok(());
        }

        if let Some(cb) = progress {
            cb(0.8, "Starting Apple Container system service...");
        }

        self.exec(&["system", "start", "--enable-kernel-install"])
            .await?;

        if self.system_is_running().await {
            if let Some(cb) = progress {
                cb(0.95, "Apple Container system service is running");
            }
            return Ok(());
        }

        Err("Apple Container CLI is installed, but `container system status` is not healthy after `container system start`.".to_string())
    }

    async fn host_service_dns_is_configured(&self) -> Result<bool, String> {
        let output = Command::new(find_container_cli_path())
            .args(["system", "dns", "list", "--quiet"])
            .output()
            .await
            .map_err(|e| format!("Failed to inspect Apple Container DNS domains: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "Failed to inspect Apple Container DNS domains: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(dns_domain_list_contains(
            &String::from_utf8_lossy(&output.stdout),
            APPLE_CONTAINER_HOST,
        ))
    }

    async fn ensure_host_service_dns(
        &self,
        progress: Option<&ProgressCallback>,
    ) -> Result<(), String> {
        if self.host_service_dns_is_configured().await? {
            return Ok(());
        }
        if let Some(cb) = progress {
            cb(
                0.97,
                "Authorizing Apple Container access to host services...",
            );
        }

        let container_cli = find_container_cli_path();
        let command = format!(
            "{container_cli} system dns create {APPLE_CONTAINER_HOST} --localhost {APPLE_HOST_FORWARDING_ADDRESS}"
        );
        super::run_with_admin(
            &command,
            "configure Apple Container access to Magican host services",
        )
        .await?;

        if !self.host_service_dns_is_configured().await? {
            return Err(format!(
                "Apple Container did not retain the required `{APPLE_CONTAINER_HOST}` localhost domain"
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl ContainerRuntime for AppleContainerRuntime {
    fn host_relay_command(&self, name: &str) -> Result<Command, String> {
        let mut command = Command::new(find_container_cli_path());
        command.args(["exec", "--interactive", name]);
        Ok(command)
    }

    fn name(&self) -> &str {
        "Apple Container"
    }

    async fn is_available(&self) -> bool {
        self.cli_is_installed().await && self.system_is_running().await
    }

    async fn install(&self, progress: Option<&ProgressCallback>) -> Result<(), String> {
        if let Some(cb) = progress {
            cb(0.0, "Preparing to install Apple container CLI...");
        }

        if !self.cli_is_installed().await {
            // Ensure Homebrew is available
            if !super::is_homebrew_available().await {
                if let Some(cb) = progress {
                    cb(
                        0.1,
                        "Installing Homebrew (required for Apple Container CLI)...",
                    );
                }
                super::install_homebrew(progress).await?;
            }

            if let Some(cb) = progress {
                cb(0.4, "Installing Apple Container CLI via Homebrew...");
            }

            // Use the full path to brew in case PATH isn't updated yet
            let brew_path = super::find_brew_path();

            let brew_result = Command::new(&brew_path)
                .args(["install", "container"])
                .output()
                .await;

            match brew_result {
                Ok(output) if output.status.success() => {
                    info!("Installed container CLI via Homebrew");
                },
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(format!(
                        "Failed to install Apple Container CLI: {}. Try the signed installer package from https://github.com/apple/container/releases",
                        stderr.trim()
                    ));
                },
                Err(e) => {
                    return Err(format!(
                        "Failed to run Homebrew: {}. Install Apple Container manually from https://github.com/apple/container/releases",
                        e
                    ));
                },
            }
        }

        self.ensure_system_started(progress).await?;
        self.ensure_host_service_dns(progress).await?;

        if let Some(cb) = progress {
            cb(
                1.0,
                "Apple Container CLI installed and system service running",
            );
        }
        Ok(())
    }

    async fn pull_image(
        &self,
        image: &str,
        progress: Option<&ProgressCallback>,
    ) -> Result<(), String> {
        if let Some(cb) = progress {
            cb(0.0, &format!("Pulling image {}...", image));
        }

        self.ensure_system_started(None).await?;
        self.exec(&["image", "pull", image]).await?;

        if let Some(cb) = progress {
            cb(1.0, "Image pull complete");
        }
        info!("Pulled image: {}", image);
        Ok(())
    }

    async fn image_exists(&self, image: &str) -> Result<bool, String> {
        // `container image inspect` returns 0 if image exists
        match Command::new(find_container_cli_path())
            .args(["image", "inspect", image])
            .output()
            .await
        {
            Ok(output) => Ok(output.status.success()),
            Err(e) => Err(format!("Failed to check image: {}", e)),
        }
    }

    async fn prepare_keyring(&self, config: &ContainerConfig) -> Result<Vec<String>, String> {
        super::keyring::prepare(config, "apple-container", find_container_cli_path()).await
    }

    async fn start(&self, config: &ContainerConfig) -> Result<(), String> {
        self.ensure_system_started(None).await?;
        self.ensure_host_service_dns(None).await?;
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
        ];

        // Canonical runtime root and private persistent credential mounts.
        args.extend(keyring_args);

        // Port mappings
        for (host, container) in &config.ports {
            args.push("-p".to_string());
            args.push(format!("{}:{}", host, container));
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
        let env_vars = runtime_env_for_container(config, APPLE_CONTAINER_HOST)?;
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

    async fn start_existing(&self, name: &str) -> Result<(), String> {
        self.exec(&["start", name]).await?;
        info!("Started existing container '{}'", name);
        Ok(())
    }

    async fn remove(&self, name: &str) -> Result<(), String> {
        self.exec(&["rm", name]).await?;
        info!("Removed container '{}'", name);
        Ok(())
    }

    async fn container_info(&self, name: &str) -> Result<ContainerInfo, String> {
        let output = Command::new(find_container_cli_path())
            .args(["inspect", name])
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
        let json: Value = serde_json::from_str(&raw).map_err(|e| {
            format!(
                "Failed to parse container inspect JSON for '{}': {}",
                name, e
            )
        })?;
        let item = first_json_item(&json).unwrap_or(&json);

        parse_container_info(name, item)
    }

    async fn logs(&self, name: &str, lines: usize) -> Result<String, String> {
        self.ensure_system_started(None).await?;
        self.exec(&["logs", "-n", &lines.to_string(), name]).await
    }

    async fn exec_in_container(&self, name: &str, command: &[&str]) -> Result<String, String> {
        self.ensure_system_started(None).await?;
        let mut args = Vec::with_capacity(command.len() + 2);
        args.push("exec");
        args.push(name);
        args.extend_from_slice(command);
        self.exec(&args).await
    }

    async fn image_digest(&self, image: &str) -> Result<String, String> {
        let output = self.exec(&["image", "inspect", image]).await?;
        let json: Value = serde_json::from_str(&output)
            .map_err(|e| format!("Failed to parse image inspect JSON for '{}': {}", image, e))?;
        extract_digest(&json).ok_or_else(|| format!("No digest found for local image '{}'", image))
    }

    async fn tag_image(&self, image: &str, new_tag: &str) -> Result<(), String> {
        self.ensure_system_started(None).await?;
        self.exec(&["image", "tag", image, new_tag]).await?;
        info!("Tagged image '{}' as '{}'", image, new_tag);
        Ok(())
    }

    async fn remote_image_digest(&self, image: &str) -> Result<String, String> {
        remote_oci_digest(image).await
    }
}

pub(super) fn find_container_cli_path() -> &'static str {
    for candidate in ["/opt/homebrew/bin/container", "/usr/local/bin/container"] {
        if std::path::Path::new(candidate).is_file() {
            return candidate;
        }
    }
    "container"
}

fn dns_domain_list_contains(output: &str, expected: &str) -> bool {
    output.lines().any(|line| line.trim() == expected)
}

fn parse_container_info(name: &str, item: &Value) -> Result<ContainerInfo, String> {
    let status_raw = get_string_path(item, &["status", "state"])
        .or_else(|| get_string_path(item, &["status"]))
        .or_else(|| get_string_path(item, &["State", "Status"]))
        .or_else(|| get_string_path(item, &["state", "status"]))
        .unwrap_or_default();
    let status = match status_raw.as_str() {
        "running" => ContainerStatus::Running,
        "restarting" => ContainerStatus::Restarting,
        "exited" | "stopped" => ContainerStatus::Stopped,
        _ => {
            return Err(format!(
                "Unknown Apple Container state for {name}: {status_raw:?}"
            ))
        },
    };

    let image = get_string_path(item, &["configuration", "image", "reference"])
        .or_else(|| get_string_path(item, &["configuration", "image"]))
        .or_else(|| get_string_path(item, &["configuration", "imageReference"]))
        .or_else(|| get_string_path(item, &["image"]))
        .or_else(|| get_string_path(item, &["Config", "Image"]))
        .unwrap_or_default();
    let created_at = get_string_path(item, &["configuration", "creationDate"])
        .or_else(|| get_string_path(item, &["created"]))
        .or_else(|| get_string_path(item, &["createdAt"]))
        .or_else(|| get_string_path(item, &["Created"]));
    let ports = parse_ports_from_inspect(item);

    Ok(ContainerInfo {
        name: name.to_string(),
        image,
        status,
        ports,
        created_at,
    })
}

fn first_json_item(value: &Value) -> Option<&Value> {
    value.as_array().and_then(|items| items.first())
}

fn get_string_path(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str().map(|s| s.to_string())
}

fn parse_ports_from_inspect(value: &Value) -> Vec<PortMapping> {
    let mut ports = Vec::new();
    collect_port_mappings(value, &mut ports);
    ports
}

fn collect_port_mappings(value: &Value, ports: &mut Vec<PortMapping>) {
    match value {
        Value::Object(map) => {
            // Docker-compatible inspect shape:
            // NetworkSettings.Ports."3002/tcp"[0].HostPort = "13002"
            for (key, value) in map {
                if let Some(container_port) = port_key_to_u16(key) {
                    if let Some(host_port) = host_port_from_value(value) {
                        push_unique_port(ports, host_port, container_port);
                    }
                }
            }

            // Future-proof against Apple JSON shapes that expose explicit
            // host/container port objects.
            if let (Some(host), Some(container)) = (
                get_port_field(map, &["hostPort", "host_port", "host"]),
                get_port_field(map, &["containerPort", "container_port", "container"]),
            ) {
                push_unique_port(ports, host, container);
            }

            for value in map.values() {
                collect_port_mappings(value, ports);
            }
        },
        Value::Array(items) => {
            for value in items {
                collect_port_mappings(value, ports);
            }
        },
        _ => {},
    }
}

fn port_key_to_u16(key: &str) -> Option<u16> {
    key.split('/')
        .next()
        .and_then(|port| port.parse::<u16>().ok())
}

fn host_port_from_value(value: &Value) -> Option<u16> {
    match value {
        Value::Array(items) => items
            .iter()
            .find_map(|item| get_string_path(item, &["HostPort"]))
            .and_then(|port| port.parse::<u16>().ok()),
        Value::Object(_) => get_string_path(value, &["HostPort"])
            .or_else(|| get_string_path(value, &["hostPort"]))
            .and_then(|port| port.parse::<u16>().ok()),
        Value::String(port) => port.parse::<u16>().ok(),
        Value::Number(port) => port.as_u64().and_then(|p| u16::try_from(p).ok()),
        _ => None,
    }
}

fn get_port_field(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<u16> {
    keys.iter().find_map(|key| {
        map.get(*key).and_then(|value| match value {
            Value::String(port) => port.parse::<u16>().ok(),
            Value::Number(port) => port.as_u64().and_then(|p| u16::try_from(p).ok()),
            _ => None,
        })
    })
}

fn push_unique_port(ports: &mut Vec<PortMapping>, host: u16, container: u16) {
    if !ports
        .iter()
        .any(|existing| existing.host == host && existing.container == container)
    {
        ports.push(PortMapping { host, container });
    }
}

fn extract_digest(value: &Value) -> Option<String> {
    if let Some(digest) = find_repo_digest(value) {
        return Some(digest);
    }
    find_digest_field(value, false).or_else(|| find_digest_field(value, true))
}

fn find_repo_digest(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in ["repoDigests", "RepoDigests"] {
                if let Some(Value::Array(items)) = map.get(key) {
                    if let Some(digest) = items.iter().filter_map(|v| v.as_str()).find_map(|s| {
                        s.rsplit_once('@')
                            .map(|(_, digest)| digest.to_string())
                            .or_else(|| s.starts_with("sha256:").then(|| s.to_string()))
                    }) {
                        return Some(digest);
                    }
                }
            }
            map.values().find_map(find_repo_digest)
        },
        Value::Array(items) => items.iter().find_map(find_repo_digest),
        _ => None,
    }
}

fn find_digest_field(value: &Value, allow_id: bool) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in ["digest", "Digest"] {
                if let Some(digest) = map.get(key).and_then(|v| v.as_str()) {
                    if digest.starts_with("sha256:") {
                        return Some(digest.to_string());
                    }
                }
            }
            if allow_id {
                for key in ["id", "Id", "ID"] {
                    if let Some(digest) = map.get(key).and_then(|v| v.as_str()) {
                        if digest.starts_with("sha256:") {
                            return Some(digest.to_string());
                        }
                    }
                }
            }
            map.values()
                .find_map(|value| find_digest_field(value, allow_id))
        },
        Value::Array(items) => items
            .iter()
            .find_map(|value| find_digest_field(value, allow_id)),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OciImageRef {
    registry: String,
    repository: String,
    reference: String,
}

fn parse_oci_image_ref(image: &str) -> Result<OciImageRef, String> {
    let image = image.trim();
    if image.is_empty() {
        return Err("image reference is empty".to_string());
    }

    if let Some((without_digest, digest)) = image.rsplit_once('@') {
        let mut parsed = parse_oci_image_ref(without_digest)?;
        parsed.reference = digest.to_string();
        return Ok(parsed);
    }

    let parts: Vec<&str> = image.split('/').collect();
    let first = parts[0];
    let first_is_registry = first.contains('.') || first.contains(':') || first == "localhost";

    let (registry, repository_parts) = if first_is_registry {
        if parts.len() < 2 {
            return Err(format!("image reference '{}' has no repository", image));
        }
        (first.to_string(), &parts[1..])
    } else {
        ("docker.io".to_string(), parts.as_slice())
    };

    let mut repository = repository_parts.join("/");
    if registry == "docker.io" && !repository.contains('/') {
        repository = format!("library/{}", repository);
    }

    let last_slash = repository.rfind('/');
    let tag_colon = repository.rfind(':');
    let reference = if let Some(colon) = tag_colon {
        if last_slash.map(|slash| colon > slash).unwrap_or(true) {
            let reference = repository[colon + 1..].to_string();
            repository.truncate(colon);
            reference
        } else {
            "latest".to_string()
        }
    } else {
        "latest".to_string()
    };

    Ok(OciImageRef {
        registry,
        repository,
        reference,
    })
}

async fn remote_oci_digest(image: &str) -> Result<String, String> {
    let image_ref = parse_oci_image_ref(image)?;
    if image_ref.reference.starts_with("sha256:") {
        return Ok(image_ref.reference);
    }

    let registry_host = if image_ref.registry == "docker.io" {
        "registry-1.docker.io"
    } else {
        image_ref.registry.as_str()
    };
    let url = format!(
        "https://{}/v2/{}/manifests/{}",
        registry_host, image_ref.repository, image_ref.reference
    );
    let url =
        Url::parse(&url).map_err(|e| format!("Invalid registry URL for '{}': {}", image, e))?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| format!("Failed to build registry client: {}", e))?;

    let mut token = None;
    let mut response = send_manifest_request(&client, Method::HEAD, url.clone(), None).await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        token = Some(
            bearer_token_for_challenge(
                &client,
                response
                    .headers()
                    .get(WWW_AUTHENTICATE)
                    .and_then(|value| value.to_str().ok()),
                &image_ref,
            )
            .await?,
        );
        response =
            send_manifest_request(&client, Method::HEAD, url.clone(), token.as_deref()).await?;
    }

    if response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED {
        response =
            send_manifest_request(&client, Method::GET, url.clone(), token.as_deref()).await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED && token.is_none() {
            token = Some(
                bearer_token_for_challenge(
                    &client,
                    response
                        .headers()
                        .get(WWW_AUTHENTICATE)
                        .and_then(|value| value.to_str().ok()),
                    &image_ref,
                )
                .await?,
            );
            response =
                send_manifest_request(&client, Method::GET, url.clone(), token.as_deref()).await?;
        }
    }

    if !response.status().is_success() {
        return Err(format!(
            "Remote manifest lookup for '{}' failed with HTTP {}",
            image,
            response.status()
        ));
    }

    if let Some(digest) = docker_content_digest(&response) {
        return Ok(digest);
    }

    let response =
        send_manifest_request(&client, Method::GET, url.clone(), token.as_deref()).await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED && token.is_none() {
        let token = bearer_token_for_challenge(
            &client,
            response
                .headers()
                .get(WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok()),
            &image_ref,
        )
        .await?;
        let response =
            send_manifest_request(&client, Method::GET, url.clone(), Some(&token)).await?;
        return remote_digest_from_get_response(image, response).await;
    }

    remote_digest_from_get_response(image, response).await
}

async fn remote_digest_from_get_response(
    image: &str,
    response: reqwest::Response,
) -> Result<String, String> {
    if !response.status().is_success() {
        return Err(format!(
            "Remote manifest lookup for '{}' failed with HTTP {}",
            image,
            response.status()
        ));
    }

    if let Some(digest) = docker_content_digest(&response) {
        return Ok(digest);
    }

    let body: Value = response.json().await.map_err(|e| {
        format!(
            "Failed to parse remote manifest JSON for '{}': {}",
            image, e
        )
    })?;
    extract_digest(&body).ok_or_else(|| format!("No remote digest found for '{}'", image))
}

async fn send_manifest_request(
    client: &reqwest::Client,
    method: Method,
    url: Url,
    token: Option<&str>,
) -> Result<reqwest::Response, String> {
    let mut request = client.request(method, url).header(
        ACCEPT,
        [
            "application/vnd.oci.image.index.v1+json",
            "application/vnd.oci.image.manifest.v1+json",
            "application/vnd.docker.distribution.manifest.list.v2+json",
            "application/vnd.docker.distribution.manifest.v2+json",
        ]
        .join(", "),
    );
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    request
        .send()
        .await
        .map_err(|e| format!("Remote manifest request failed: {}", e))
}

fn docker_content_digest(response: &reqwest::Response) -> Option<String> {
    let header = HeaderName::from_static("docker-content-digest");
    response
        .headers()
        .get(header)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string())
}

async fn bearer_token_for_challenge(
    client: &reqwest::Client,
    challenge: Option<&str>,
    image_ref: &OciImageRef,
) -> Result<String, String> {
    let challenge =
        challenge.ok_or_else(|| "Registry requested auth without WWW-Authenticate".to_string())?;
    let params = parse_bearer_challenge(challenge)
        .ok_or_else(|| format!("Unsupported registry auth challenge: {}", challenge))?;
    let realm = params
        .get("realm")
        .ok_or_else(|| format!("Registry auth challenge missing realm: {}", challenge))?;
    let mut url =
        Url::parse(realm).map_err(|e| format!("Invalid registry auth realm '{}': {}", realm, e))?;

    {
        let mut query = url.query_pairs_mut();
        if let Some(service) = params.get("service") {
            query.append_pair("service", service);
        }
        if let Some(scope) = params.get("scope") {
            query.append_pair("scope", scope);
        } else {
            query.append_pair(
                "scope",
                &format!("repository:{}:pull", image_ref.repository),
            );
        }
    }

    let body: Value = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Registry token request failed: {}", e))?
        .error_for_status()
        .map_err(|e| format!("Registry token request failed: {}", e))?
        .json()
        .await
        .map_err(|e| format!("Failed to parse registry token response: {}", e))?;

    body.get("token")
        .or_else(|| body.get("access_token"))
        .and_then(|value| value.as_str())
        .map(|token| token.to_string())
        .ok_or_else(|| "Registry token response did not contain token".to_string())
}

fn parse_bearer_challenge(challenge: &str) -> Option<std::collections::HashMap<String, String>> {
    let challenge = challenge.trim();
    let rest = challenge.strip_prefix("Bearer ")?;
    let mut params = std::collections::HashMap::new();
    for part in rest.split(',') {
        let (key, value) = part.trim().split_once('=')?;
        params.insert(key.to_string(), value.trim_matches('"').to_string());
    }
    Some(params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn container_routing_apple_v1_running_is_never_classified_as_stopped() {
        let value = json!({
            "configuration": {
                "image": { "reference": "magician:test" },
                "creationDate": "2026-09-16T09:04:12Z",
                "publishedPorts": [{ "hostPort": 13002, "containerPort": 3002 }]
            },
            "status": { "state": "running", "networks": [] }
        });
        let info = parse_container_info("integration", &value).unwrap();
        assert_eq!(info.status, ContainerStatus::Running);
        assert_eq!(info.image, "magician:test");
        assert_eq!(info.created_at.as_deref(), Some("2026-09-16T09:04:12Z"));
        assert_eq!(info.ports[0].host, 13002);
    }

    #[test]
    fn container_routing_unknown_inspection_cannot_authorize_replacement() {
        for value in [json!({}), json!({ "status": { "state": "future-state" } })] {
            assert!(parse_container_info("integration", &value).is_err());
        }
        let legacy = parse_container_info("legacy", &json!({ "status": "running" })).unwrap();
        assert_eq!(legacy.status, ContainerStatus::Running);
        let stopped =
            parse_container_info("stopped", &json!({ "status": { "state": "stopped" } })).unwrap();
        assert_eq!(stopped.status, ContainerStatus::Stopped);
    }

    #[test]
    fn parses_ghcr_image_reference_with_tag() {
        let parsed = parse_oci_image_ref("ghcr.io/magicbeanbs100x/magician:latest").unwrap();

        assert_eq!(
            parsed,
            OciImageRef {
                registry: "ghcr.io".to_string(),
                repository: "magicbeanbs100x/magician".to_string(),
                reference: "latest".to_string(),
            }
        );
    }

    #[test]
    fn parses_docker_hub_short_name_as_library_latest() {
        let parsed = parse_oci_image_ref("alpine").unwrap();

        assert_eq!(
            parsed,
            OciImageRef {
                registry: "docker.io".to_string(),
                repository: "library/alpine".to_string(),
                reference: "latest".to_string(),
            }
        );
    }

    #[test]
    fn extracts_repo_digest_without_repository_prefix() {
        let value = json!([
            {
                "repoDigests": [
                    "ghcr.io/magicbeanbs100x/magician@sha256:abc123"
                ]
            }
        ]);

        assert_eq!(extract_digest(&value).as_deref(), Some("sha256:abc123"));
    }

    #[test]
    fn parses_docker_style_ports_from_json_inspect() {
        let value = json!({
            "NetworkSettings": {
                "Ports": {
                    "3002/tcp": [{"HostPort": "13002"}],
                    "3003/tcp": [{"HostPort": "13003"}]
                }
            }
        });

        let ports = parse_ports_from_inspect(&value);

        assert_eq!(ports.len(), 2);
        assert_eq!(ports[0].host, 13002);
        assert_eq!(ports[0].container, 3002);
        assert_eq!(ports[1].host, 13003);
        assert_eq!(ports[1].container, 3003);
    }

    #[test]
    fn parses_registry_bearer_challenge() {
        let params = parse_bearer_challenge(
            r#"Bearer realm="https://ghcr.io/token",service="ghcr.io",scope="repository:owner/image:pull""#,
        )
        .unwrap();

        assert_eq!(params.get("realm").unwrap(), "https://ghcr.io/token");
        assert_eq!(params.get("service").unwrap(), "ghcr.io");
        assert_eq!(params.get("scope").unwrap(), "repository:owner/image:pull");
    }

    #[test]
    fn apple_host_dns_list_requires_an_exact_domain() {
        let output = "test\nhost.container.internal\nother.test\n";

        assert!(dns_domain_list_contains(output, APPLE_CONTAINER_HOST));
        assert!(!dns_domain_list_contains(
            "host.container.internal.example\n",
            APPLE_CONTAINER_HOST
        ));
    }
}
