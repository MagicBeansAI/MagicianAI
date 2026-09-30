use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::time::{sleep, Duration};
use tracing::{info, warn};

use crate::config::{ensure_data_dirs, save_config, MagicianDesktopConfig};
use crate::container::{ContainerConfig, ContainerRuntime, ContainerStatus};

/// Where the Magician engine for this desktop lives.
///
/// This is deliberately more explicit than inferring placement from a URL and
/// `manage_runtime_stack`: setup needs to explain the consequence before it
/// changes either setting. The persisted runtime configuration remains the
/// compatibility contract used by the rest of the desktop app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupPlacement {
    ManagedContainer,
    NativeInstall,
    ExistingLocal,
    Remote,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupPlacementOption {
    pub id: SetupPlacement,
    pub label: String,
    pub description: String,
    pub available: bool,
    pub unavailable_reason: Option<String>,
    pub recommended: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupOptions {
    pub platform: String,
    pub current_placement: SetupPlacement,
    pub current_engine_url: String,
    pub data_root: String,
    pub placements: Vec<SetupPlacementOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupSelection {
    pub placement: SetupPlacement,
    #[serde(default)]
    pub engine_url: Option<String>,
    #[serde(default)]
    pub data_root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupStartResult {
    /// `install`, `edge_enrollment`, or `authenticate`.
    pub next_action: String,
    #[serde(default)]
    pub install_plan: Option<InstallPlan>,
    /// Whether approval will replace an existing managed container so its
    /// mounts and resource settings match the selected configuration.
    pub replace_existing_container: bool,
    pub message: String,
}

fn current_placement(config: &MagicianDesktopConfig) -> SetupPlacement {
    if config.is_remote_engine() {
        SetupPlacement::Remote
    } else if cfg!(target_os = "macos") && crate::native_runtime::installed_by_desktop() {
        SetupPlacement::NativeInstall
    } else if config.general.manage_runtime_stack {
        SetupPlacement::ManagedContainer
    } else {
        SetupPlacement::ExistingLocal
    }
}

fn placement_options(app: &AppHandle) -> Vec<SetupPlacementOption> {
    let native = crate::native_runtime::availability(app);
    let native_available = cfg!(target_os = "macos") && native.available();
    vec![
        SetupPlacementOption {
            id: SetupPlacement::NativeInstall,
            label: "Install native services here".to_string(),
            description: "Install the Magician backend, Magicutor, and the supervisor as operating-system services on this computer.".to_string(),
            available: native_available,
            unavailable_reason: if !cfg!(target_os = "macos") {
                Some(
                    "Future option on this platform. Native services are available on macOS today.".to_string()
                )
            } else {
                native.reason
            },
            recommended: native_available,
        },
        SetupPlacementOption {
            id: SetupPlacement::ManagedContainer,
            label: "Install and manage a local container".to_string(),
            description: "Run the Linux backend image and keep its data and credentials on this computer. macOS can set up its runtime; Linux and Windows require Docker to be running first.".to_string(),
            available: true,
            unavailable_reason: None,
            recommended: !native_available,
        },
        SetupPlacementOption {
            id: SetupPlacement::ExistingLocal,
            label: "Connect to a local container".to_string(),
            description: "Connect to a Linux backend container already listening on this computer. Desktop supplies host tools but does not own the container lifecycle.".to_string(),
            available: true,
            unavailable_reason: None,
            recommended: false,
        },
        SetupPlacementOption {
            id: SetupPlacement::Remote,
            label: "Connect to a remote container".to_string(),
            description: "Keep data and backend services on a server while this desktop supplies local CUA, browser, and platform capabilities through Desktop Edge.".to_string(),
            available: true,
            unavailable_reason: None,
            recommended: false,
        },
    ]
}

pub(crate) fn configured_for_selection(
    current: &MagicianDesktopConfig,
    selection: &SetupSelection,
) -> Result<MagicianDesktopConfig, String> {
    let mut config = current.clone();
    if let Some(root) = selection
        .data_root
        .as_deref()
        .map(str::trim)
        .filter(|root| !root.is_empty())
    {
        config.general.runtime_root = Some(root.to_string());
    }

    match selection.placement {
        SetupPlacement::ManagedContainer => {
            config.general.manage_runtime_stack = true;
            config.network.engine_base_url = None;
        },
        SetupPlacement::NativeInstall => {
            if !cfg!(target_os = "macos") {
                return Err(
                    "Native Magician services are a future option on this platform; choose a local or remote container"
                        .to_string(),
                );
            }
            config.general.manage_runtime_stack = false;
            config.network.engine_base_url = None;
        },
        SetupPlacement::ExistingLocal => {
            config.general.manage_runtime_stack = false;
            config.network.engine_base_url = selection
                .engine_url
                .as_deref()
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .map(ToOwned::to_owned);
            if config.is_remote_engine() {
                return Err("An existing local backend must use a loopback URL".to_string());
            }
        },
        SetupPlacement::Remote => {
            let url = selection
                .engine_url
                .as_deref()
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .ok_or("Enter the remote backend HTTPS URL")?;
            config.general.manage_runtime_stack = false;
            config.network.engine_base_url = Some(url.to_string());
            if !config.is_remote_engine() {
                return Err("A remote backend must use a non-loopback URL".to_string());
            }
        },
    }

    crate::config::normalize_config(&mut config);
    config.validate_runtime_root()?;
    config.validate_engine_base_url()?;
    Ok(config)
}

pub fn show_setup_window(app: &AppHandle) -> Result<(), String> {
    show_setup_window_mode(app, None)
}

pub fn show_setup_window_mode(app: &AppHandle, mode: Option<&str>) -> Result<(), String> {
    crate::orb_window::block_for_setup(app);
    if let Some(window) = app.get_webview_window("setup") {
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }

    let path = if mode == Some("capabilities") {
        "/setup?mode=capabilities"
    } else {
        "/setup"
    };
    tauri::WebviewWindowBuilder::new(app, "setup", crate::tray::desktop_app_webview_url(path))
        .title(format!(
            "{} Setup",
            crate::presentation_identity_generated::PRODUCT_NAME
        ))
        .inner_size(760.0, 720.0)
        .min_inner_size(620.0, 560.0)
        .resizable(true)
        .build()
        .map(|_| ())
        .map_err(|error| format!("Failed to create setup window: {error}"))
}

#[tauri::command]
pub async fn finish_setup(
    app: AppHandle,
    capabilities_completed: Option<bool>,
) -> Result<(), String> {
    if capabilities_completed.unwrap_or(false) {
        crate::onboarding::complete_onboarding(&app).await?;
        let orb = app
            .state::<crate::AppState>()
            .config
            .lock()
            .await
            .orb
            .clone();
        crate::orb_window::release_setup_gate(&app, &orb);
    }
    crate::tray::open_settings_window(&app);
    if let Some(window) = app.get_webview_window("setup") {
        window
            .close()
            .map_err(|error| format!("Failed to close setup window: {error}"))?;
    }
    Ok(())
}

#[tauri::command]
pub async fn get_setup_options(app: AppHandle) -> Result<SetupOptions, String> {
    let config = app.state::<crate::AppState>().config.lock().await.clone();
    Ok(SetupOptions {
        platform: std::env::consts::OS.to_string(),
        current_placement: current_placement(&config),
        current_engine_url: config.engine_base_url(),
        data_root: crate::runtime_paths::runtime_root_dir()
            .display()
            .to_string(),
        placements: placement_options(&app),
    })
}

#[tauri::command]
pub async fn onboarding_completion_pending(app: AppHandle) -> bool {
    let state = app.state::<crate::AppState>();
    let config = state.config.lock().await;
    crate::onboarding::completion_pending_for_engine(&config.engine_base_url())
}

#[tauri::command]
pub async fn restart_onboarding_for_current_engine(app: AppHandle) -> Result<(), String> {
    let state = app.state::<crate::AppState>();
    let config = state.config.lock().await;
    crate::onboarding::mark_incomplete_for_engine(&config.engine_base_url())
}

#[tauri::command]
pub async fn open_remote_enrollment_page(app: AppHandle) -> Result<(), String> {
    let config = app.state::<crate::AppState>().config.lock().await.clone();
    if !config.is_remote_engine() {
        return Err("Select a remote server first".to_string());
    }
    config.validate_engine_base_url()?;
    open::that(format!("{}/settings", config.engine_base_url()))
        .map_err(|error| format!("Could not open the remote server page: {error}"))
}

#[tauri::command]
pub async fn apply_setup_selection(
    app: AppHandle,
    selection: SetupSelection,
) -> Result<SetupStartResult, String> {
    let current = app.state::<crate::AppState>().config.lock().await.clone();
    let config = configured_for_selection(&current, &selection)?;

    let remote_access_enrollment = if selection.placement == SetupPlacement::Remote {
        matches!(
            wait_for_remote_health(&config.engine_base_url(), Duration::from_secs(12)).await?,
            RemoteHealth::AccessEnrollmentRequired
        )
    } else {
        false
    };
    if selection.placement == SetupPlacement::ExistingLocal {
        // Verify before changing the selected origin. A typo must not strand the
        // desktop on a server it has never reached.
        wait_for_health(&config.engine_url("/health"), Duration::from_secs(12)).await?;
    }

    match selection.placement {
        SetupPlacement::ManagedContainer => {
            let (_, runtime) = crate::container::detect::detect_runtime().await?;
            let replace_existing_container = if runtime.is_available().await {
                runtime
                    .container_info(&config.general.container_name)
                    .await
                    .map_err(|error| format!("Failed to inspect the backend container: {error}"))?
                    .status
                    != ContainerStatus::NotFound
                    && managed_container_configuration_changed(&current, &config)
            } else {
                false
            };
            {
                let state = app.state::<crate::AppState>();
                *state.runtime.lock().await = Some(Arc::clone(&runtime));
            }
            let plan =
                check_and_request_consent(&app, runtime, &config, replace_existing_container)
                    .await?;
            Ok(SetupStartResult {
                next_action: "install".to_string(),
                install_plan: Some(plan),
                replace_existing_container,
                message: "Review what Magican Desktop will install.".to_string(),
            })
        },
        SetupPlacement::NativeInstall => {
            let availability = crate::native_runtime::availability(&app);
            let source = availability.source.ok_or_else(|| {
                availability.reason.unwrap_or_else(|| {
                    "No compatible native Magician backend package is available".to_string()
                })
            })?;
            let source_for_summary = source.clone();
            let (version, replacing) = tokio::task::spawn_blocking(move || {
                crate::native_runtime::package_summary(&source_for_summary)
            })
            .await
            .map_err(|error| format!("Native package inspection task failed: {error}"))??;
            let pre_existing = crate::manifest::snapshot_pre_existing().await;
            {
                let state = app.state::<crate::AppState>();
                *state.pre_existing.lock().await = Some(pre_existing);
            }
            let mut summary = vec![format!(
                "Install native Magician backend {version} from {}",
                source.display()
            )];
            let host_prerequisites = host_prerequisite_plan(SetupPlacement::NativeInstall)?;
            for prerequisite in host_prerequisites
                .iter()
                .filter(|prerequisite| !prerequisite.installed)
            {
                summary.push(format!("Install {} on this Mac", prerequisite.label));
            }
            if replacing {
                summary.push(
                    "Replace the existing native backend only after the package verifies"
                        .to_string(),
                );
            }
            summary.extend([
                format!(
                    "Keep notes, chats, tasks, and settings in {}",
                    config
                        .general
                        .runtime_root
                        .as_deref()
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(crate::runtime_paths::runtime_root_dir)
                        .display()
                ),
                "Register the backend with this operating system so it starts without a terminal"
                    .to_string(),
                "Start the Magician backend and Magicutor, then verify their health".to_string(),
            ]);
            let plan = InstallPlan {
                needs_homebrew: host_prerequisites
                    .iter()
                    .any(|prerequisite| prerequisite.id == "homebrew" && !prerequisite.installed),
                runtime_name: crate::native_runtime::RUNTIME_NAME.to_string(),
                needs_runtime_install: true,
                host_prerequisites,
                replaces_existing_container: false,
                summary,
            };
            let _ = app.emit("setup-consent-needed", &plan);
            Ok(SetupStartResult {
                next_action: "install".to_string(),
                install_plan: Some(plan),
                replace_existing_container: false,
                message: "Review the native backend installation.".to_string(),
            })
        },
        SetupPlacement::ExistingLocal => {
            save_selected_config(&app, &config).await?;
            let _ = app.emit(
                "setup-progress",
                SetupProgress {
                    step: SetupStep::Ready,
                    progress: 1.0,
                    message: "Connected to the local Magician backend.".to_string(),
                },
            );
            Ok(SetupStartResult {
                next_action: "authenticate".to_string(),
                install_plan: None,
                replace_existing_container: false,
                message: "Connected to the local Magician backend.".to_string(),
            })
        },
        SetupPlacement::Remote => {
            save_selected_config(&app, &config).await?;
            let (next_action, message) = if remote_access_enrollment {
                (
                    "edge_enrollment",
                    "Server reached through Cloudflare Access. Enroll this desktop from the authenticated server page, then sign in.",
                )
            } else {
                (
                    "authenticate",
                    "Server verified. Sign in, then enroll Desktop Edge from Devices.",
                )
            };
            let _ = app.emit(
                "setup-progress",
                SetupProgress {
                    step: SetupStep::Ready,
                    progress: 1.0,
                    message: message.to_string(),
                },
            );
            Ok(SetupStartResult {
                next_action: next_action.to_string(),
                install_plan: None,
                replace_existing_container: false,
                message: message.to_string(),
            })
        },
    }
}

async fn save_selected_config(
    app: &AppHandle,
    config: &MagicianDesktopConfig,
) -> Result<(), String> {
    save_config(config)?;
    crate::commands::apply_launch_at_login(app, config.general.launch_at_login)?;
    *app.state::<crate::AppState>().config.lock().await = config.clone();
    crate::onboarding::mark_incomplete_for_engine(&config.engine_base_url())?;
    Ok(())
}

fn managed_container_configuration_changed(
    current: &MagicianDesktopConfig,
    next: &MagicianDesktopConfig,
) -> bool {
    current_placement(current) != SetupPlacement::ManagedContainer
        || current.general.runtime_root != next.general.runtime_root
        || current.general.container_image != next.general.container_image
        || current.general.container_name != next.general.container_name
        || current.container != next.container
        || current.network.magician_port != next.network.magician_port
        || current.network.magicutor_port != next.network.magicutor_port
        || current.host_gateway != next.host_gateway
        || current.api_keys != next.api_keys
}

/// Steps in the first-run setup flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SetupStep {
    CheckingPrerequisites,
    AwaitingConsent,
    InstallingPrerequisites,
    InstallingRuntime,
    InstallingBackend,
    PullingImage,
    CreatingDataDirs,
    StartingContainer,
    RegisteringService,
    WaitingForHealth,
    Ready,
    Failed(String),
}

/// Progress update emitted to the frontend during setup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupProgress {
    pub step: SetupStep,
    pub progress: f32,
    pub message: String,
}

/// What the setup flow needs to install, shown to user for consent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallPlan {
    /// Whether Homebrew needs to be installed
    pub needs_homebrew: bool,
    /// Name of the container runtime to install (e.g., "Apple Container CLI", "Docker + Colima")
    pub runtime_name: String,
    /// Whether the runtime needs to be installed (vs already present)
    pub needs_runtime_install: bool,
    /// Reviewed, data-declared host programs needed before the engine-backed
    /// capability catalog can take over.
    pub host_prerequisites: Vec<HostPrerequisitePlanItem>,
    /// An existing container will be recreated after approval so its host
    /// mounts and runtime options match the selected setup.
    pub replaces_existing_container: bool,
    /// Human-readable summary of what will happen
    pub summary: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostPrerequisitePlanItem {
    pub id: String,
    pub label: String,
    pub installed: bool,
}

fn host_prerequisite_profile(placement: SetupPlacement) -> Option<&'static str> {
    host_prerequisite_profile_for(std::env::consts::OS, placement)
}

