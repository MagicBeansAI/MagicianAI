#[cfg(debug_assertions)]
use std::path::{Component, PathBuf};

#[cfg(debug_assertions)]
use tauri::http::{header, Response, StatusCode};
use tauri::{
    image::Image,
    menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager,
};

/// Tauri event emitted when the notifications toggle flips. The notify-overlay
/// route listens for this to gate its window visibility in lock-step with the
/// tray toggle (show only when enabled AND there are cards).
const NOTIFICATIONS_ENABLED_CHANGED_EVENT: &str = "notifications-enabled-changed";
use tracing::{info, warn};

/// Visual state of the platform tray surface: the macOS menu bar, Windows
/// notification area, or Linux desktop panel/status notifier.
#[derive(Debug, Clone, PartialEq)]
pub enum TrayState {
    /// Container is running and healthy (green indicator).
    Running,
    /// Container is starting or health check pending (yellow indicator).
    Starting,
    /// Container is stopped or unreachable (red indicator).
    Stopped,
    /// Container image is being updated (blue indicator).
    Updating,
}

/// Build the platform tray icon and its shared control menu.
pub fn create_tray(app: &AppHandle) -> Result<(), String> {
    let status_label = current_status_label(app);
    let menu = build_tray_menu(app, &status_label, None)?;

    let mut tray = TrayIconBuilder::with_id("main-tray")
        .tooltip("Magican")
        .menu(&menu)
        // macOS users expect a click on a status item to open its menu. Linux
        // status notifiers own their click policy and ignore this setting.
        // Windows uses the native convention below: left click opens Magican,
        // while right click opens this menu.
        .show_menu_on_left_click(!cfg!(target_os = "windows"))
        .on_menu_event(move |app, event| {
            handle_menu_event(app, event.id().as_ref());
        });

    #[cfg(target_os = "macos")]
    {
        tray = tray.icon(menu_bar_template_icon()).icon_as_template(true);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let icon = app
            .default_window_icon()
            .cloned()
            .ok_or_else(|| "Magican application icon is unavailable".to_string())?;
        tray = tray.icon(icon);
    }
    #[cfg(target_os = "windows")]
    {
        use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
        tray = tray.on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                open_app(tray.app_handle());
            }
        });
    }

    let tray = tray
        .build(app)
        .map_err(|e| format!("Failed to build {}: {}", tray_surface_name(), e))?;

    // Store the tray-icon handle so `set_pending_approval_count` (+ the toggle
    // handler) can paint the pending count next to the icon via `set_title`.
    if let Ok(mut current) = app.state::<crate::AppState>().tray_icon.lock() {
        *current = Some(tray);
    }

    info!(
        surface = tray_surface_name(),
        "Desktop tray surface created"
    );
    Ok(())
}

fn tray_surface_name() -> &'static str {
    tray_surface_name_for(std::env::consts::OS)
}

fn tray_surface_name_for(os: &str) -> &'static str {
    match os {
        "macos" => "menu-bar status item",
        "windows" => "notification-area icon",
        "linux" => "desktop-panel tray icon",
        _ => "system tray icon",
    }
}

fn menu_bar_template_icon() -> Image<'static> {
    const SIZE: u32 = 18;
    // Outfit Bold Magican `M` template mask from generate-magican-app-icons.py.
    const MAGICAN_M_ALPHA: &[u8] = include_bytes!("../icons/tray-mask-18.bin");
    let mut rgba = Vec::with_capacity(MAGICAN_M_ALPHA.len() * 4);
    for &alpha in MAGICAN_M_ALPHA {
        rgba.extend_from_slice(&[0, 0, 0, alpha]);
    }
    Image::new_owned(rgba, SIZE, SIZE)
}

fn orb_listening_toggle_label(state: &str) -> &'static str {
    if matches!(state, "off" | "ended" | "disarming") {
        "Wake Orb"
    } else {
        "Let Orb Rest"
    }
}

