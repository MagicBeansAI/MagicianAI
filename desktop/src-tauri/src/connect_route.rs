//! Desktop-owned selection for the stable mobile connection hostname.
//!
//! Mobile clients keep one remote origin (`connect.<zone>`). This module
//! changes only that hostname's `/health` and `/api/*` tunnel rules; `/host/*`
//! remains attached to this desktop so host tools continue to execute here.

use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const HEALTH_PATH: &str = "^/health/?$";
const API_PATH: &str = "^/api(/.*)?$";

#[derive(Debug, Clone, Deserialize)]
pub struct ConnectRouteSelection {
    pub backend: String,
    #[serde(default)]
    pub remote_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectRouteStatus {
    pub hostname: String,
    pub public_origin: String,
    pub selected_backend: String,
    pub local_url: String,
    pub local_health: String,
    pub container_url: String,
    pub container_health: String,
    pub remote_url: Option<String>,
    pub remote_health: String,
    pub management_available: bool,
    pub management_message: String,
}

#[derive(Debug, Clone)]
struct RouteSettings {
    root: PathBuf,
    hostname: String,
    selected_backend: String,
    local_port: u16,
    container_port: u16,
    remote_url: Option<String>,
    tunnel_mode: String,
    cloudflare_api_token: Option<String>,
    connector_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConnectorIdentity {
    a: String,
    t: String,
}

#[tauri::command]
pub async fn get_connect_route_status(
    window: tauri::WebviewWindow,
) -> Result<ConnectRouteStatus, String> {
    require_settings_window(&window)?;
    status_for(&RouteSettings::load()?).await
}

#[tauri::command]
pub async fn set_connect_route(
    window: tauri::WebviewWindow,
    selection: ConnectRouteSelection,
) -> Result<ConnectRouteStatus, String> {
    require_settings_window(&window)?;
    let mut settings = RouteSettings::load()?;
    if settings.tunnel_mode != "token" {
        return Err(
            "Desktop route switching requires the dashboard-managed token tunnel. Set MAGICIAN_TUNNEL_MODE=token during connection setup."
                .to_string(),
        );
    }

    let (service, health_origin, remote_url) = settings.resolve_selection(&selection)?;
    ensure_healthy(&health_origin).await?;
    update_cloudflare_route(&settings, &service).await?;
    persist_selection(&settings.root, &selection.backend, remote_url.as_deref())?;

    settings.selected_backend = selection.backend;
    if remote_url.is_some() {
        settings.remote_url = remote_url;
    }
    status_for(&settings).await
}

fn require_settings_window(window: &tauri::WebviewWindow) -> Result<(), String> {
    if window.label() == "settings" {
        Ok(())
    } else {
        Err("Only Magican Desktop Settings can manage the public device route.".to_string())
    }
}

impl RouteSettings {
    fn load() -> Result<Self, String> {
        let root = crate::runtime_paths::runtime_root_dir();
        let value = |key: &str| read_setting(&root, key);
        let zone = value("MAGICIAN_TUNNEL_ZONE");
        let hostname = value("MAGICIAN_CONNECT_HOST")
            .or_else(|| value("MAGICIAN_IOS_HOST"))
            .or_else(|| zone.map(|zone| format!("connect.{zone}")))
            .ok_or_else(|| {
                "Device connection hostname is not configured. Run connection setup first."
                    .to_string()
            })?;
        validate_hostname(&hostname)?;

        Ok(Self {
            root: root.clone(),
            hostname,
            selected_backend: value("MAGICIAN_CONNECT_BACKEND")
                .unwrap_or_else(|| "local".to_string())
                .to_ascii_lowercase(),
            local_port: parse_port(value("MAGICIAN_CONNECT_LOCAL_API_PORT"), 3002)?,
            container_port: parse_port(value("MAGICIAN_CONNECT_CONTAINER_API_PORT"), 13002)?,
            remote_url: value("MAGICIAN_CONNECT_REMOTE_URL")
                .map(|value| value.trim_end_matches('/').to_string()),
            tunnel_mode: value("MAGICIAN_TUNNEL_MODE")
                .unwrap_or_else(|| "browser".to_string())
                .to_ascii_lowercase(),
            cloudflare_api_token: value("CLOUDFLARE_API_TOKEN").or_else(|| value("CF_API_TOKEN")),
            connector_token: value("CLOUDFLARED_TOKEN"),
        })
    }