fn host_prerequisite_profile_for(
    operating_system: &str,
    placement: SetupPlacement,
) -> Option<&'static str> {
    match (operating_system, placement) {
        ("macos", SetupPlacement::ManagedContainer) => Some("macos_container"),
        ("macos", SetupPlacement::NativeInstall) => Some("macos_native"),
        ("linux", SetupPlacement::ManagedContainer) => Some("linux_container"),
        (_, SetupPlacement::ExistingLocal | SetupPlacement::Remote) => None,
        _ => None,
    }
}

fn host_binary_exists(binary: &str) -> bool {
    if binary == "brew" {
        return Path::new(&crate::container::find_brew_path()).is_file();
    }
    let resolved = runtime_core::process::resolve_program(
        OsStr::new(binary),
        std::env::var_os("PATH").as_deref(),
    );
    if resolved.is_file() {
        return true;
    }
    ["/opt/homebrew/bin", "/usr/local/bin"]
        .iter()
        .map(PathBuf::from)
        .map(|directory| directory.join(binary))
        .any(|candidate| candidate.is_file())
}

fn resolved_host_prerequisites(
    placement: SetupPlacement,
) -> Result<Vec<magician_components::setup::HostPrerequisite>, String> {
    host_prerequisite_profile(placement)
        .map(magician_components::setup::host_prerequisites)
        .transpose()
        .map(Option::unwrap_or_default)
}