/// Build the tray menu with a status label and optional update version.
fn build_tray_menu(
    app: &AppHandle,
    status_label: &str,
    update_version: Option<&str>,
) -> Result<tauri::menu::Menu<tauri::Wry>, String> {
    let status_item = MenuItemBuilder::with_id("status", status_label)
        .enabled(false)
        .build(app)
        .map_err(|e| format!("Failed to build status menu item: {}", e))?;

    let runtime_item = MenuItemBuilder::with_id("runtime_mode", &current_runtime_label(app))
        .enabled(false)
        .build(app)
        .map_err(|e| format!("Failed to build runtime menu item: {}", e))?;

    let separator1 = PredefinedMenuItem::separator(app)
        .map_err(|e| format!("Failed to build separator: {}", e))?;

    // Title shows the double-tap gesture (the primary trigger); the optional
    // chord, if one is set, is shown as the menu accelerator.
    let overlay_shortcut = crate::overlay::current_overlay_shortcut(app);
    let quick_automate_title = match crate::overlay::current_overlay_gesture_label(app) {
        Some(label) => format!("Quick Automate... ({label})"),
        None => "Quick Automate...".to_string(),
    };
    let mut quick_automate_builder =
        MenuItemBuilder::with_id("quick_automate", &quick_automate_title);
    if !overlay_shortcut.trim().is_empty() {
        quick_automate_builder = quick_automate_builder.accelerator(overlay_shortcut);
    }
    let quick_automate = quick_automate_builder
        .build(app)
        .map_err(|e| format!("Failed to build menu item: {}", e))?;
    let orb_snapshot = crate::orb_window::current_snapshot(app);
    let orb_status =
        MenuItemBuilder::with_id("orb_status", &format!("Status: {}", orb_snapshot.status))
            .enabled(false)
            .build(app)
            .map_err(|e| format!("Failed to build orb status item: {e}"))?;
    let orb_hotkey = app
        .state::<crate::AppState>()
        .config
        .try_lock()
        .map(|config| config.orb.hotkey.clone())
        .unwrap_or_default();
    let mut orb_toggle_builder = MenuItemBuilder::with_id(
        "orb_toggle",
        if orb_snapshot.window_open {
            "Show / Hide Orb"
        } else {
            "Show Orb"
        },
    );
    if !crate::config::normalize_optional_shortcut(&orb_hotkey).is_empty() {
        orb_toggle_builder = orb_toggle_builder.accelerator(orb_hotkey);
    }
    let orb_toggle = orb_toggle_builder
        .build(app)
        .map_err(|e| format!("Failed to build orb toggle item: {e}"))?;
    let orb_talk = MenuItemBuilder::with_id("orb_talk", "Talk Now")
        .enabled(matches!(orb_snapshot.state, "armed" | "cooldown"))
        .build(app)
        .map_err(|e| format!("Failed to build orb talk item: {e}"))?;
    let orb_pause = MenuItemBuilder::with_id(
        "orb_pause",
        if orb_snapshot.state == "paused" {
            "Resume Listening"
        } else {
            "Pause for 1 Hour"
        },
    )
    .enabled(matches!(
        orb_snapshot.state,
        "armed"
            | "heard"
            | "connecting"
            | "listening"
            | "thinking"
            | "speaking"
            | "cooldown"
            | "hold_ready"
            | "recoverable_error"
            | "paused"
    ))
    .build(app)
    .map_err(|e| format!("Failed to build orb pause item: {e}"))?;
    let orb_arm =
        MenuItemBuilder::with_id("orb_arm", orb_listening_toggle_label(orb_snapshot.state))
            .build(app)
            .map_err(|e| format!("Failed to build orb arm item: {e}"))?;
    let orb_menu = SubmenuBuilder::with_id(app, "ambient_orb", "Ambient Orb")
        .items(&[&orb_status, &orb_toggle, &orb_talk, &orb_pause, &orb_arm])
        .build()
        .map_err(|e| format!("Failed to build orb submenu: {e}"))?;

    let open_app = MenuItemBuilder::with_id("open_app", "Open App")
        .accelerator("CmdOrCtrl+D")
        .build(app)
        .map_err(|e| format!("Failed to build menu item: {}", e))?;

    // Stateful Show/Hide-notifications toggle. Label is computed from the
    // current (enabled, pending-count) so a rebuilt menu reflects live state;
    // toggling/count-changes later `set_text` it in place (see
    // `update_notifications_menu_item_text`).
    let notifications_toggle_text =
        notifications_toggle_label(notifications_enabled(app), pending_approval_count(app));
    let toggle_notifications =
        MenuItemBuilder::with_id("toggle_notifications", &notifications_toggle_text)
            .build(app)
            .map_err(|e| format!("Failed to build menu item: {}", e))?;

    // Screen capture-and-ask — each item invokes exactly what its global
    // chord does, and the accelerator label makes the chords discoverable
    // (they otherwise live only in the config file).
    let (sc_ask, sc_region, sc_clip, sc_watch) = crate::screen_ask::current_screen_chords(app);
    let build_screen_item = |id: &str, label: &str, chord: &str| {
        let mut builder = MenuItemBuilder::with_id(id, label);
        if !chord.trim().is_empty() {
            builder = builder.accelerator(chord);
        }
        builder
            .build(app)
            .map_err(|e| format!("Failed to build menu item: {}", e))
    };
    let screen_ask_item = build_screen_item("screen_ask", "Screenshot + Ask", &sc_ask)?;
    let screen_region_item = build_screen_item("screen_region", "Pick Region + Ask", &sc_region)?;
    let screen_clip_item =
        build_screen_item("screen_clip", "Record Clip (press again to stop)", &sc_clip)?;
    let screen_watch_item = build_screen_item(
        "screen_watch",
        "Watch Screen (press again to stop)",
        &sc_watch,
    )?;
    let screen_menu = SubmenuBuilder::with_id(app, "screen", "Screen")
        .items(&[
            &screen_ask_item,
            &screen_region_item,
            &screen_clip_item,
            &screen_watch_item,
        ])
        .build()
        .map_err(|e| format!("Failed to build screen submenu: {}", e))?;

    let separator2 = PredefinedMenuItem::separator(app)
        .map_err(|e| format!("Failed to build separator: {}", e))?;

    let runtime_stack_managed = tray_runtime_stack_managed(app);
    let service_controls_enabled = runtime_stack_managed || tray_uses_local_supervisor(app);
    let (start_label, stop_label, restart_label) = if runtime_stack_managed {
        ("Start", "Stop", "Restart")
    } else {
        ("Start Supervisor", "Stop Supervisor", "Restart Supervisor")
    };

    let start_item = MenuItemBuilder::with_id("start", start_label)
        .enabled(service_controls_enabled)
        .build(app)
        .map_err(|e| format!("Failed to build menu item: {}", e))?;

    let stop_item = MenuItemBuilder::with_id("stop", stop_label)
        .enabled(service_controls_enabled)
        .build(app)
        .map_err(|e| format!("Failed to build menu item: {}", e))?;

    let restart_item = MenuItemBuilder::with_id("restart", restart_label)
        .enabled(service_controls_enabled)
        .build(app)
        .map_err(|e| format!("Failed to build menu item: {}", e))?;

    // Per-service control via the Makefile / supervisor-ctl. Useful when
    // bouncing just magician or magicutor without taking the whole stack
    // down. Entries shell out to `make <target>` with the CWD derived
    // by walking up from the tray binary's location until a `Makefile`
    // is found; see `services_dispatch` + `locate_makefile_root`.
    let svc_restart_magician =
        MenuItemBuilder::with_id("svc_restart_magician", "Restart Magician backend")
            .enabled(service_controls_enabled)
            .build(app)
            .map_err(|e| format!("Failed to build menu item: {}", e))?;
    let svc_stop_magician = MenuItemBuilder::with_id("svc_stop_magician", "Stop Magician backend")
        .enabled(service_controls_enabled)
        .build(app)
        .map_err(|e| format!("Failed to build menu item: {}", e))?;
    let svc_restart_magicutor =
        MenuItemBuilder::with_id("svc_restart_magicutor", "Restart Magicutor")
            .enabled(service_controls_enabled)
            .build(app)
            .map_err(|e| format!("Failed to build menu item: {}", e))?;
    let svc_stop_magicutor = MenuItemBuilder::with_id("svc_stop_magicutor", "Stop Magicutor")
        .enabled(service_controls_enabled)
        .build(app)
        .map_err(|e| format!("Failed to build menu item: {}", e))?;
    // Restart doubles as Start when the service is stopped — the
    // supervisor's `restart` is `stop + start` where `stop` is a no-op
    // on a missing process; `restart-supervisor` and `restart-ui-dev`
    // compose `stop-X + run-X` with no-op-safe stop halves. So
    // explicit Start items would be duplicate verbs.
    let svc_stop_supervisor = MenuItemBuilder::with_id("svc_stop_supervisor", "Stop Supervisor")
        .enabled(service_controls_enabled)
        .build(app)
        .map_err(|e| format!("Failed to build menu item: {}", e))?;
    let svc_restart_supervisor =
        MenuItemBuilder::with_id("svc_restart_supervisor", "Restart Supervisor")
            .enabled(service_controls_enabled)
            .build(app)
            .map_err(|e| format!("Failed to build menu item: {}", e))?;
    let svc_stop_ui = MenuItemBuilder::with_id("svc_stop_ui", "Stop UI Dev Server")
        .enabled(!runtime_stack_managed && service_controls_enabled)
        .build(app)
        .map_err(|e| format!("Failed to build menu item: {}", e))?;
    let svc_restart_ui = MenuItemBuilder::with_id("svc_restart_ui", "Restart UI Dev Server")
        .enabled(!runtime_stack_managed && service_controls_enabled)
        .build(app)
        .map_err(|e| format!("Failed to build menu item: {}", e))?;

    let services_menu = SubmenuBuilder::new(app, "Services")
        .items(&[
            &svc_restart_magician,
            &svc_stop_magician,
            &PredefinedMenuItem::separator(app)
                .map_err(|e| format!("Failed to build separator: {}", e))?,
            &svc_restart_magicutor,
            &svc_stop_magicutor,
            &PredefinedMenuItem::separator(app)
                .map_err(|e| format!("Failed to build separator: {}", e))?,
            &svc_restart_supervisor,
            &svc_stop_supervisor,
            &PredefinedMenuItem::separator(app)
                .map_err(|e| format!("Failed to build separator: {}", e))?,
            &svc_restart_ui,
            &svc_stop_ui,
        ])
        .build()
        .map_err(|e| format!("Failed to build services submenu: {}", e))?;

    let separator3 = PredefinedMenuItem::separator(app)
        .map_err(|e| format!("Failed to build separator: {}", e))?;

    let settings_item = MenuItemBuilder::with_id("settings", "Settings...")
        .accelerator("CmdOrCtrl+,")
        .build(app)
        .map_err(|e| format!("Failed to build menu item: {}", e))?;

    let logs_item = MenuItemBuilder::with_id("view_logs", "View Logs...")
        .build(app)
        .map_err(|e| format!("Failed to build menu item: {}", e))?;

    let separator4 = PredefinedMenuItem::separator(app)
        .map_err(|e| format!("Failed to build separator: {}", e))?;

    let check_updates_text = match update_version {
        Some(version) => format!("Update Available (v{}) \u{2014} Install", version),
        None => "Check for Updates...".to_string(),
    };
    let check_updates = MenuItemBuilder::with_id("check_updates", &check_updates_text)
        .build(app)
        .map_err(|e| format!("Failed to build menu item: {}", e))?;

    let install_extension =
        MenuItemBuilder::with_id("install_extension", "Install Browser Extension")
            .build(app)
            .map_err(|e| format!("Failed to build menu item: {}", e))?;

    let separator5 = PredefinedMenuItem::separator(app)
        .map_err(|e| format!("Failed to build separator: {}", e))?;

    let quit_item = MenuItemBuilder::with_id("quit", "Quit Magican")
        .accelerator("CmdOrCtrl+Q")
        .build(app)
        .map_err(|e| format!("Failed to build menu item: {}", e))?;

    let menu = MenuBuilder::new(app)
        .items(&[
            &status_item,
            &runtime_item,
            &separator1,
            &quick_automate,
            &orb_menu,
            &screen_menu,
            &open_app,
            &toggle_notifications,
            &separator2,
            &start_item,
            &stop_item,
            &restart_item,
            &services_menu,
            &separator3,
            &settings_item,
            &logs_item,
            &separator4,
            &check_updates,
            &install_extension,
            &separator5,
            &quit_item,
        ])
        .build()
        .map_err(|e| format!("Failed to build tray menu: {}", e))?;
    set_status_menu_item(app, Some(status_item.clone()));
    set_runtime_menu_item(app, Some(runtime_item.clone()));
    set_notifications_menu_item(app, Some(toggle_notifications.clone()));
    Ok(menu)
}

/// Walk up from the tray binary's location looking for the nearest
/// `Makefile`. That's the magician repo root where `make restart-magician`
/// etc. live. Falls back to the process CWD if no Makefile is found
/// upward (e.g., binary moved to `/Applications/`); the dispatched
/// command will then fail explicitly with a "no such target" error
/// that surfaces in the tray log, rather than silently running in the
/// wrong directory.
fn locate_makefile_root() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut dir: &std::path::Path = exe.parent()?;
    loop {
        if dir.join("Makefile").is_file() {
            return Some(dir.to_path_buf());
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return None,
        }
    }
}