    fn resolve_selection(
        &self,
        selection: &ConnectRouteSelection,
    ) -> Result<(String, String, Option<String>), String> {
        match selection.backend.as_str() {
            "local" => Ok((
                format!("http://localhost:{}", self.local_port),
                format!("http://127.0.0.1:{}", self.local_port),
                None,
            )),
            "container" => Ok((
                format!("http://localhost:{}", self.container_port),
                format!("http://127.0.0.1:{}", self.container_port),
                None,
            )),
            "remote" => {
                let raw = selection
                    .remote_url
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .or(self.remote_url.as_deref())
                    .ok_or_else(|| "Enter the remote Magician HTTPS origin.".to_string())?;
                let origin = validate_remote_origin(raw, &self.hostname)?;
                Ok((origin.clone(), origin.clone(), Some(origin)))
            },
            _ => Err("Remote device route must be local, container, or remote.".to_string()),
        }
    }
}

async fn status_for(settings: &RouteSettings) -> Result<ConnectRouteStatus, String> {
    let client = health_client()?;
    let local_url = format!("http://127.0.0.1:{}", settings.local_port);
    let container_url = format!("http://127.0.0.1:{}", settings.container_port);
    let local_health = health_label(&client, &local_url).await;
    let container_health = health_label(&client, &container_url).await;
    let remote_health = match settings.remote_url.as_deref() {
        Some(origin) => health_label(&client, origin).await,
        None => "unconfigured".to_string(),
    };
    let management_available = settings.tunnel_mode == "token"
        && settings.cloudflare_api_token.is_some()
        && settings.connector_token.is_some();
    let management_message = if management_available {
        "Ready to switch the dashboard-managed tunnel.".to_string()
    } else if settings.tunnel_mode != "token" {
        "Desktop switching needs MAGICIAN_TUNNEL_MODE=token; browser-managed tunnels remain available through the Make commands."
            .to_string()
    } else {
        "Cloudflare tunnel credentials are incomplete in the local runtime environment.".to_string()
    };

    Ok(ConnectRouteStatus {
        hostname: settings.hostname.clone(),
        public_origin: format!("https://{}", settings.hostname),
        selected_backend: settings.selected_backend.clone(),
        local_url,
        local_health,
        container_url,
        container_health,
        remote_url: settings.remote_url.clone(),
        remote_health,
        management_available,
        management_message,
    })
}

fn health_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("Could not create the route health client: {error}"))
}

async fn health_label(client: &reqwest::Client, origin: &str) -> String {
    let url = format!("{}/health", origin.trim_end_matches('/'));
    match client.get(url).send().await {
        Ok(response) if response.status().is_success() => "healthy".to_string(),
        Ok(response) => format!("HTTP {}", response.status().as_u16()),
        Err(error) if error.is_timeout() => "timeout".to_string(),
        Err(_) => "unavailable".to_string(),
    }
}

async fn ensure_healthy(origin: &str) -> Result<(), String> {
    let label = health_label(&health_client()?, origin).await;
    if label == "healthy" {
        Ok(())
    } else {
        Err(format!(
            "The selected backend did not pass {}/health ({label}); the public route was not changed.",
            origin.trim_end_matches('/')
        ))
    }
}

async fn update_cloudflare_route(settings: &RouteSettings, service: &str) -> Result<(), String> {
    let api_token = settings.cloudflare_api_token.as_deref().ok_or_else(|| {
        "CLOUDFLARE_API_TOKEN is missing from the local runtime environment.".to_string()
    })?;
    let connector_token = settings.connector_token.as_deref().ok_or_else(|| {
        "CLOUDFLARED_TOKEN is missing from the local runtime environment.".to_string()
    })?;
    let identity = decode_connector_identity(connector_token)?;
    let endpoint = format!(
        "https://api.cloudflare.com/client/v4/accounts/{}/cfd_tunnel/{}/configurations",
        identity.a, identity.t
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("Could not create the Cloudflare client: {error}"))?;

    let current = cloudflare_request(client.get(&endpoint).bearer_auth(api_token)).await?;
    let mut config = current
        .pointer("/result/config")
        .cloned()
        .ok_or_else(|| "Cloudflare returned no tunnel ingress configuration.".to_string())?;
    let ingress = config
        .get_mut("ingress")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| {
            "Cloudflare returned an invalid tunnel ingress configuration.".to_string()
        })?;
    rewrite_connect_ingress(ingress, &settings.hostname, service)?;

    cloudflare_request(
        client
            .put(&endpoint)
            .bearer_auth(api_token)
            .json(&json!({ "config": config })),
    )
    .await?;

    let verified = cloudflare_request(client.get(&endpoint).bearer_auth(api_token)).await?;
    let verified_ingress = verified
        .pointer("/result/config/ingress")
        .and_then(Value::as_array)
        .ok_or_else(|| "Cloudflare did not return the updated tunnel ingress.".to_string())?;
    verify_connect_ingress(verified_ingress, &settings.hostname, service)
}