fn host_prerequisite_plan(
    placement: SetupPlacement,
) -> Result<Vec<HostPrerequisitePlanItem>, String> {
    resolved_host_prerequisites(placement).map(|prerequisites| {
        prerequisites
            .into_iter()
            .map(|prerequisite| HostPrerequisitePlanItem {
                installed: prerequisite
                    .bins
                    .iter()
                    .all(|binary| host_binary_exists(binary)),
                id: prerequisite.id,
                label: prerequisite.label,
            })
            .collect()
    })
}

async fn install_host_prerequisites(
    app: &AppHandle,
    placement: SetupPlacement,
) -> Result<(), String> {
    use magician_components::setup::HostPrerequisiteInstaller;

    let prerequisites = resolved_host_prerequisites(placement)?;
    let missing = prerequisites
        .iter()
        .filter(|prerequisite| {
            !prerequisite
                .bins
                .iter()
                .all(|binary| host_binary_exists(binary))
        })
        .count();
    if missing == 0 {
        return Ok(());
    }

    for (index, prerequisite) in prerequisites.iter().enumerate() {
        if prerequisite
            .bins
            .iter()
            .all(|binary| host_binary_exists(binary))
        {
            continue;
        }
        let progress = 0.06 + (index as f32 / prerequisites.len().max(1) as f32) * 0.08;
        let message = format!("Installing {}…", prerequisite.label);
        let _ = app.emit(
            "setup-progress",
            SetupProgress {
                step: SetupStep::InstallingPrerequisites,
                progress,
                message: message.clone(),
            },
        );
        match &prerequisite.install {
            HostPrerequisiteInstaller::Homebrew => {
                crate::container::install_homebrew(None).await?;
            },
            HostPrerequisiteInstaller::HomebrewFormula { formula } => {
                if !host_binary_exists("brew") {
                    return Err(format!(
                        "{} requires Homebrew, but Homebrew setup did not complete",
                        prerequisite.label
                    ));
                }
                let output = tokio::process::Command::new(crate::container::find_brew_path())
                    .args(["install", formula])
                    .output()
                    .await
                    .map_err(|error| {
                        format!("Could not install {}: {error}", prerequisite.label)
                    })?;
                if !output.status.success() {
                    return Err(format!(
                        "Could not install {}: {}",
                        prerequisite.label,
                        String::from_utf8_lossy(&output.stderr).trim()
                    ));
                }
            },
            HostPrerequisiteInstaller::Manual { install_hint } => {
                return Err(format!(
                    "{} is required. {install_hint}",
                    prerequisite.label
                ));
            },
        }
        let missing_bins = prerequisite
            .bins
            .iter()
            .filter(|binary| !host_binary_exists(binary))
            .cloned()
            .collect::<Vec<_>>();
        if !missing_bins.is_empty() {
            return Err(format!(
                "{} installation completed, but these programs are still unavailable: {}",
                prerequisite.label,
                missing_bins.join(", ")
            ));
        }
    }
    Ok(())
}