/// Dispatch a Services-submenu action through the active runtime mode.
///
/// Local supervisor mode shells out to the repo Makefile (`make
/// restart-magician`, etc.). Container-managed mode sends service commands to
/// the supervisor running inside the container via `magic-supervisor client`;
/// supervisor-level stop/restart maps to stopping/restarting the whole
/// container because the supervisor is the container entrypoint.
fn services_dispatch(app: &AppHandle, make_target: &str) {
    if tray_runtime_stack_managed(app) {
        match make_target {
            "restart-magician" => {
                container_supervisor_dispatch(app, make_target, "restart-magician")
            },
            "stop-magician" => container_supervisor_dispatch(app, make_target, "stop-magician"),
            "restart-magicutor" => {
                container_supervisor_dispatch(app, make_target, "restart-magicutor")
            },
            "stop-magicutor" => container_supervisor_dispatch(app, make_target, "stop-magicutor"),
            "restart-supervisor" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = crate::commands::restart_container_inner(&app).await {
                        warn!("Failed to restart container supervisor: {}", e);
                    }
                });
            },
            "stop-supervisor" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = crate::commands::stop_container_inner(&app).await {
                        warn!("Failed to stop container supervisor: {}", e);
                    }
                });
            },
            _ => {
                tracing::warn!(
                    target = make_target,
                    "services_dispatch: service action is only available in local supervisor mode"
                );
            },
        }
        return;
    }

    if cfg!(target_os = "macos") && crate::native_runtime::installed_by_desktop() {
        let action = make_target.to_string();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = crate::native_runtime::dispatch(&action).await {
                tracing::warn!(
                    target = %action,
                    "Failed to dispatch native backend service action: {error}"
                );
            }
        });
        return;
    }

    if tray_uses_local_supervisor(app) {
        services_dispatch_local_make(make_target);
    } else {
        warn!("External engine selected; refusing to control the native supervisor");
    }
}

/// Dispatch a Services-submenu action by shelling out to the magician repo's
/// Makefile. Runs detached in a background tokio task because some targets
/// (`run-supervisor`) block; we don't want the tray menu click to hang. Output
/// is logged via `tracing`.
///
/// Working directory: the nearest `Makefile` walked up from the tray binary's
/// location. Falls back to the process CWD when no Makefile is found upward
/// (e.g., binary installed to `/Applications/`).
fn services_dispatch_local_make(make_target: &str) {
    let target = make_target.to_string();
    tauri::async_runtime::spawn(async move {
        let repo_root = locate_makefile_root().unwrap_or_else(|| {
            tracing::warn!(
                "services_dispatch: no Makefile found walking up from current_exe; \
                 using process CWD as fallback",
            );
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"))
        });
        tracing::info!(
            target = %target,
            repo_root = %repo_root.display(),
            "services_dispatch: invoking make",
        );
        // Detached: we don't await output so long-running targets
        // (`run-supervisor`) can stay alive after the spawn completes.
        // Stdout/stderr inherit the tray's, so logs land in the same
        // log file the operator already tails.
        let result = tokio::process::Command::new("make")
            .arg(&target)
            .current_dir(&repo_root)
            .spawn();
        match result {
            Ok(mut child) => {
                let target_for_wait = target.clone();
                tokio::spawn(async move {
                    match child.wait().await {
                        Ok(status) if status.success() => {
                            tracing::info!(
                                target = %target_for_wait,
                                "services_dispatch: make target completed cleanly",
                            );
                        },
                        Ok(status) => {
                            tracing::warn!(
                                target = %target_for_wait,
                                exit_code = ?status.code(),
                                "services_dispatch: make target exited non-zero",
                            );
                        },
                        Err(e) => {
                            tracing::warn!(
                                target = %target_for_wait,
                                error = %e,
                                "services_dispatch: make wait failed",
                            );
                        },
                    }
                });
            },
            Err(e) => {
                tracing::error!(
                    target = %target,
                    error = %e,
                    "services_dispatch: failed to spawn make — is it on PATH?",
                );
            },
        }
    });
}

fn container_supervisor_dispatch(app: &AppHandle, target: &str, supervisor_command: &str) {
    let app = app.clone();
    let target = target.to_string();
    let supervisor_command = supervisor_command.to_string();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = run_container_supervisor_command(&app, &target, &supervisor_command).await {
            warn!(
                target = %target,
                command = %supervisor_command,
                "Failed to dispatch container supervisor command: {}",
                e
            );
        }
    });
}

async fn run_container_supervisor_command(
    app: &AppHandle,
    target: &str,
    supervisor_command: &str,
) -> Result<(), String> {
    let state = app.state::<crate::AppState>();
    let config = state.config.lock().await.clone();
    let runtime = state.runtime.lock().await;
    let Some(rt) = runtime.as_ref().cloned() else {
        return Err("No runtime available".to_string());
    };
    drop(runtime);

    let container_name = config.general.container_name.clone();
    crate::container::run_supervisor_command(rt.as_ref(), &container_name, supervisor_command)
        .await?;
    tracing::info!(target = %target, command = %supervisor_command, container = %container_name,
        "services_dispatch: container supervisor command completed");
    Ok(())
}

fn tray_runtime_stack_managed(app: &AppHandle) -> bool {
    let requested_from_env = std::env::var("MAGICIAN_DESKTOP_MANAGE_RUNTIME")
        .ok()
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        });
    app.state::<crate::AppState>()
        .config
        .try_lock()
        .map(|config| {
            config.should_manage_runtime_stack(
                requested_from_env.unwrap_or(config.general.manage_runtime_stack),
            )
        })
        .unwrap_or(false)
}

fn current_status_label(app: &AppHandle) -> String {
    app.state::<crate::AppState>()
        .tray_status_label
        .lock()
        .map(|label| label.clone())
        .unwrap_or_else(|_| format!("Magican v{}", env!("CARGO_PKG_VERSION")))
}

fn current_runtime_label(app: &AppHandle) -> String {
    if !tray_runtime_stack_managed(app) {
        return app
            .state::<crate::AppState>()
            .config
            .try_lock()
            .map(|config| unmanaged_runtime_label(&config))
            .unwrap_or_else(|_| "Runtime: External engine".to_string());
    }

    let runtime_name = app
        .state::<crate::AppState>()
        .runtime
        .try_lock()
        .ok()
        .and_then(|runtime| runtime.as_ref().map(|runtime| runtime.name().to_string()));

    match runtime_name {
        Some(name) if !name.trim().is_empty() => {
            let container = app
                .state::<crate::AppState>()
                .config
                .try_lock()
                .map(|config| config.general.container_name.clone())
                .unwrap_or_default();
            format!("Runtime: Container ({container}; {name})")
        },
        _ => "Runtime: Container managed".to_string(),
    }
}

fn uses_local_supervisor(config: &crate::config::MagicianDesktopConfig) -> bool {
    config.engine_base_url() == "http://127.0.0.1:3002" && config.network.magicutor_port == 3003
}

fn tray_uses_local_supervisor(app: &AppHandle) -> bool {
    app.state::<crate::AppState>()
        .config
        .try_lock()
        .map(|config| uses_local_supervisor(&config))
        .unwrap_or(false)
}

fn unmanaged_runtime_label(config: &crate::config::MagicianDesktopConfig) -> String {
    if cfg!(target_os = "macos")
        && crate::native_runtime::installed_by_desktop()
        && !config.is_remote_engine()
    {
        "Runtime: Native services".to_string()
    } else if uses_local_supervisor(config) {
        "Runtime: Local supervisor".to_string()
    } else {
        format!("Runtime: External engine ({})", config.engine_base_url())
    }
}

/// Read the number of approvals awaiting the user from
/// `AppState::pending_approval_count`.
fn pending_approval_count(app: &AppHandle) -> u32 {
    app.state::<crate::AppState>()
        .pending_approval_count
        .lock()
        .map(|count| *count)
        .unwrap_or(0)
}

/// Read the current notifications-enabled flag (default ON).
fn notifications_enabled(app: &AppHandle) -> bool {
    app.state::<crate::AppState>()
        .notifications_enabled
        .lock()
        .map(|enabled| *enabled)
        .unwrap_or(true)
}

/// Compose the label for the Show/Hide-notifications toggle from the current
/// `(enabled, count)`:
///   - enabled  → "Hide Notifications" (the next click hides the overlay)
///   - disabled → "Show Notifications" (the next click re-shows it)
/// plus a "  ·  {count}" suffix whenever approvals are pending — so the count is
/// visible in the menu regardless of the mute state.
fn notifications_toggle_label(enabled: bool, count: u32) -> String {
    let base = if enabled {
        "Hide Notifications"
    } else {
        "Show Notifications"
    };
    if count > 0 {
        format!("{base}  \u{00B7}  {count}")
    } else {
        base.to_string()
    }
}

