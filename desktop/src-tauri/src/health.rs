use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::time::{interval, Duration};
use tracing::{debug, info};

use crate::config::MagicianDesktopConfig;
use crate::container::ContainerStatus;
use crate::tray::TrayState;
use crate::AppState;

/// Health status of an individual service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ServiceHealth {
    Healthy,
    Unhealthy(String),
    Unreachable,
}

/// Aggregated health of all services inside the container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregatedHealth {
    pub magician: ServiceHealth,
    pub magicutor: ServiceHealth,
    /// Whether the container itself is running.
    pub container_running: bool,
}

impl AggregatedHealth {
    /// All services healthy and container running.
    pub fn all_healthy(&self) -> bool {
        self.container_running
            && self.magician == ServiceHealth::Healthy
            && self.magicutor == ServiceHealth::Healthy
    }

    /// At least one service healthy, but not all.
    pub fn partially_healthy(&self) -> bool {
        self.container_running
            && (self.magician == ServiceHealth::Healthy || self.magicutor == ServiceHealth::Healthy)
            && !self.all_healthy()
    }

    /// Status summary for the tray menu tooltip.
    pub fn status_detail(&self) -> String {
        if !self.container_running {
            return "Container stopped".to_string();
        }
        let m = status_symbol(&self.magician);
        let x = status_symbol(&self.magicutor);
        format!("Magician backend {} \u{00B7} Magicutor {}", m, x)
    }
}

fn status_symbol(s: &ServiceHealth) -> &'static str {
    match s {
        ServiceHealth::Healthy => "\u{1F7E2}",      // 🟢
        ServiceHealth::Unhealthy(_) => "\u{1F7E1}", // 🟡
        ServiceHealth::Unreachable => "\u{1F534}",  // 🔴
    }
}

/// Response from the /health endpoint.
#[derive(Debug, Deserialize)]
struct HealthResponse {
    #[serde(default)]
    status: String,
}

/// Poll both service health endpoints and update the tray icon state.
///
/// Runs as a background task for the lifetime of the application.
pub async fn health_monitor(app: AppHandle) {
    let mut ticker = interval(Duration::from_secs(5));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .expect("Failed to create HTTP client");

    info!("Health monitor started (polling every 5s)");

    let mut prev_tray_state: Option<TrayState> = None;
    let mut prev_detail = String::new();
    let mut prev_update_version: Option<String> = None;

    loop {
        ticker.tick().await;

        let state = app.state::<AppState>();
        let (magician_url, magicutor_url, container_name, remote_engine) = {
            let config = state.config.lock().await;
            (
                config.engine_url("/health"),
                format!("http://127.0.0.1:{}/health", config.network.magicutor_port),
                config.general.container_name.clone(),
                config.is_remote_engine(),
            )
        };

        // Poll both services concurrently
        let magician_url = magician_url.as_str();
        let magicutor_url = magicutor_url.as_str();

        let (magician_health, magicutor_health) = tokio::join!(
            check_service_health(&client, &magician_url),
            check_service_health(&client, &magicutor_url),
        );

        // If both unreachable, check if container is actually running.
        // A remote engine has no local container to consult.
        let container_running = if remote_engine {
            magician_health != ServiceHealth::Unreachable
        } else if magician_health == ServiceHealth::Unreachable
            && magicutor_health == ServiceHealth::Unreachable
        {
            let runtime = state.runtime.lock().await;
            if let Some(ref rt) = *runtime {
                rt.container_info(&container_name)
                    .await
                    .map(|info| info.status == ContainerStatus::Running)
                    .unwrap_or(false)
            } else {
                false
            }
        } else {
            // At least one endpoint responded — container is running
            true
        };

        let health = AggregatedHealth {
            magician: magician_health,
            magicutor: magicutor_health,
            container_running,
        };

        // Map to tray state
        let tray_state = if health.all_healthy() {
            TrayState::Running
        } else if health.partially_healthy() {
            TrayState::Starting // partial = yellow
        } else if health.container_running {
            TrayState::Starting // container up but services not ready
        } else {
            TrayState::Stopped
        };

        // Read pending app update version so the tray menu preserves the indicator
        let update_version = {
            let state = app.state::<AppState>();
            let pending = state.pending_app_update.lock().await;
            pending.clone()
        };

        let detail = health.status_detail();
        let state_changed = prev_tray_state.as_ref() != Some(&tray_state)
            || prev_detail != detail
            || prev_update_version != update_version;

        if state_changed {
            crate::tray::update_tray_state_with_detail(
                &app,
                tray_state.clone(),
                &detail,
                update_version.as_deref(),
            );
            prev_tray_state = Some(tray_state);
            prev_detail = detail;
            prev_update_version = update_version;
        }

        // Emit detailed health for any open windows
        let _ = app.emit("health-status", &health);

        debug!("Health: {:?}", health);
    }
}

/// Check the externally managed localhost services without consulting a
/// container runtime. Used by Settings in local `make run-all` mode, where the
/// tray is a host gateway and the supervisor owns service lifecycle.
pub async fn check_local_services(config: &MagicianDesktopConfig) -> AggregatedHealth {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("Failed to create HTTP client");
    let magician_url = config.engine_url("/health");
    let magicutor_url = format!("http://127.0.0.1:{}/health", config.network.magicutor_port);
    let (magician, magicutor) = tokio::join!(
        check_service_health(&client, &magician_url),
        check_service_health(&client, &magicutor_url),
    );
    let container_running =
        magician != ServiceHealth::Unreachable || magicutor != ServiceHealth::Unreachable;
    AggregatedHealth {
        magician,
        magicutor,
        container_running,
    }
}

/// Check a single service health endpoint.
async fn check_service_health(client: &reqwest::Client, url: &str) -> ServiceHealth {
    match client.get(url).send().await {
        Ok(response) => {
            if response.status().is_success() {
                match response.json::<HealthResponse>().await {
                    Ok(body) => {
                        if body.status == "ok" || body.status == "healthy" || body.status.is_empty()
                        {
                            ServiceHealth::Healthy
                        } else {
                            ServiceHealth::Unhealthy(format!("Status: {}", body.status))
                        }
                    },
                    // 200 but bad JSON = still alive
                    Err(_) => ServiceHealth::Healthy,
                }
            } else {
                ServiceHealth::Unhealthy(format!("HTTP {}", response.status()))
            }
        },
        Err(e) => {
            if e.is_connect() || e.is_timeout() {
                ServiceHealth::Unreachable
            } else {
                ServiceHealth::Unhealthy(format!("Request error: {}", e))
            }
        },
    }
}