/// Phase 1 of first-run setup: detect what's needed and ask for consent.
/// Does NOT install anything — just emits what will be installed.
pub async fn check_and_request_consent(
    app: &AppHandle,
    runtime: Arc<dyn ContainerRuntime>,
    config: &MagicianDesktopConfig,
    replace_existing_container: bool,
) -> Result<InstallPlan, String> {
    let emit_progress = |step: SetupStep, progress: f32, message: &str| {
        let _ = app.emit(
            "setup-progress",
            SetupProgress {
                step,
                progress,
                message: message.to_string(),
            },
        );
    };

    emit_progress(
        SetupStep::CheckingPrerequisites,
        0.0,
        "Checking system requirements...",
    );

    // Snapshot what's already installed BEFORE we do anything
    let pre_existing = crate::manifest::snapshot_pre_existing().await;
    {
        let state = app.state::<crate::AppState>();
        let mut pre = state.pre_existing.lock().await;
        *pre = Some(pre_existing);
    }

    let needs_runtime_install = !runtime.is_available().await;
    if needs_runtime_install && std::env::consts::OS != "macos" {
        return Err(crate::container::detect::container_runtime_prerequisite_error());
    }
    let host_prerequisites = host_prerequisite_plan(SetupPlacement::ManagedContainer)?;
    let resolved_prerequisites = resolved_host_prerequisites(SetupPlacement::ManagedContainer)?;
    for prerequisite in &resolved_prerequisites {
        if prerequisite
            .bins
            .iter()
            .all(|binary| host_binary_exists(binary))
        {
            continue;
        }
        if let magician_components::setup::HostPrerequisiteInstaller::Manual { install_hint } =
            &prerequisite.install
        {
            return Err(format!(
                "{} is required. {install_hint}",
                prerequisite.label
            ));
        }
    }
    let needs_homebrew = host_prerequisites
        .iter()
        .any(|prerequisite| prerequisite.id == "homebrew" && !prerequisite.installed);
    let needs_image_pull = !runtime
        .image_exists(&config.general.container_image)
        .await
        .unwrap_or(false);

    let mut summary = Vec::new();
    if needs_homebrew {
        summary.push("Install Homebrew (macOS package manager)".to_string());
    }
    for prerequisite in host_prerequisites
        .iter()
        .filter(|prerequisite| prerequisite.id != "homebrew" && !prerequisite.installed)
    {
        summary.push(format!("Install {} on this Mac", prerequisite.label));
    }
    if needs_runtime_install {
        summary.push(format!("Set up the {} container runtime", runtime.name()));
    }
    if needs_image_pull {
        summary.push("Download the Magician backend container image".to_string());
    }
    if replace_existing_container {
        summary.push(
            "Recreate the existing backend container with the selected data folder and settings"
                .to_string(),
        );
    }
    summary.push("Create local data directories".to_string());
    summary.push(
        "Create private, persistent container credential custody outside the data folder"
            .to_string(),
    );
    summary.push("Start backend services".to_string());

    let plan = InstallPlan {
        needs_homebrew,
        runtime_name: runtime.name().to_string(),
        needs_runtime_install,
        host_prerequisites,
        replaces_existing_container: replace_existing_container,
        summary,
    };

    emit_progress(SetupStep::AwaitingConsent, 0.05, "Waiting for approval...");
    let _ = app.emit("setup-consent-needed", &plan);

    Ok(plan)
}