/// Push the freshly-composed toggle label onto the stored notifications menu
/// item without rebuilding the native menu (rebuilding an open status-item menu
/// dismisses it under the cursor).
fn update_notifications_menu_item_text(app: &AppHandle) {
    let item = app
        .state::<crate::AppState>()
        .tray_notifications_item
        .lock()
        .ok()
        .and_then(|current| current.clone());
    if let Some(item) = item {
        let label =
            notifications_toggle_label(notifications_enabled(app), pending_approval_count(app));
        if let Err(error) = item.set_text(&label) {
            warn!("Failed to update notifications toggle menu item: {}", error);
        }
    }
}

/// Paint the pending-approval count next to the menu-bar tray icon via
/// `TrayIcon::set_title` (the macOS API for text adjacent to a status item).
/// `count > 0` shows the number; `count == 0` clears it. The count is the
/// ambient indicator and is shown REGARDLESS of the muted state.
fn update_tray_icon_count(app: &AppHandle, count: u32) {
    let tray = app
        .state::<crate::AppState>()
        .tray_icon
        .lock()
        .ok()
        .and_then(|current| current.clone());
    if let Some(tray) = tray {
        // macOS NSStatusItem quirk: `set_title(None)` does NOT reliably clear a
        // previously-painted title — the last number stays on the icon even after
        // the count drops to 0 (the reported "stuck 1", left by the brief
        // backfill-replay blip of 1→0 on overlay mount). Clear with an EMPTY
        // STRING, which reliably removes the text.
        let title = if count > 0 {
            count.to_string()
        } else {
            String::new()
        };
        if let Err(error) = tray.set_title(Some(title)) {
            warn!("Failed to set tray icon count title: {}", error);
        }
    }
}

/// Re-apply the notifications toggle label from the current state. Used at
/// startup once the persisted toggle flag has been seeded into `AppState`.
pub fn refresh_notifications_menu_item(app: &AppHandle) {
    update_notifications_menu_item_text(app);
}

fn set_status_label(app: &AppHandle, status_label: &str) {
    if let Ok(mut current) = app.state::<crate::AppState>().tray_status_label.lock() {
        *current = status_label.to_string();
    }
}

fn set_status_menu_item(app: &AppHandle, item: Option<tauri::menu::MenuItem<tauri::Wry>>) {
    if let Ok(mut current) = app.state::<crate::AppState>().tray_status_item.lock() {
        *current = item;
    }
}

fn set_runtime_menu_item(app: &AppHandle, item: Option<tauri::menu::MenuItem<tauri::Wry>>) {
    if let Ok(mut current) = app.state::<crate::AppState>().tray_runtime_item.lock() {
        *current = item;
    }
}

fn set_notifications_menu_item(app: &AppHandle, item: Option<tauri::menu::MenuItem<tauri::Wry>>) {
    if let Ok(mut current) = app
        .state::<crate::AppState>()
        .tray_notifications_item
        .lock()
    {
        *current = item;
    }
}

fn update_status_menu_item_text(app: &AppHandle, status_label: &str) {
    let item = app
        .state::<crate::AppState>()
        .tray_status_item
        .lock()
        .ok()
        .and_then(|current| current.clone());
    if let Some(item) = item {
        if let Err(error) = item.set_text(status_label) {
            warn!("Failed to update tray status menu item: {}", error);
        }
    }
}

fn update_runtime_menu_item_text(app: &AppHandle) {
    let item = app
        .state::<crate::AppState>()
        .tray_runtime_item
        .lock()
        .ok()
        .and_then(|current| current.clone());
    if let Some(item) = item {
        let runtime_label = current_runtime_label(app);
        if let Err(error) = item.set_text(&runtime_label) {
            warn!("Failed to update tray runtime menu item: {}", error);
        }
    }
}

/// Set the count of approvals awaiting the user. The count is the ambient
/// indicator: when `count > 0` it is painted next to the menu-bar icon via
/// `TrayIcon::set_title` and appended to the Show/Hide-notifications toggle
/// label; when `count == 0` the title is cleared and the suffix dropped. The
/// count is shown REGARDLESS of the muted state, so the tray accurately
/// reflects the backlog even while notifications are hidden.
///
/// The count is stored in `AppState::pending_approval_count` (read back by the
/// toggle handler + the menu rebuild). Updates use `set_text`/`set_title` in
/// place rather than rebuilding the native menu (a rebuild dismisses an open
/// status-item menu under the cursor).
#[tauri::command]
pub async fn set_pending_approval_count(app: AppHandle, count: u32) -> Result<(), String> {
    if let Ok(mut current) = app.state::<crate::AppState>().pending_approval_count.lock() {
        *current = count;
    }
    // (1) Toggle menu item label gains/loses the "  ·  {count}" suffix.
    update_notifications_menu_item_text(&app);
    // (2) The count rides on the tray icon itself (the "most important" surface).
    update_tray_icon_count(&app, count);
    Ok(())
}

/// Return the current notifications-enabled flag (default ON). The notify-overlay
/// route seeds its local mirror from this on mount, then tracks live flips via
/// the `notifications-enabled-changed` event.
#[tauri::command]
pub async fn get_notifications_enabled(app: AppHandle) -> Result<bool, String> {
    Ok(notifications_enabled(&app))
}

/// Open a unified-UI `path` (e.g. `/t/<thread>`, `/tasks`, `/home`). Invoked by
/// the notification overlay when the user clicks an informational card. The
/// shared router sends general destinations to the browser and reserves the
/// bounded native window for Attention/approval paths. Completion is reported
/// only after the browser launch or native parse/navigate/create/focus sequence
/// succeeds, so the overlay can retain a card when delivery fails.
#[tauri::command]
pub async fn open_app_at(app: AppHandle, path: String) -> Result<(), String> {
    open_app_at_path_result(&app, &path).await
}

pub fn refresh_menu(app: &AppHandle, update_version: Option<&str>) {
    let Some(tray) = app.tray_by_id("main-tray") else {
        return;
    };

    let status_label = current_status_label(app);
    match build_tray_menu(app, &status_label, update_version) {
        Ok(menu) => {
            if let Err(error) = tray.set_menu(Some(menu)) {
                warn!("Failed to refresh tray menu: {}", error);
            }
        },
        Err(error) => {
            warn!("Failed to rebuild tray menu: {}", error);
        },
    }
}

/// Rebuild the tray menu to show an update-available indicator.
pub fn show_update_available(app: &AppHandle, version: &str) {
    refresh_menu(app, Some(version));
}