async fn cloudflare_request(request: reqwest::RequestBuilder) -> Result<Value, String> {
    let response = request
        .send()
        .await
        .map_err(|error| format!("Cloudflare tunnel request failed: {error}"))?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|_| format!("Cloudflare returned an unreadable response (HTTP {status})."))?;
    if !status.is_success() || body.get("success").and_then(Value::as_bool) != Some(true) {
        let message = body
            .get("errors")
            .and_then(Value::as_array)
            .and_then(|errors| errors.first())
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("Cloudflare rejected the tunnel change");
        return Err(format!("{message} (HTTP {status})."));
    }
    Ok(body)
}

fn rewrite_connect_ingress(
    ingress: &mut [Value],
    hostname: &str,
    service: &str,
) -> Result<(), String> {
    let mut health = 0;
    let mut api = 0;
    for rule in ingress {
        if rule.get("hostname").and_then(Value::as_str) != Some(hostname) {
            continue;
        }
        match rule.get("path").and_then(Value::as_str) {
            Some(HEALTH_PATH) => {
                rule["service"] = Value::String(service.to_string());
                health += 1;
            },
            Some(API_PATH) => {
                rule["service"] = Value::String(service.to_string());
                api += 1;
            },
            _ => {},
        }
    }
    if health == 1 && api == 1 {
        Ok(())
    } else {
        Err(
            "The tunnel does not contain exactly one managed /health and /api route. Run connection setup before switching targets."
                .to_string(),
        )
    }
}

fn verify_connect_ingress(ingress: &[Value], hostname: &str, service: &str) -> Result<(), String> {
    let matches = |path: &str| {
        ingress
            .iter()
            .filter(|rule| {
                rule.get("hostname").and_then(Value::as_str) == Some(hostname)
                    && rule.get("path").and_then(Value::as_str) == Some(path)
                    && rule.get("service").and_then(Value::as_str) == Some(service)
            })
            .count()
    };
    if matches(HEALTH_PATH) == 1 && matches(API_PATH) == 1 {
        Ok(())
    } else {
        Err(
            "Cloudflare did not confirm both managed device routes; the selection was not saved."
                .to_string(),
        )
    }
}

fn decode_connector_identity(token: &str) -> Result<ConnectorIdentity, String> {
    let decoded = [
        general_purpose::STANDARD.decode(token),
        general_purpose::STANDARD_NO_PAD.decode(token),
        general_purpose::URL_SAFE_NO_PAD.decode(token),
    ]
    .into_iter()
    .find_map(Result::ok)
    .ok_or_else(|| "CLOUDFLARED_TOKEN is not a valid connector token.".to_string())?;
    let identity: ConnectorIdentity = serde_json::from_slice(&decoded)
        .map_err(|_| "CLOUDFLARED_TOKEN does not identify a tunnel.".to_string())?;
    if identity.a.trim().is_empty() || identity.t.trim().is_empty() {
        return Err("CLOUDFLARED_TOKEN does not identify an account and tunnel.".to_string());
    }
    Ok(identity)
}

fn validate_remote_origin(raw: &str, connect_hostname: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(raw)
        .map_err(|_| "Remote Magician must be a valid HTTPS origin.".to_string())?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || !matches!(parsed.path(), "" | "/")
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(
            "Remote Magician must be an HTTPS origin with no path, query, credentials, or fragment."
                .to_string(),
        );
    }
    if parsed
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case(connect_hostname))
    {
        return Err("The remote backend cannot be the public connection hostname; that would create a routing loop."
            .to_string());
    }
    Ok(raw.trim().trim_end_matches('/').to_string())
}

fn validate_hostname(hostname: &str) -> Result<(), String> {
    let valid = !hostname.is_empty()
        && hostname.len() <= 253
        && hostname.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
    if valid {
        Ok(())
    } else {
        Err("The configured device connection hostname is invalid.".to_string())
    }
}