/// Install the reviewed native package, register the per-user operating-system
/// service, and wait for the same health contract used by every other setup
/// placement. Package checksums are verified before any installed file changes.
pub async fn execute_native_setup(
    app: &AppHandle,
    config: &MagicianDesktopConfig,
) -> Result<(), String> {
    let emit_progress = |step: SetupStep, progress: f32, message: &str| {
        let _ = app.emit(
            "setup-progress",
            SetupProgress {
                step,
                progress,
                message: message.to_string(),
            },
        );
    };

    let availability = crate::native_runtime::availability(app);
    let source = availability.source.ok_or_else(|| {
        availability.reason.unwrap_or_else(|| {
            "No compatible native Magician backend package is available".to_string()
        })
    })?;

    install_host_prerequisites(app, SetupPlacement::NativeInstall)
        .await
        .map_err(|error| {
            emit_progress(SetupStep::Failed(error.clone()), 0.0, &error);
            error
        })?;

    emit_progress(
        SetupStep::InstallingBackend,
        0.2,
        "Verifying and installing the native backend package…",
    );
    let package = crate::native_runtime::install_package(source)
        .await
        .map_err(|error| {
            emit_progress(SetupStep::Failed(error.clone()), 0.0, &error);
            error
        })?;

    emit_progress(
        SetupStep::CreatingDataDirs,
        0.55,
        "Preparing the backend data folder without replacing existing files…",
    );
    ensure_data_dirs().map_err(|error| {
        emit_progress(SetupStep::Failed(error.clone()), 0.0, &error);
        error
    })?;

    emit_progress(
        SetupStep::RegisteringService,
        0.7,
        "Registering and starting the backend service…",
    );
    crate::native_runtime::prepare_runtime_and_start(&crate::runtime_paths::runtime_root_dir())
        .await
        .map_err(|error| {
            emit_progress(SetupStep::Failed(error.clone()), 0.0, &error);
            error
        })?;

    emit_progress(
        SetupStep::WaitingForHealth,
        0.85,
        "Waiting for the native backend to become healthy…",
    );
    wait_for_health(&config.engine_url("/health"), Duration::from_secs(120))
        .await
        .map_err(|error| {
            emit_progress(SetupStep::Failed(error.clone()), 0.0, &error);
            error
        })?;

    let pre_existing = app
        .state::<crate::AppState>()
        .pre_existing
        .lock()
        .await
        .clone()
        .unwrap_or(crate::manifest::PreExistingState {
            homebrew: true,
            container_runtime: true,
            native_backend: true,
            native_service: true,
        });
    let manifest = crate::manifest::InstallManifest {
        installed_at: timestamp_iso8601(),
        platform: crate::manifest::platform_string(),
        runtime: crate::native_runtime::RUNTIME_NAME.to_string(),
        installed_by_us: crate::manifest::InstalledArtifacts {
            homebrew: false,
            container_runtime: String::new(),
            container_image: String::new(),
            container_name: String::new(),
            launch_agent: false,
            native_prefix: crate::native_runtime::install_prefix()
                .display()
                .to_string(),
            native_service: crate::native_runtime::SERVICE_ID.to_string(),
            native_package_version: package.version,
        },
        pre_existing,
        data_dirs: vec![crate::runtime_paths::runtime_root_dir()
            .display()
            .to_string()],
    };
    crate::manifest::write_manifest(&manifest)?;

    emit_progress(SetupStep::Ready, 1.0, "Magican is ready!");
    Ok(())
}