/// Handle a menu item click.
fn handle_menu_event(app: &AppHandle, event_id: &str) {
    match event_id {
        "quick_automate" => {
            if let Err(e) = crate::overlay::show_overlay_window(app) {
                warn!("Failed to show overlay window: {}", e);
            }
        },
        // Screen capture-and-ask — same entry points as the global chords.
        "screen_ask" => crate::screen_ask::trigger_screen_ask(app),
        "screen_region" => crate::screen_ask::trigger_screen_region(app),
        "screen_clip" => crate::screen_ask::trigger_screen_clip(app),
        "screen_watch" => crate::screen_ask::trigger_screen_watch(app),
        "orb_toggle" => crate::orb_window::toggle(app),
        "orb_talk" => {
            crate::orb_window::dispatch(
                app,
                crate::orb_state::OrbAction::WakeHeard {
                    phrase: "Ready".to_string(),
                },
            );
        },
        "orb_pause" => {
            let snapshot = crate::orb_window::current_snapshot(app);
            let app = app.clone();
            if snapshot.state == "paused" {
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = crate::orb_window::orb_resume(app).await {
                        warn!("Failed to resume orb: {error}");
                    }
                });
            } else {
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = crate::orb_window::orb_pause_one_hour(app).await {
                        warn!("Failed to pause orb: {error}");
                    }
                });
            }
        },
        "orb_arm" => {
            let snapshot = crate::orb_window::current_snapshot(app);
            if matches!(snapshot.state, "off" | "ended" | "disarming") {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = crate::orb_window::orb_rearm(app.clone()).await {
                        warn!("Failed to re-arm orb: {error}");
                    }
                });
            } else {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = crate::orb_window::orb_disarm(app.clone()).await {
                        warn!("Failed to disarm orb: {error}");
                    }
                    refresh_menu(&app, None);
                });
            }
        },
        "open_app" | "open_dashboard" => {
            open_app(app);
        },
        "toggle_notifications" => {
            toggle_notifications(app);
        },

        "start" => {
            let app = app.clone();
            if tray_runtime_stack_managed(&app) {
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = crate::commands::start_container_inner(&app).await {
                        warn!("Failed to start container: {}", e);
                    }
                });
            } else {
                services_dispatch(&app, "restart-supervisor");
            }
        },
        "stop" => {
            let app = app.clone();
            if tray_runtime_stack_managed(&app) {
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = crate::commands::stop_container_inner(&app).await {
                        warn!("Failed to stop container: {}", e);
                    }
                });
            } else {
                services_dispatch(&app, "stop-supervisor");
            }
        },
        "restart" => {
            let app = app.clone();
            if tray_runtime_stack_managed(&app) {
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = crate::commands::restart_container_inner(&app).await {
                        warn!("Failed to restart container: {}", e);
                    }
                });
            } else {
                services_dispatch(&app, "restart-supervisor");
            }
        },
        "svc_restart_magician" => services_dispatch(app, "restart-magician"),
        "svc_stop_magician" => services_dispatch(app, "stop-magician"),
        "svc_restart_magicutor" => services_dispatch(app, "restart-magicutor"),
        "svc_stop_magicutor" => services_dispatch(app, "stop-magicutor"),
        "svc_stop_supervisor" => services_dispatch(app, "stop-supervisor"),
        "svc_restart_supervisor" => services_dispatch(app, "restart-supervisor"),
        "svc_stop_ui" => services_dispatch(app, "stop-ui-dev"),
        "svc_restart_ui" => services_dispatch(app, "restart-ui-dev"),
        "settings" => {
            open_settings_window(app);
        },
        "view_logs" => {
            open_logs_window(app);
        },
        "install_extension" => {
            if let Err(e) = crate::setup::show_setup_window_mode(app, Some("capabilities")) {
                warn!("Failed to open browser extension setup: {}", e);
            }
        },
        "check_updates" => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = crate::commands::check_for_updates_inner(&app).await {
                    warn!("Update check failed: {}", e);
                }
            });
        },
        "quit" => {
            info!("Quit requested from tray menu");
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                app.exit(0);
            });
        },
        _ => {
            warn!("Unknown menu event: {}", event_id);
        },
    }
}

/// Flip the notifications-enabled flag and propagate everywhere it matters:
///   - update the toggle menu item label (Show↔Hide, keeping the count suffix);
///   - emit `notifications-enabled-changed` so the notify-overlay route shows/
///     hides its window in lock-step (the count keeps flowing to the tray either
///     way — only the overlay window visibility is gated);
///   - persist the new value into `config.general.notifications_enabled` so the
///     choice survives a relaunch.
///
/// The flag is flipped synchronously (the tray handler runs off the async
/// runtime), then the config persist is spawned onto the async runtime so the
/// menu click never blocks on the config lock or disk I/O.
fn toggle_notifications(app: &AppHandle) {
    let new_value = {
        let state = app.state::<crate::AppState>();
        let Ok(mut enabled) = state.notifications_enabled.lock() else {
            warn!("Failed to lock notifications_enabled; toggle ignored");
            return;
        };
        *enabled = !*enabled;
        *enabled
    };

    // Reflect the flip in the menu label immediately (in place; no rebuild).
    update_notifications_menu_item_text(app);

    // Let the overlay route react (show/hide the window in lock-step).
    if let Err(error) = app.emit(NOTIFICATIONS_ENABLED_CHANGED_EVENT, new_value) {
        warn!("Failed to emit notifications-enabled-changed: {}", error);
    }

    // Persist into the desktop config (async; never blocks the menu click).
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<crate::AppState>();
        let config_to_save = {
            let mut config = state.config.lock().await;
            config.general.notifications_enabled = new_value;
            config.clone()
        };
        if let Err(error) = crate::config::save_config(&config_to_save) {
            warn!("Failed to persist notifications_enabled: {}", error);
        }
    });
}

pub fn app_url_from_config(config: &crate::config::MagicianDesktopConfig) -> String {
    app_url_at_path(config, "/today")
}

/// Build a **unified-ui** URL for a specific UI `path` (e.g. `/attention`,
/// `/tasks`, `/t/<thread>`) from the configured `host_gateway.ui_url`, with any
/// default-route suffix stripped. Browser destinations and the native Attention
/// window share this origin; desktop Settings/Setup use the separate
/// `magician-desktop://` frontend, which has none of these routes.
pub(crate) fn app_url_at_path(config: &crate::config::MagicianDesktopConfig, path: &str) -> String {
    let base = config.host_gateway.ui_url.trim();
    let base = if base.is_empty() {
        "http://127.0.0.1:5173"
    } else {
        base
    };
    let base = base.trim_end_matches('/');
    // Strip a default-route suffix the config might carry so we don't end up with
    // e.g. `.../today/attention`.
    let base = base
        .strip_suffix("/today")
        .or_else(|| base.strip_suffix("/home"))
        .or_else(|| base.strip_suffix("/desk"))
        .unwrap_or(base);
    let path = path.trim();
    let path = if path.is_empty() { "/" } else { path };
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

/// Label for the bounded, singleton Tauri window that handles canonical
/// Attention routes and item deep links. General routes open in the browser.
pub const ATTENTION_WINDOW_LABEL: &str = "magician-attention";
const ATTENTION_WINDOW_TITLE: &str = "Magican — Needs You";
const ATTENTION_WINDOW_INNER_SIZE: (f64, f64) = (820.0, 760.0);
const ATTENTION_WINDOW_MIN_SIZE: (f64, f64) = (620.0, 560.0);
const ATTENTION_WINDOW_MAX_SIZE: (f64, f64) = (1100.0, 900.0);
const SETTINGS_WINDOW_LABEL: &str = "settings";
const SETTINGS_WINDOW_INNER_SIZE: (f64, f64) = (720.0, 640.0);
const SETTINGS_WINDOW_MIN_SIZE: (f64, f64) = (640.0, 540.0);
const LOGS_WINDOW_LABEL: &str = "logs";
#[cfg(debug_assertions)]
const DESKTOP_APP_SCHEME: &str = "magician-desktop";
#[cfg(not(debug_assertions))]
const DESKTOP_APP_SCHEME: &str = "tauri";

pub fn register_desktop_app_protocol<R: tauri::Runtime>(
    builder: tauri::Builder<R>,
) -> tauri::Builder<R> {
    #[cfg(debug_assertions)]
    {
        builder.register_uri_scheme_protocol(DESKTOP_APP_SCHEME, |_ctx, request| {
            desktop_app_asset_response(request)
        })
    }
    #[cfg(not(debug_assertions))]
    {
        builder
    }
}

pub fn open_app(app: &AppHandle) {
    let app_clone = app.clone();
    tauri::async_runtime::spawn(async move {
        let url_string = {
            let state = app_clone.state::<crate::AppState>();
            let config = state.config.lock().await;
            app_url_from_config(&config)
        };
        if let Err(error) = open_ui_url_in_browser(&url_string) {
            warn!("Failed to open Magican at '{}': {}", url_string, error);
        }
    });
}

fn open_ui_url_in_browser(url: &str) -> Result<(), String> {
    open::that(url).map_err(|error| format!("failed to open '{url}' in the browser: {error}"))
}

pub(crate) fn desktop_app_url(path: &str) -> tauri::Url {
    let path = path.trim();
    let path = if path.is_empty() { "/" } else { path };
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    format!("{DESKTOP_APP_SCHEME}://localhost{path}")
        .parse()
        .expect("valid tauri desktop app URL")
}

pub(crate) fn desktop_app_webview_url(path: &str) -> tauri::WebviewUrl {
    tauri::WebviewUrl::CustomProtocol(desktop_app_url(path))
}

#[cfg(debug_assertions)]
fn desktop_app_asset_response(request: tauri::http::Request<Vec<u8>>) -> Response<Vec<u8>> {
    match read_desktop_app_asset(request.uri().path()) {
        Ok((bytes, content_type)) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type)
            .body(bytes)
            .expect("valid desktop app asset response"),
        Err(status) => Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(format!("desktop app asset not found: {}", request.uri().path()).into_bytes())
            .expect("valid desktop app asset error response"),
    }
}

#[cfg(debug_assertions)]
fn read_desktop_app_asset(uri_path: &str) -> Result<(Vec<u8>, &'static str), StatusCode> {
    let dist_dir = desktop_dist_dir();
    let asset_path = desktop_asset_path(&dist_dir, uri_path).ok_or(StatusCode::BAD_REQUEST)?;
    let path = if asset_path.exists() {
        asset_path
    } else {
        dist_dir.join("index.html")
    };
    let bytes = std::fs::read(&path).map_err(|_| StatusCode::NOT_FOUND)?;
    Ok((bytes, desktop_asset_content_type(&path)))
}

