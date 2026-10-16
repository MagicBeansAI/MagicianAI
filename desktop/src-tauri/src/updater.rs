use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;
use tokio::time::{interval, sleep, Duration};
use tracing::{info, warn};

use crate::config::MagicianDesktopConfig;
use crate::container::{ContainerConfig, ContainerRuntime, ContainerStatus};
use crate::setup::wait_for_health;
use crate::tray::TrayState;
use crate::AppState;

/// State of an update check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateState {
    pub container_update: Option<ContainerUpdate>,
    pub has_app_update: bool,
    pub app_update_version: Option<String>,
    pub last_checked: Option<String>,
}

/// Information about an available container image update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerUpdate {
    pub current_digest: String,
    pub remote_digest: String,
    pub image: String,
}

/// Check for available container image and app updates.
pub async fn check_for_updates(
    app: &AppHandle,
    runtime: &dyn ContainerRuntime,
    config: &MagicianDesktopConfig,
) -> Result<UpdateState, String> {
    let image = &config.general.container_image;

    let local_digest = runtime.image_digest(image).await.unwrap_or_default();
    let remote_digest = runtime.remote_image_digest(image).await.unwrap_or_default();

    let container_update =
        if !local_digest.is_empty() && !remote_digest.is_empty() && local_digest != remote_digest {
            info!(
                "Container update available: local={}, remote={}",
                local_digest, remote_digest
            );
            Some(ContainerUpdate {
                current_digest: local_digest,
                remote_digest,
                image: image.clone(),
            })
        } else {
            None
        };

    let now = chrono_now_iso();

    // Check for app update using Tauri updater plugin
    let (has_app_update, app_update_version) = match app.updater() {
        Ok(updater) => match updater.check().await {
            Ok(Some(update)) => (true, Some(update.version.clone())),
            Ok(None) => (false, None),
            Err(e) => {
                warn!("App update check failed: {}", e);
                (false, None)
            },
        },
        Err(_) => (false, None), // Updater not configured (no pubkey yet)
    };

    Ok(UpdateState {
        container_update,
        has_app_update,
        app_update_version,
        last_checked: Some(now),
    })
}

/// The tag used to store the previous image for rollback.
const ROLLBACK_TAG: &str = "magician:previous";

/// Perform a container image update: tag current for rollback, pull new image,
/// stop, remove, restart, verify health, and roll back if health check fails.
pub async fn perform_container_update(
    app: &AppHandle,
    runtime: Arc<dyn ContainerRuntime>,
    config: &MagicianDesktopConfig,
) -> Result<(), String> {
    if !crate::engine_roots::should_supervise_local_engine(config) {
        return Err(
            "Engine is remote; this desktop does not update a local engine container.".into(),
        );
    }
    let container_name = &config.general.container_name;
    let image = &config.general.container_image;

    // Update tray to "Updating" state
    crate::tray::update_tray_state(app, TrayState::Updating);

    // 0. Tag current image as rollback target before updating
    let _ = app.emit("update-progress", "Saving current image for rollback...");
    info!(
        "Update: tagging current image '{}' as '{}'",
        image, ROLLBACK_TAG
    );
    if let Err(e) = runtime.tag_image(image, ROLLBACK_TAG).await {
        warn!("Could not tag rollback image (continuing anyway): {}", e);
    }

    // 1. Pull latest image
    let _ = app.emit("update-progress", "Pulling latest image...");
    info!("Update: pulling latest image '{}'", image);
    runtime.pull_image(image, None).await?;

    // Credential loss or unmanaged mounts must leave the old service running.
    let container_config = ContainerConfig::from_desktop_config(config);
    runtime.prepare_keyring(&container_config).await?;

    // 2. Stop running container
    let _ = app.emit("update-progress", "Stopping current container...");
    info!("Update: stopping container '{}'", container_name);
    let info = runtime.container_info(container_name).await?;
    if info.status == ContainerStatus::Running {
        runtime.stop(container_name).await?;
    }

    // 3. Remove old container
    let _ = app.emit("update-progress", "Removing old container...");
    info!("Update: removing container '{}'", container_name);
    if info.status != ContainerStatus::NotFound {
        runtime.remove(container_name).await?;
    }

    // 4. Start new container
    let _ = app.emit("update-progress", "Starting updated container...");
    info!("Update: starting new container");
    runtime.start(&container_config).await?;

    // 5. Wait for health — if it fails, attempt rollback
    let _ = app.emit("update-progress", "Waiting for health check...");
    match wait_for_health(&config.engine_url("/health"), Duration::from_secs(120)).await {
        Ok(()) => {
            let _ = app.emit("update-progress", "Update complete!");
            info!("Update: complete, container is healthy");
            Ok(())
        },
        Err(health_err) => {
            warn!(
                "Update: health check failed after update, attempting rollback: {}",
                health_err
            );
            rollback(app, &runtime, container_name, config).await
        },
    }
}