/// Phase 2 of first-run setup: user has consented, proceed with installation.
pub async fn execute_setup(
    app: &AppHandle,
    runtime: Arc<dyn ContainerRuntime>,
    config: &MagicianDesktopConfig,
    replace_existing_container: bool,
) -> Result<(), String> {
    let emit_progress = |step: SetupStep, progress: f32, message: &str| {
        let _ = app.emit(
            "setup-progress",
            SetupProgress {
                step,
                progress,
                message: message.to_string(),
            },
        );
    };

    install_host_prerequisites(app, SetupPlacement::ManagedContainer)
        .await
        .map_err(|error| {
            emit_progress(SetupStep::Failed(error.clone()), 0.0, &error);
            error
        })?;

    // macOS owns runtime setup. Linux and Windows require a working Docker
    // daemon before this flow begins, and a daemon stopped during consent is a
    // prerequisite failure rather than a surprise package installation.
    if !runtime.is_available().await {
        if std::env::consts::OS != "macos" {
            let error = crate::container::detect::container_runtime_prerequisite_error();
            emit_progress(SetupStep::Failed(error.clone()), 0.0, &error);
            return Err(error);
        }
        emit_progress(
            SetupStep::InstallingRuntime,
            0.1,
            &format!("Installing {} runtime...", runtime.name()),
        );
        info!("Setup: installing runtime '{}'", runtime.name());

        let progress_cb: Box<dyn Fn(f32, &str) + Send + Sync> = {
            let app_clone = app.clone();
            Box::new(move |p, msg| {
                let _ = app_clone.emit(
                    "setup-progress",
                    SetupProgress {
                        step: SetupStep::InstallingRuntime,
                        progress: 0.1 + p * 0.2,
                        message: msg.to_string(),
                    },
                );
            })
        };

        runtime.install(Some(&progress_cb)).await.map_err(|e| {
            emit_progress(
                SetupStep::Failed(e.clone()),
                0.0,
                &format!("Install failed: {}", e),
            );
            e
        })?;
    }

    // Pull image if not present
    let image = &config.general.container_image;
    let image_present = runtime.image_exists(image).await.unwrap_or(false);
    if !image_present {
        info!("Setup: pulling image '{}'", image);
        emit_progress(
            SetupStep::PullingImage,
            0.35,
            &format!("Pulling {}...", image),
        );

        let progress_cb: Box<dyn Fn(f32, &str) + Send + Sync> = {
            let app_clone = app.clone();
            Box::new(move |p, msg| {
                let _ = app_clone.emit(
                    "setup-progress",
                    SetupProgress {
                        step: SetupStep::PullingImage,
                        progress: 0.35 + p * 0.25,
                        message: msg.to_string(),
                    },
                );
            })
        };

        runtime
            .pull_image(image, Some(&progress_cb))
            .await
            .map_err(|e| {
                emit_progress(
                    SetupStep::Failed(e.clone()),
                    0.0,
                    &format!("Pull failed: {}", e),
                );
                e
            })?;
    } else {
        info!("Setup: image '{}' already present", image);
    }

    // Create data directories
    emit_progress(
        SetupStep::CreatingDataDirs,
        0.6,
        "Creating data directories...",
    );
    ensure_data_dirs().map_err(|e| {
        emit_progress(
            SetupStep::Failed(e.clone()),
            0.0,
            &format!("Failed to create directories: {}", e),
        );
        e
    })?;

    if crate::engine_roots::should_supervise_local_engine(config) {
        // Start container
        emit_progress(SetupStep::StartingContainer, 0.7, "Starting container...");
        info!("Setup: starting container");

        let container_config = ContainerConfig::from_desktop_config(config);
        let status = runtime
            .container_info(&config.general.container_name)
            .await
            .map_err(|error| format!("Failed to inspect the backend container: {error}"))?
            .status;

        if replace_existing_container && status != ContainerStatus::NotFound {
            // Validate credential custody, actual mounts and candidate-image
            // access while the old service is still intact.
            runtime
                .prepare_keyring(&container_config)
                .await
                .map_err(|error| {
                    emit_progress(SetupStep::Failed(error.clone()), 0.0, &error);
                    error
                })?;
            info!("Setup: replacing existing container so configuration changes take effect");
            if status == ContainerStatus::Running || status == ContainerStatus::Restarting {
                runtime
                    .stop(&config.general.container_name)
                    .await
                    .map_err(|error| {
                        emit_progress(SetupStep::Failed(error.clone()), 0.0, &error);
                        error
                    })?;
            }
            runtime
                .remove(&config.general.container_name)
                .await
                .map_err(|error| {
                    emit_progress(SetupStep::Failed(error.clone()), 0.0, &error);
                    error
                })?;
            sleep(Duration::from_millis(500)).await;
        }

        let status = if replace_existing_container {
            ContainerStatus::NotFound
        } else {
            status
        };
        match status {
            ContainerStatus::Running | ContainerStatus::Restarting => {
                info!("Setup: container already active; continuing to health check");
            },
            ContainerStatus::Stopped => {
                runtime
                    .start_existing(&config.general.container_name)
                    .await
                    .map_err(|error| {
                        emit_progress(SetupStep::Failed(error.clone()), 0.0, &error);
                        error
                    })?;
            },
            ContainerStatus::NotFound => {
                let port_check = crate::port_check::check_ports(
                    &container_config.ports,
                    &config.general.container_name,
                )
                .await;
                if !port_check.all_clear() {
                    let occupied = port_check
                        .conflicts
                        .iter()
                        .map(|conflict| conflict.port.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let error = format!(
                        "Local backend port(s) {occupied} are already in use. Stop the other local Magician backend or choose it as an existing local backend, then retry."
                    );
                    emit_progress(SetupStep::Failed(error.clone()), 0.0, &error);
                    return Err(error);
                }
                runtime.start(&container_config).await.map_err(|error| {
                    emit_progress(
                        SetupStep::Failed(error.clone()),
                        0.0,
                        &format!("Failed to start container: {error}"),
                    );
                    error
                })?;
            },
        }
    } else {
        info!(
            "Setup: remote engine at {}; not starting a local container",
            config.engine_base_url()
        );
    }

    // Wait for health
    emit_progress(
        SetupStep::WaitingForHealth,
        0.85,
        "Waiting for the backend to become healthy...",
    );
    info!("Setup: waiting for health endpoint");

    wait_for_health(&config.engine_url("/health"), Duration::from_secs(120))
        .await
        .map_err(|e| {
            emit_progress(
                SetupStep::Failed(e.clone()),
                0.0,
                &format!("Health check timed out: {}", e),
            );
            e
        })?;

    // Write install manifest
    {
        let state = app.state::<crate::AppState>();
        let pre_existing: crate::manifest::PreExistingState = state
            .pre_existing
            .lock()
            .await
            .clone()
            .unwrap_or(crate::manifest::PreExistingState {
                homebrew: true, // conservative default: assume pre-existing
                container_runtime: true,
                native_backend: true,
                native_service: true,
            });

        let manifest = crate::manifest::InstallManifest {
            installed_at: timestamp_iso8601(),
            platform: crate::manifest::platform_string(),
            runtime: runtime.name().to_string(),
            installed_by_us: crate::manifest::InstalledArtifacts {
                homebrew: !pre_existing.homebrew && crate::container::is_homebrew_available().await,
                container_runtime: if pre_existing.container_runtime {
                    "none".to_string()
                } else {
                    match runtime.name() {
                        "Apple Container" => "apple-container".to_string(),
                        "Docker" => "colima+docker".to_string(),
                        other => other.to_lowercase(),
                    }
                },
                container_image: config.general.container_image.clone(),
                container_name: config.general.container_name.clone(),
                launch_agent: config.general.launch_at_login,
                native_prefix: String::new(),
                native_service: String::new(),
                native_package_version: String::new(),
            },
            pre_existing,
            data_dirs: vec![crate::runtime_paths::runtime_root_dir()
                .display()
                .to_string()],
        };

        if let Err(e) = crate::manifest::write_manifest(&manifest) {
            warn!("Failed to write install manifest: {}", e);
        }
    }

    // Done!
    emit_progress(SetupStep::Ready, 1.0, "Magican is ready!");
    info!("Setup: complete, backend is healthy");
    Ok(())
}

/// Wait for the health endpoint to return success, with a timeout.
pub async fn wait_for_health(url: &str, timeout: Duration) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        if tokio::time::Instant::now() > deadline {
            return Err(format!(
                "Health check timed out after {}s",
                timeout.as_secs()
            ));
        }

        match client.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                info!("Health endpoint responding at {}", url);
                return Ok(());
            },
            Ok(resp) => {
                warn!(
                    "Health endpoint returned {} at {}, retrying...",
                    resp.status(),
                    url
                );
            },
            Err(_) => {
                // Not yet available
            },
        }

        sleep(Duration::from_secs(2)).await;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteHealth {
    Healthy,
    AccessEnrollmentRequired,
}