#[cfg(debug_assertions)]
fn desktop_dist_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../dist")
}

#[cfg(debug_assertions)]
fn desktop_asset_path(dist_dir: &std::path::Path, uri_path: &str) -> Option<PathBuf> {
    let relative = uri_path
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .trim_start_matches('/');
    let relative = if relative.is_empty() {
        "index.html"
    } else {
        relative
    };
    let mut path = dist_dir.to_path_buf();
    for component in std::path::Path::new(relative).components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {},
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(path)
}

#[cfg(debug_assertions)]
fn desktop_asset_content_type(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("css") => "text/css; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("ico") => "image/x-icon",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("wasm") => "application/wasm",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    }
}

fn open_attention_window_at_url(app: &AppHandle, url_string: String) -> Result<(), String> {
    // Promote the app to a Regular activation policy so the dock
    // icon appears and the bounded Attention window shows up in Cmd+Tab. When
    // the window closes, `register_attention_window_close_demotion` flips it
    // back to Accessory.
    #[cfg(target_os = "macos")]
    {
        promote_for_foreground_window(app);
    }
    let result = (|| {
        let url: tauri::Url = url_string
            .parse()
            .map_err(|error| format!("invalid Attention URL '{url_string}': {error}"))?;
        let url = match reuse_attention_window_or_return_url(
            app.get_webview_window(ATTENTION_WINDOW_LABEL),
            url,
            |window, url| {
                window.navigate(url).map_err(|error| {
                    format!("failed to navigate Attention window to '{url_string}': {error}")
                })
            },
            present_window_result,
        )? {
            Some(url) => url,
            None => return Ok(()),
        };
        let app_clone = app.clone();
        let window = tauri::WebviewWindowBuilder::new(
            &app_clone,
            ATTENTION_WINDOW_LABEL,
            tauri::WebviewUrl::External(url),
        )
        .title(ATTENTION_WINDOW_TITLE)
        // Scripted custom-surface origin policy (plan 1.6): this is the one
        // desktop-managed webview rendering unified-ui routes at the shared
        // UI origin, so it is the only desktop surface a scripted
        // custom-surface frame could try to load in. The guard is inert for
        // every other navigation and consults the same kernel constants the
        // web host enforces; general app-surface routes open in the system
        // browser.
        .on_navigation(|url| {
            crate::app_surface_origin_policy::admits_webview_navigation(url.as_str())
        })
        .inner_size(ATTENTION_WINDOW_INNER_SIZE.0, ATTENTION_WINDOW_INNER_SIZE.1)
        .min_inner_size(ATTENTION_WINDOW_MIN_SIZE.0, ATTENTION_WINDOW_MIN_SIZE.1)
        .max_inner_size(ATTENTION_WINDOW_MAX_SIZE.0, ATTENTION_WINDOW_MAX_SIZE.1)
        .resizable(true)
        .center()
        .build()
        .map_err(|error| format!("failed to create Attention window: {error}"))?;
        register_attention_window_close_demotion(&app_clone, &window);
        present_window_result(&window)
    })();
    #[cfg(target_os = "macos")]
    return rollback_on_error(result, || {
        // Promotion happens before parse/navigation/build so the new window can
        // present correctly. Undo it on every failed exit, while preserving a
        // Regular policy if another foreground Magician window still exists.
        demote_if_no_foreground_windows(app);
    });
    #[cfg(not(target_os = "macos"))]
    result
}

/// Open a unified-UI destination. Attention and approval paths use the bounded
/// native Attention window and canonical item-ID deep links. Every other route
/// opens as a browser URL.
pub(crate) fn open_app_at_path(app: &AppHandle, path: &str) {
    let app = app.clone();
    let path = path.to_string();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = open_app_at_path_result(&app, &path).await {
            warn!("Failed to open Magican destination '{}': {}", path, error);
        }
    });
}

async fn open_app_at_path_result(app: &AppHandle, path: &str) -> Result<(), String> {
    let destination = app_open_destination(path);
    let destination_path = match &destination {
        AppOpenDestination::NativeAttention { path } | AppOpenDestination::Browser { path } => path,
    };
    let url_string = {
        let state = app.state::<crate::AppState>();
        let config = state.config.lock().await;
        app_url_at_path(&config, destination_path)
    };
    match destination {
        AppOpenDestination::NativeAttention { .. } => open_attention_window_at_url(app, url_string),
        AppOpenDestination::Browser { .. } => open_ui_url_in_browser(&url_string),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AppOpenDestination {
    NativeAttention { path: String },
    Browser { path: String },
}

fn app_open_destination(path: &str) -> AppOpenDestination {
    match attention_route_item_from_path(path) {
        Some(item_id) => AppOpenDestination::NativeAttention {
            path: attention_deep_link_path(item_id.as_deref()),
        },
        None => AppOpenDestination::Browser {
            path: path.to_string(),
        },
    }
}

fn reuse_attention_window_or_return_url<W>(
    existing_window: Option<W>,
    url: tauri::Url,
    navigate: impl FnOnce(&W, tauri::Url) -> Result<(), String>,
    present: impl FnOnce(&W) -> Result<(), String>,
) -> Result<Option<tauri::Url>, String> {
    match existing_window {
        Some(window) => {
            navigate(&window, url)?;
            present(&window)?;
            Ok(None)
        },
        None => Ok(Some(url)),
    }
}

fn rollback_on_error<T, E>(result: Result<T, E>, rollback: impl FnOnce()) -> Result<T, E> {
    if result.is_err() {
        rollback();
    }
    result
}

fn attention_route_item_from_path(path: &str) -> Option<Option<String>> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    };
    let url = tauri::Url::parse(&format!("http://magician.local{normalized}")).ok()?;
    let route = url.path().trim_end_matches('/');
    if route != "/attention" && route != "/approvals" {
        return None;
    }
    let item_id = url.query_pairs().find_map(|(key, value)| {
        if !matches!(
            key.as_ref(),
            "attention_item" | "approval_id" | "correlation_id"
        ) {
            return None;
        }
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    });
    Some(item_id)
}

fn attention_deep_link_path(item_id: Option<&str>) -> String {
    let mut path = "/attention?native_attention=1".to_string();
    if let Some(item_id) = item_id.map(str::trim).filter(|value| !value.is_empty()) {
        path.push_str("&attention_item=");
        path.push_str(&percent_encode_path_segment(item_id));
    }
    path
}

fn percent_encode_path_segment(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                output.push(byte as char)
            },
            _ => output.push_str(&format!("%{byte:02X}")),
        }
    }
    output
}

/// Intercept the Attention window's close requests so the red X
/// (or Cmd+W) HIDES the window instead of destroying it — and demotes
/// the activation policy back to Accessory so the dock icon
/// disappears. This keeps the menu-bar tray running.
/// "Quit Magician" from the dock right-click menu, or "Quit" from the
/// tray menu, still terminates the whole process normally because
/// those send `RunEvent::ExitRequested` not `WindowEvent::CloseRequested`.
#[cfg(target_os = "macos")]
fn register_attention_window_close_demotion(app: &AppHandle, window: &tauri::WebviewWindow) {
    let app_handle = app.clone();
    window.on_window_event(move |event| match event {
        tauri::WindowEvent::CloseRequested { api, .. } => {
            // Stop Tauri from destroying the window — hide it so a
            // future actionable notification re-shows the same window without
            // a re-create. Hiding via API also unsets the dock-icon anchor;
            // then demote activation policy.
            api.prevent_close();
            if let Some(w) = app_handle.get_webview_window(ATTENTION_WINDOW_LABEL) {
                let _ = w.hide();
            }
            demote_if_no_foreground_windows(&app_handle);
        },
        tauri::WindowEvent::Destroyed => {
            // Defensive: if the window does get destroyed (e.g. the
            // user explicitly quits the whole app), still demote so
            // the dock icon doesn't linger in any transient state.
            demote_if_no_foreground_windows(&app_handle);
        },
        _ => {},
    });
}

#[cfg(not(target_os = "macos"))]
fn register_attention_window_close_demotion(_app: &AppHandle, _window: &tauri::WebviewWindow) {}