fn parse_port(value: Option<String>, default: u16) -> Result<u16, String> {
    match value {
        Some(value) => value
            .parse::<u16>()
            .map_err(|_| format!("Invalid device-route port: {value}")),
        None => Ok(default),
    }
}

fn read_setting(root: &Path, key: &str) -> Option<String> {
    if let Ok(value) = std::env::var(key) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    for filename in [".env.development", ".env"] {
        let values = fs::read_to_string(root.join(filename))
            .ok()
            .map(|content| parse_env(&content))
            .unwrap_or_default();
        if let Some(value) = values.get(key).filter(|value| !value.is_empty()) {
            return Some(value.clone());
        }
    }
    None
}

fn parse_env(content: &str) -> BTreeMap<String, String> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let line = line.strip_prefix("export ").unwrap_or(line);
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, raw) = line.split_once('=')?;
            let raw = raw.trim();
            let value = if raw.len() >= 2
                && ((raw.starts_with('"') && raw.ends_with('"'))
                    || (raw.starts_with('\'') && raw.ends_with('\'')))
            {
                &raw[1..raw.len() - 1]
            } else {
                raw
            };
            Some((key.trim().to_string(), value.to_string()))
        })
        .collect()
}

fn persist_selection(root: &Path, backend: &str, remote_url: Option<&str>) -> Result<(), String> {
    for filename in [".env", ".env.development"] {
        let path = root.join(filename);
        let mut updates = vec![("MAGICIAN_CONNECT_BACKEND", backend)];
        if let Some(remote_url) = remote_url {
            updates.push(("MAGICIAN_CONNECT_REMOTE_URL", remote_url));
        }
        update_env_file(&path, &updates)?;
    }
    Ok(())
}

fn update_env_file(path: &Path, updates: &[(&str, &str)]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    }
    let existing = fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = existing.lines().map(ToString::to_string).collect();
    for (key, value) in updates {
        lines.retain(|line| {
            let trimmed = line
                .trim_start()
                .strip_prefix("export ")
                .unwrap_or(line.trim_start());
            !trimmed.starts_with(&format!("{key}="))
        });
        lines.push(format!("{key}={value}"));
    }
    let mut rendered = lines.join("\n");
    rendered.push('\n');
    fs::write(path, rendered)
        .map_err(|error| format!("Could not update {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ingress() -> Vec<Value> {
        vec![
            json!({"hostname":"connect.example.test","path":"^/host(/.*)?$","service":"http://localhost:3017"}),
            json!({"hostname":"connect.example.test","path":HEALTH_PATH,"service":"http://localhost:3002"}),
            json!({"hostname":"connect.example.test","path":API_PATH,"service":"http://localhost:3002"}),
            json!({"service":"http_status:404"}),
        ]
    }

    #[test]
    fn remote_origin_requires_https_origin_and_refuses_public_route_loop() {
        assert_eq!(
            validate_remote_origin("https://engine.example/", "connect.example.test").unwrap(),
            "https://engine.example"
        );
        for rejected in [
            "http://engine.example",
            "https://engine.example/path",
            "https://user@engine.example",
            "https://connect.example.test",
        ] {
            assert!(validate_remote_origin(rejected, "connect.example.test").is_err());
        }
    }

    #[test]
    fn rewrite_changes_only_mobile_api_routes() {
        let mut rules = ingress();
        rewrite_connect_ingress(&mut rules, "connect.example.test", "https://engine.example")
            .unwrap();
        verify_connect_ingress(&rules, "connect.example.test", "https://engine.example").unwrap();
        assert_eq!(rules[0]["service"], "http://localhost:3017");
        assert_eq!(rules[3]["service"], "http_status:404");
    }

    #[test]
    fn rewrite_fails_closed_when_managed_rules_are_missing_or_duplicated() {
        let mut missing = ingress();
        missing.remove(1);
        assert!(rewrite_connect_ingress(
            &mut missing,
            "connect.example.test",
            "https://engine.example"
        )
        .is_err());

        let mut duplicate = ingress();
        duplicate.push(json!({"hostname":"connect.example.test","path":API_PATH,"service":"http://localhost:3002"}));
        assert!(rewrite_connect_ingress(
            &mut duplicate,
            "connect.example.test",
            "https://engine.example"
        )
        .is_err());
    }
}