fn cloudflare_access_redirect(response: &reqwest::Response) -> bool {
    cloudflare_access_location(
        response.status().as_u16(),
        response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok()),
    )
}

fn cloudflare_access_location(status: u16, location: Option<&str>) -> bool {
    if !(300..400).contains(&status) {
        return false;
    }
    location
        .and_then(|value| reqwest::Url::parse(value).ok())
        .is_some_and(|url| {
            url.scheme() == "https"
                && url.host_str().is_some_and(|host| {
                    host.eq_ignore_ascii_case("cloudflareaccess.com")
                        || host.to_ascii_lowercase().ends_with(".cloudflareaccess.com")
                })
        })
}

async fn wait_for_remote_health(origin: &str, timeout: Duration) -> Result<RemoteHealth, String> {
    let url = reqwest::Url::parse(&format!("{}/health", origin.trim_end_matches('/')))
        .map_err(|_| "The remote backend health URL is invalid")?;
    crate::edge_client::hydrate_outer_access(origin).await?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("Failed to create remote health client: {error}"))?;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last_status = None;
    loop {
        if tokio::time::Instant::now() > deadline {
            return Err(match last_status {
                Some(status) => format!("Remote health check returned HTTP {status}"),
                None => format!("Remote health check timed out after {}s", timeout.as_secs()),
            });
        }
        let request = crate::edge_client::authorize_outer_http(client.get(url.clone()), &url);
        match request.send().await {
            Ok(response) if response.status().is_success() => return Ok(RemoteHealth::Healthy),
            Ok(response) if cloudflare_access_redirect(&response) => {
                return Ok(RemoteHealth::AccessEnrollmentRequired)
            },
            Ok(response) => last_status = Some(response.status().as_u16()),
            Err(_) => {},
        }
        sleep(Duration::from_secs(2)).await;
    }
}