#[cfg(target_os = "macos")]
fn register_transient_window_demotion(app: &AppHandle, window: &tauri::WebviewWindow) {
    let app_handle = app.clone();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            demote_if_no_foreground_windows(&app_handle);
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn register_transient_window_demotion(_app: &AppHandle, _window: &tauri::WebviewWindow) {}

#[cfg(target_os = "macos")]
fn promote_for_foreground_window(app: &AppHandle) {
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
}

#[cfg(not(target_os = "macos"))]
fn promote_for_foreground_window(_app: &AppHandle) {}

#[cfg(target_os = "macos")]
fn demote_if_no_foreground_windows(app: &AppHandle) {
    if foreground_window_visible(app) {
        return;
    }
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
}

#[cfg(not(target_os = "macos"))]
fn demote_if_no_foreground_windows(_app: &AppHandle) {}

fn foreground_window_visible(app: &AppHandle) -> bool {
    [
        ATTENTION_WINDOW_LABEL,
        SETTINGS_WINDOW_LABEL,
        LOGS_WINDOW_LABEL,
    ]
    .iter()
    .any(|label| {
        app.get_webview_window(label)
            .and_then(|window| window.is_visible().ok())
            .unwrap_or(false)
    })
}

fn present_window(window: &tauri::WebviewWindow) {
    let _ = present_window_result(window);
}

fn present_window_result(window: &tauri::WebviewWindow) -> Result<(), String> {
    window
        .show()
        .map_err(|error| format!("failed to show window: {error}"))?;
    window
        .unminimize()
        .map_err(|error| format!("failed to restore window: {error}"))?;
    window
        .set_focus()
        .map_err(|error| format!("failed to focus window: {error}"))?;
    Ok(())
}

/// Open the settings window, or focus it if already open.
pub fn open_settings_window(app: &AppHandle) {
    promote_for_foreground_window(app);
    if let Some(window) = app.get_webview_window(SETTINGS_WINDOW_LABEL) {
        if let Err(e) = window.navigate(desktop_app_url("/")) {
            warn!(
                "Failed to navigate settings window to desktop settings: {}",
                e
            );
        }
        present_window(&window);
        return;
    }

    match tauri::WebviewWindowBuilder::new(app, SETTINGS_WINDOW_LABEL, desktop_app_webview_url("/"))
        .title("Magican Settings")
        .inner_size(SETTINGS_WINDOW_INNER_SIZE.0, SETTINGS_WINDOW_INNER_SIZE.1)
        .min_inner_size(SETTINGS_WINDOW_MIN_SIZE.0, SETTINGS_WINDOW_MIN_SIZE.1)
        .resizable(true)
        // Same custom-surface origin guard as the Attention window: these
        // windows render the full SPA, whose TopBar can navigate to /apps,
        // so a scripted surface frame must be refused here too. Inert
        // unless an asset route is requested.
        .on_navigation(|url| {
            crate::app_surface_origin_policy::admits_webview_navigation(url.as_str())
        })
        .build()
    {
        Ok(window) => {
            register_transient_window_demotion(app, &window);
            present_window(&window);
        },
        Err(error) => warn!("Failed to open settings window: {}", error),
    }
}

/// Present the short, trusted approval step requested from Web Settings.
/// The web page owns discovery and guidance; the bundled desktop origin keeps
/// Keychain fingerprints and signing confirmations out of the server-owned UI.
pub(crate) fn open_android_observation_approval(app: &AppHandle, trust_mode: Option<&str>) {
    let mode = match trust_mode {
        Some("play_integrity") => 2,
        _ => 1,
    };
    ANDROID_OBSERVATION_APPROVAL_PENDING.store(mode, std::sync::atomic::Ordering::Release);
    open_settings_window(app);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        let trust_mode = if mode == 2 {
            "play_integrity"
        } else {
            "owner_pinned_private_build"
        };
        if let Err(error) = app.emit_to("settings", "open-android-observation-approval", trust_mode)
        {
            warn!("Failed to focus Android observation approval: {error}");
        }
    });
}

static ANDROID_OBSERVATION_APPROVAL_PENDING: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(0);

#[tauri::command]
pub(crate) fn take_android_observation_approval_request() -> Option<String> {
    match ANDROID_OBSERVATION_APPROVAL_PENDING.swap(0, std::sync::atomic::Ordering::AcqRel) {
        1 => Some("owner_pinned_private_build".to_owned()),
        2 => Some("play_integrity".to_owned()),
        _ => None,
    }
}