/// Roll back to the previous container image after a failed update.
///
/// Steps:
/// 1. Stop the new (unhealthy) container
/// 2. Remove it
/// 3. Start with the `magician:previous` image
/// 4. Log the failure
/// 5. Emit `update-rollback` event to frontend
async fn rollback(
    app: &AppHandle,
    runtime: &Arc<dyn ContainerRuntime>,
    container_name: &str,
    config: &MagicianDesktopConfig,
) -> Result<(), String> {
    let _ = app.emit("update-progress", "Rolling back to previous version...");

    // 1. Stop the new container
    info!("Rollback: stopping failed container '{}'", container_name);
    if let Err(e) = runtime.stop(container_name).await {
        warn!("Rollback: failed to stop container: {}", e);
    }

    // 2. Remove it
    info!("Rollback: removing failed container '{}'", container_name);
    if let Err(e) = runtime.remove(container_name).await {
        warn!("Rollback: failed to remove container: {}", e);
    }

    // 3. Start with the rollback image
    info!("Rollback: starting container with image '{}'", ROLLBACK_TAG);
    let mut rollback_config = ContainerConfig::from_desktop_config(config);
    rollback_config.image = ROLLBACK_TAG.to_string();

    if let Err(e) = runtime.start(&rollback_config).await {
        let msg = format!(
            "Rollback failed: could not start previous image '{}': {}",
            ROLLBACK_TAG, e
        );
        warn!("{}", msg);
        crate::tray::update_tray_state(app, TrayState::Stopped);
        let _ = app.emit("update-rollback", &msg);
        return Err(msg);
    }

    // 4 & 5. Log + emit event
    let msg = "Update failed health check. Rolled back to previous version.".to_string();
    warn!("Rollback: {}", msg);
    crate::tray::update_tray_state(app, TrayState::Running);
    let _ = app.emit("update-rollback", &msg);

    Err(format!(
        "Container update failed health check; rolled back to previous image"
    ))
}

/// Perform the Tauri app self-update: download, install, and restart.
pub async fn perform_app_update(app: &AppHandle) -> Result<(), String> {
    let _ = app.emit("update-progress", "Checking for app update...");

    let updater = app
        .updater()
        .map_err(|e| format!("Updater not configured: {}", e))?;
    let update = updater
        .check()
        .await
        .map_err(|e| format!("Update check failed: {}", e))?;

    let update = update.ok_or_else(|| "No app update available".to_string())?;

    info!("Downloading app update v{}", update.version);
    let _ = app.emit(
        "update-progress",
        format!("Downloading v{}...", update.version),
    );

    update
        .download_and_install(
            |chunk_length, content_length| {
                tracing::trace!("Downloaded {} / {:?}", chunk_length, content_length);
            },
            || {
                info!("App update download complete");
            },
        )
        .await
        .map_err(|e| format!("Failed to install update: {}", e))?;

    info!("App update installed, restarting...");
    let _ = app.emit("update-progress", "Update installed, restarting...");
    app.restart();
}

/// Perform a combined update: container first, then app.
///
/// 1. Updates the container image (pull, stop, remove, start, health check)
/// 2. Triggers the Tauri app update (download + install + relaunch)
///
/// Note: If an app update is available, `perform_app_update` calls `app.restart()`
/// and this function will not return after step 2.
pub async fn perform_combined_update(
    app: &AppHandle,
    runtime: Arc<dyn ContainerRuntime>,
    config: &MagicianDesktopConfig,
) -> Result<(), String> {
    // Step 1: Update container
    let _ = app.emit("update-progress", "Updating container...");
    info!("Combined update: starting container update");
    perform_container_update(app, Arc::clone(&runtime), config).await?;

    // Step 2: Trigger app update.
    // Note: perform_app_update calls app.restart() on success, so this
    // function will not return past this point if an app update is installed.
    perform_app_update(app).await?;

    Ok(())
}

/// Background loop that checks for updates on launch and every 6 hours.
pub async fn update_check_loop(app: AppHandle) {
    // Wait a bit after startup before first check
    sleep(Duration::from_secs(30)).await;

    let mut ticker = interval(Duration::from_secs(6 * 60 * 60)); // 6 hours

    loop {
        ticker.tick().await;

        let state = app.state::<AppState>();
        let config = state.config.lock().await.clone();

        if !config.updates.auto_check {
            continue;
        }

        let rt = {
            let runtime = state.runtime.lock().await;
            match &*runtime {
                Some(rt) => Arc::clone(rt),
                None => continue,
            }
        };

        match check_for_updates(&app, rt.as_ref(), &config).await {
            Ok(update_state) => {
                let has_update =
                    update_state.container_update.is_some() || update_state.has_app_update;

                if has_update {
                    info!("Update available, notifying frontend");
                    let _ = app.emit("update-available", &update_state);
                }

                // Persist update version in shared state so health monitor preserves it
                if let Some(ref version) = update_state.app_update_version {
                    let mut pending = state.pending_app_update.lock().await;
                    *pending = Some(version.clone());
                    drop(pending);
                    crate::tray::show_update_available(&app, version);
                }

                // Auto-update container if configured
                if update_state.container_update.is_some() && config.updates.auto_update_container {
                    info!("Auto-update enabled, performing container update");
                    if let Err(e) = perform_container_update(&app, Arc::clone(&rt), &config).await {
                        warn!("Auto-update failed: {}", e);
                    }
                }
            },
            Err(e) => {
                warn!("Update check failed: {}", e);
            },
        }
    }
}

/// Simple ISO 8601 timestamp without pulling in chrono.
fn chrono_now_iso() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}Z", now.as_secs())
}