/// Generate an ISO 8601 UTC timestamp without external dependencies.
fn timestamp_iso8601() -> String {
    let output = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => {
            let epoch = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            format!("{}", epoch)
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection(placement: SetupPlacement, engine_url: Option<&str>) -> SetupSelection {
        SetupSelection {
            placement,
            engine_url: engine_url.map(str::to_string),
            data_root: Some(
                std::env::temp_dir()
                    .join("MagicianNotes")
                    .display()
                    .to_string(),
            ),
        }
    }

    #[test]
    fn remote_selection_requires_https_and_disables_local_lifecycle() {
        let current = MagicianDesktopConfig::default();
        let configured = configured_for_selection(
            &current,
            &selection(SetupPlacement::Remote, Some("https://engine.example")),
        )
        .unwrap();
        assert_eq!(
            configured.network.engine_base_url.as_deref(),
            Some("https://engine.example")
        );
        assert!(!configured.general.manage_runtime_stack);
        assert!(configured.is_remote_engine());

        assert!(configured_for_selection(
            &current,
            &selection(SetupPlacement::Remote, Some("http://engine.example")),
        )
        .is_err());
    }

    #[test]
    fn existing_local_selection_refuses_a_remote_origin() {
        let current = MagicianDesktopConfig::default();
        assert!(configured_for_selection(
            &current,
            &selection(
                SetupPlacement::ExistingLocal,
                Some("https://engine.example")
            ),
        )
        .is_err());

        let configured = configured_for_selection(
            &current,
            &selection(
                SetupPlacement::ExistingLocal,
                Some("http://127.0.0.1:13002"),
            ),
        )
        .unwrap();
        assert!(!configured.general.manage_runtime_stack);
        assert!(!configured.is_remote_engine());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_install_selects_loopback_without_container_custody() {
        let mut current = MagicianDesktopConfig::default();
        current.network.engine_base_url = Some("https://engine.example".to_string());
        let configured =
            configured_for_selection(&current, &selection(SetupPlacement::NativeInstall, None))
                .unwrap();
        assert_eq!(configured.network.engine_base_url, None);
        assert!(!configured.general.manage_runtime_stack);
        assert!(!configured.is_remote_engine());
    }

    #[test]
    fn managed_container_clears_a_previous_remote_origin() {
        let mut current = MagicianDesktopConfig::default();
        current.network.engine_base_url = Some("https://engine.example".to_string());
        current.general.manage_runtime_stack = false;
        let configured =
            configured_for_selection(&current, &selection(SetupPlacement::ManagedContainer, None))
                .unwrap();
        assert_eq!(configured.network.engine_base_url, None);
        assert!(configured.general.manage_runtime_stack);
    }

    #[test]
    fn host_prerequisite_profiles_follow_platform_and_placement() {
        assert_eq!(
            host_prerequisite_profile_for("macos", SetupPlacement::ManagedContainer),
            Some("macos_container")
        );
        assert_eq!(
            host_prerequisite_profile_for("macos", SetupPlacement::NativeInstall),
            Some("macos_native")
        );
        assert_eq!(
            host_prerequisite_profile_for("linux", SetupPlacement::ManagedContainer),
            Some("linux_container")
        );
        assert_eq!(
            host_prerequisite_profile_for("linux", SetupPlacement::ExistingLocal),
            None
        );
        assert_eq!(
            host_prerequisite_profile_for("windows", SetupPlacement::ManagedContainer),
            None
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn native_install_is_rejected_until_supported_on_this_platform() {
        let current = MagicianDesktopConfig::default();
        let error =
            configured_for_selection(&current, &selection(SetupPlacement::NativeInstall, None))
                .unwrap_err();
        assert!(error.contains("future option"));
    }

    #[test]
    fn cloudflare_access_redirect_requires_the_exact_https_access_domain() {
        assert!(cloudflare_access_location(
            302,
            Some("https://team.cloudflareaccess.com/cdn-cgi/access/login/example")
        ));
        assert!(!cloudflare_access_location(
            302,
            Some("https://cloudflareaccess.com.attacker.example/login")
        ));
        assert!(!cloudflare_access_location(
            302,
            Some("http://team.cloudflareaccess.com/login")
        ));
        assert!(!cloudflare_access_location(
            200,
            Some("https://team.cloudflareaccess.com/login")
        ));
    }
}