/// Open the logs window, or focus it if already open.
fn open_logs_window(app: &AppHandle) {
    promote_for_foreground_window(app);
    if let Some(window) = app.get_webview_window(LOGS_WINDOW_LABEL) {
        present_window(&window);
        return;
    }

    match tauri::WebviewWindowBuilder::new(app, LOGS_WINDOW_LABEL, desktop_app_webview_url("/logs"))
        .title("Magican Logs")
        .inner_size(800.0, 500.0)
        .resizable(true)
        // See the settings window: the guard is inert unless a scripted
        // custom-surface asset route is requested.
        .on_navigation(|url| {
            crate::app_surface_origin_policy::admits_webview_navigation(url.as_str())
        })
        .build()
    {
        Ok(window) => {
            register_transient_window_demotion(app, &window);
            present_window(&window);
        },
        Err(error) => warn!("Failed to open logs window: {}", error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_tray_surfaces_have_native_names() {
        assert_eq!(tray_surface_name_for("macos"), "menu-bar status item");
        assert_eq!(tray_surface_name_for("windows"), "notification-area icon");
        assert_eq!(tray_surface_name_for("linux"), "desktop-panel tray icon");
        assert_eq!(tray_surface_name_for("freebsd"), "system tray icon");
    }

    #[test]
    fn container_routing_external_ports_do_not_control_the_native_supervisor() {
        let mut config = crate::config::MagicianDesktopConfig::default();
        assert!(uses_local_supervisor(&config));
        assert_eq!(
            unmanaged_runtime_label(&config),
            "Runtime: Local supervisor"
        );
        config.network.magician_port = 13002;
        config.network.magicutor_port = 13003;
        assert!(!uses_local_supervisor(&config));
        assert_eq!(
            unmanaged_runtime_label(&config),
            "Runtime: External engine (http://127.0.0.1:13002)"
        );
        config.network.engine_base_url = Some("https://engine.example".into());
        assert!(!uses_local_supervisor(&config));
    }

    #[test]
    fn menu_bar_template_icon_is_a_visible_native_m_mask() {
        const SIZE: usize = 18;
        // Downsampling the supersampled Outfit glyph with Lanczos leaves a
        // subpixel fringe (alpha 1–4) around otherwise transparent pixels.
        // That fringe is visually transparent, but requiring byte-exact zero
        // makes this contract reject the checked-in generator output.
        const MAX_INVISIBLE_EDGE_ALPHA: u8 = 4;

        let icon = menu_bar_template_icon();
        assert_eq!((icon.width(), icon.height()), (SIZE as u32, SIZE as u32));
        assert_eq!(icon.rgba().len(), SIZE * SIZE * 4);
        assert!(icon
            .rgba()
            .chunks_exact(4)
            .all(|pixel| pixel[..3] == [0, 0, 0]));
        assert!(icon.rgba().chunks_exact(4).any(|pixel| pixel[3] == u8::MAX));

        let alpha = icon
            .rgba()
            .chunks_exact(4)
            .map(|pixel| pixel[3])
            .collect::<Vec<_>>();
        assert!((0..SIZE).all(|x| alpha[x] <= MAX_INVISIBLE_EDGE_ALPHA));
        assert!((0..SIZE).all(|x| alpha[(SIZE - 1) * SIZE + x] <= MAX_INVISIBLE_EDGE_ALPHA));
        assert!((0..SIZE).all(|y| alpha[y * SIZE] <= MAX_INVISIBLE_EDGE_ALPHA));
        assert!((0..SIZE).all(|y| alpha[y * SIZE + SIZE - 1] <= MAX_INVISIBLE_EDGE_ALPHA));
    }

    #[test]
    fn orb_rest_action_uses_plain_public_copy() {
        assert_eq!(orb_listening_toggle_label("armed"), "Let Orb Rest");
        assert_eq!(orb_listening_toggle_label("listening"), "Let Orb Rest");
        assert_eq!(orb_listening_toggle_label("off"), "Wake Orb");
        assert_eq!(orb_listening_toggle_label("ended"), "Wake Orb");
        assert_eq!(orb_listening_toggle_label("disarming"), "Wake Orb");
    }

    #[test]
    fn desktop_app_url_uses_tray_protocol_not_unified_ui_dev_url() {
        assert_eq!(
            desktop_app_url("/").as_str(),
            format!("{DESKTOP_APP_SCHEME}://localhost/")
        );
        assert_eq!(
            desktop_app_url("setup").as_str(),
            format!("{DESKTOP_APP_SCHEME}://localhost/setup")
        );
    }

    #[test]
    fn attention_paths_are_classified_for_the_native_attention_window() {
        assert_eq!(attention_route_item_from_path("/attention"), Some(None));
        assert_eq!(attention_route_item_from_path("attention/"), Some(None));
        assert_eq!(
            attention_route_item_from_path("/attention?attention=1&attention_item=pause%2F7"),
            Some(Some("pause/7".to_string()))
        );
        assert_eq!(
            attention_route_item_from_path("/approvals?approval_id=approval-4"),
            Some(Some("approval-4".to_string()))
        );
        assert_eq!(
            attention_route_item_from_path(
                "/approvals?attention_item=&approval_id=approval-after-empty"
            ),
            Some(Some("approval-after-empty".to_string()))
        );
        assert_eq!(
            attention_route_item_from_path(
                "/attention?correlation_id=question%20with%20unicode-%E2%9C%93#ignored"
            ),
            Some(Some("question with unicode-✓".to_string()))
        );
        assert_eq!(attention_route_item_from_path("/tasks"), None);
        assert_eq!(attention_route_item_from_path("/attention-history"), None);
        assert_eq!(attention_route_item_from_path(""), None);
    }

    #[test]
    fn attention_deep_links_target_the_canonical_page_and_item() {
        assert_eq!(
            attention_deep_link_path(None),
            "/attention?native_attention=1"
        );
        assert_eq!(
            attention_deep_link_path(Some("pause/id with spaces")),
            "/attention?native_attention=1&attention_item=pause%2Fid%20with%20spaces"
        );
        assert_eq!(
            attention_deep_link_path(Some("question-✓&next=wrong")),
            "/attention?native_attention=1&attention_item=question-%E2%9C%93%26next%3Dwrong"
        );
    }

    #[test]
    fn app_open_destination_reserves_only_attention_routes_for_tauri() {
        assert_eq!(
            app_open_destination("/attention?attention_item=pause%2F7"),
            AppOpenDestination::NativeAttention {
                path: "/attention?native_attention=1&attention_item=pause%2F7".to_string(),
            }
        );
        assert_eq!(
            app_open_destination("/approvals?approval_id=approval-4"),
            AppOpenDestination::NativeAttention {
                path: "/attention?native_attention=1&attention_item=approval-4".to_string(),
            }
        );
        for path in [
            "/today",
            "/chat",
            "/settings",
            "/settings/model-routing",
            "/t/thread-7?panel=details",
            "/attention-history",
        ] {
            assert_eq!(
                app_open_destination(path),
                AppOpenDestination::Browser {
                    path: path.to_string(),
                }
            );
        }
    }

    #[test]
    fn attention_window_contract_is_bounded_and_reuses_existing_window() {
        assert_eq!(ATTENTION_WINDOW_LABEL, "magician-attention");
        assert_eq!(ATTENTION_WINDOW_TITLE, "Magican — Needs You");
        assert_eq!(ATTENTION_WINDOW_INNER_SIZE, (820.0, 760.0));
        assert_eq!(ATTENTION_WINDOW_MIN_SIZE, (620.0, 560.0));
        assert_eq!(ATTENTION_WINDOW_MAX_SIZE, (1100.0, 900.0));

        let first = app_open_destination("/attention?attention_item=pause-1");
        let second = app_open_destination("/attention?attention_item=pause-2");
        assert_eq!(
            first,
            AppOpenDestination::NativeAttention {
                path: "/attention?native_attention=1&attention_item=pause-1".to_string(),
            }
        );
        assert_eq!(
            second,
            AppOpenDestination::NativeAttention {
                path: "/attention?native_attention=1&attention_item=pause-2".to_string(),
            }
        );

        let first_url: tauri::Url =
            "http://127.0.0.1:5173/attention?native_attention=1&attention_item=pause-1"
                .parse()
                .expect("valid Attention URL");
        let create_url = reuse_attention_window_or_return_url(
            None::<()>,
            first_url.clone(),
            |_, _| panic!("a missing window cannot be navigated"),
            |_| panic!("a missing window cannot be presented"),
        )
        .expect("missing window selects creation");
        assert_eq!(create_url, Some(first_url));

        let second_url: tauri::Url =
            "http://127.0.0.1:5173/attention?native_attention=1&attention_item=pause-2"
                .parse()
                .expect("valid Attention URL");
        let navigated = std::cell::Cell::new(false);
        let presented = std::cell::Cell::new(false);
        let create_url = reuse_attention_window_or_return_url(
            Some(()),
            second_url.clone(),
            |_, actual_url| {
                navigated.set(true);
                assert_eq!(actual_url, second_url);
                Ok(())
            },
            |_| {
                presented.set(true);
                Ok(())
            },
        )
        .expect("existing window navigation succeeds");
        assert_eq!(create_url, None);
        assert!(navigated.get());
        assert!(presented.get());
    }

    #[test]
    fn settings_window_opens_at_a_roomy_but_bounded_minimum_size() {
        assert_eq!(SETTINGS_WINDOW_INNER_SIZE, (720.0, 640.0));
        assert_eq!(SETTINGS_WINDOW_MIN_SIZE, (640.0, 540.0));
        assert!(SETTINGS_WINDOW_INNER_SIZE.0 >= SETTINGS_WINDOW_MIN_SIZE.0);
        assert!(SETTINGS_WINDOW_INNER_SIZE.1 >= SETTINGS_WINDOW_MIN_SIZE.1);
    }

    #[test]
    fn attention_window_reuse_propagates_navigation_failure() {
        let url: tauri::Url = "http://127.0.0.1:5173/attention"
            .parse()
            .expect("valid Attention URL");
        let presented = std::cell::Cell::new(false);
        let error = reuse_attention_window_or_return_url(
            Some(()),
            url,
            |_, _| Err("navigation failed".to_string()),
            |_| {
                presented.set(true);
                Ok(())
            },
        )
        .expect_err("navigation error must reach the notification command");
        assert_eq!(error, "navigation failed");
        assert!(!presented.get());
    }

    #[test]
    fn failed_attention_open_runs_activation_rollback_only_on_error() {
        let rollbacks = std::cell::Cell::new(0);
        assert_eq!(
            rollback_on_error(Ok::<_, &str>("opened"), || rollbacks
                .set(rollbacks.get() + 1)),
            Ok("opened")
        );
        assert_eq!(rollbacks.get(), 0);

        assert_eq!(
            rollback_on_error(Err::<(), _>("failed"), || rollbacks
                .set(rollbacks.get() + 1)),
            Err("failed")
        );
        assert_eq!(rollbacks.get(), 1);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn desktop_asset_path_rejects_parent_traversal() {
        let dist_dir = std::path::Path::new("/tmp/desktop-dist");
        assert!(desktop_asset_path(dist_dir, "/assets/app.js").is_some());
        assert!(desktop_asset_path(dist_dir, "/../secrets").is_none());
    }
}

/// Update the tray icon state and tooltip text.
pub fn update_tray_state(app: &AppHandle, state: TrayState) {
    let status_text = match state {
        TrayState::Running => "\u{1F7E2} Running",      // 🟢
        TrayState::Starting => "\u{1F7E1} Starting...", // 🟡
        TrayState::Stopped => "\u{1F534} Stopped",      // 🔴
        TrayState::Updating => "\u{1F535} Updating...", // 🔵
    };
    update_tray_state_with_detail(app, state, status_text, None);
}

/// Update tray state with per-service detail line shown below the status.
pub fn update_tray_state_with_detail(
    app: &AppHandle,
    state: TrayState,
    detail: &str,
    _update_version: Option<&str>,
) {
    let tray = match app.tray_by_id("main-tray") {
        Some(t) => t,
        None => return,
    };

    let (tooltip, status_text) = match state {
        TrayState::Running => ("Magican \u{2014} Running", "\u{1F7E2} Running"), // 🟢
        TrayState::Starting => ("Magican \u{2014} Starting...", "\u{1F7E1} Starting..."), // 🟡
        TrayState::Stopped => ("Magican \u{2014} Stopped", "\u{1F534} Stopped"), // 🔴
        TrayState::Updating => ("Magican \u{2014} Updating...", "\u{1F535} Updating..."), // 🔵
    };

    // Keep the status row current without replacing the native menu object.
    // Replacing an open macOS status-item menu dismisses it under the cursor.
    let status_label = format!(
        "Magican v{} \u{2014} {}\n{}",
        env!("CARGO_PKG_VERSION"),
        status_text,
        detail
    );
    set_status_label(app, &status_label);
    update_status_menu_item_text(app, &status_label);
    update_runtime_menu_item_text(app);

    let _ = tray.set_tooltip(Some(tooltip));

    let _ = app.emit(
        "tray-state",
        match state {
            TrayState::Running => "Running",
            TrayState::Starting => "Starting",
            TrayState::Stopped => "Stopped",
            TrayState::Updating => "Updating",
        },
    );
}
