// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app_android_authority;
mod app_macos_host;
mod app_macos_identity;
mod app_macos_pairing;
mod app_memory_authority;
mod app_surface_origin_policy;
mod cleanup;
mod commands;
mod config;
mod connect_route;
mod container;
mod container_host_relay;
mod contextual_assist;
mod cua_setup;
#[cfg(target_os = "macos")]
mod dock_icon;
#[cfg(target_os = "macos")]
mod dock_quit_intercept;
mod edge_client;
mod edge_dispatch;
mod engine_roots;
mod env_file;
mod health;
mod host_gateway;
mod host_imessage;
mod magician_auth;
mod manifest;
mod native_runtime;
mod onboarding;
mod orb_state;
mod orb_window;
mod overlay;
mod permissions;
mod port_check;
mod presentation_identity_generated;
mod runtime_paths;
mod screen_ask;
mod setup;
mod theme;
mod tray;
mod updater;
mod voice_gesture;
mod voice_note;
mod voice_wake;

use config::MagicianDesktopConfig;
use container::ContainerRuntime;
use presentation_identity_generated::{HOST_APP_NAME, PRODUCT_NAME};
use std::sync::{Arc, Mutex as StdMutex};
use tauri::{Emitter, Manager};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

/// Runs `f` inside an Objective-C autorelease pool.
///
/// Tokio worker threads live for the whole process and never drain a pool,
/// so AppKit work reached from a polling loop leaks every autoreleased
/// object. `available_monitors()` alone autoreleases an
/// `NSScreen.deviceDescription` dictionary per screen per call; unwrapped in
/// the 300 ms contextual-assist poll it grew the host to 7 GB in two days.
/// Wrap the synchronous body of any periodic task that touches windows or
/// monitors.
pub(crate) fn with_autorelease_pool<R>(f: impl FnOnce() -> R) -> R {
    objc2::rc::autoreleasepool(|_| f())
}

/// Shared application state, managed by Tauri.
pub struct AppState {
    pub runtime: Mutex<Option<Arc<dyn ContainerRuntime>>>,
    pub config: Mutex<MagicianDesktopConfig>,
    pub pre_existing: Mutex<Option<crate::manifest::PreExistingState>>,
    /// Version string of an available app update, used to persist tray menu indicator.
    pub pending_app_update: Mutex<Option<String>>,
    pub overlay_session: Mutex<overlay::OverlaySessionState>,
    pub screen_ask: Mutex<screen_ask::ScreenAskState>,
    pub host_gateway: Mutex<host_gateway::HostGatewayState>,
    pub voice_hotkeys: Mutex<voice_note::VoiceHotkeyState>,
    pub voice_gestures: Mutex<voice_gesture::VoiceGestureState>,
    /// Active push-to-talk mode, mirrored from the web's universal Call/Dictate
    /// switch via `voice_note::set_ptt_mode`. The native Left-Option hold reads
    /// it to dispatch to live PTT (`voice_note::PTT_MODE_LIVE`) or dictation
    /// (`voice_note::PTT_MODE_DICTATE`). Lock-free so the CGEventTap callback can
    /// read it synchronously.
    pub ptt_mode: std::sync::atomic::AtomicU8,
    /// Native desktop wake-word listener (Vosk in Rust). The device-local Orb
    /// settings are its only authority; composer/webview state cannot arm it or
    /// receive its accepted phrases.
    pub native_wake: StdMutex<voice_wake::WakeController>,
    /// Process-owned orb lifecycle. The webview projects this state but never
    /// invents microphone or conversation phases.
    pub orb: StdMutex<orb_state::OrbMachine>,
    /// Native panel geometry, presentation, and shortcut bookkeeping.
    pub orb_window: StdMutex<orb_window::OrbWindowRuntime>,
    /// Dedicated idle/sleep assertion held only while ambient listening is
    /// open. It intentionally never forces the display awake.
    pub orb_keep_awake: StdMutex<Option<keepawake::KeepAwake>>,
    /// Sequence-scoped ownership for a hands-free orb session reusing the live
    /// PTT transport. Zero means no orb owner; `u64::MAX` is a pending claim.
    /// Comparing the concrete sequence prevents stale socket cleanup from
    /// ending or relabeling a newer/ordinary PTT session.
    pub orb_conversation_owner: std::sync::atomic::AtomicU64,
    /// Lock-free mirror of persisted orb enablement for microphone callbacks
    /// and lease teardown paths that must never guess on config-lock contention.
    pub orb_enabled: std::sync::atomic::AtomicBool,
    /// Startup and guided setup keep every Orb entry point dormant until the
    /// backend, sign-in, and required capability onboarding have completed.
    /// This is deliberately separate from the persisted user preference:
    /// setup must not rewrite whether the user wants the Orb after onboarding.
    pub orb_setup_blocked: std::sync::atomic::AtomicBool,
    /// Lock-free mirror of the independent native wake setting. Unlike
    /// `orb_enabled`, this may remain true while the Orb lifecycle is hidden so
    /// the configured phrase can cold-start it.
    pub orb_wake_enabled: std::sync::atomic::AtomicBool,
    /// True only while an accepted cold wake is re-arming the Orb. It prevents
    /// the intermediate `Armed` projection from needlessly reopening Vosk just
    /// before `WakeHeard` transfers the microphone to conversation capture.
    pub orb_wake_handoff: std::sync::atomic::AtomicBool,
    /// True while Left Option is physically held for press-to-talk. The event
    /// tap writes it so a dictation poll can see the release without waiting
    /// on the async runtime.
    pub orb_ptt_down: std::sync::atomic::AtomicBool,
    /// True when the current Orb conversation was opened by the hold gesture.
    /// Live sessions started this way keep their socket after each release.
    /// Wake-word and Talk Now conversations leave it false and stay continuous.
    pub orb_ptt_session: std::sync::atomic::AtomicBool,
    /// Monotonic lease source and current owner for non-orb microphone work.
    /// A sequence-scoped compare/exchange prevents stale note/PTT teardown from
    /// resuming wake behind a newer capture.
    pub external_voice_generation: std::sync::atomic::AtomicU64,
    pub external_voice_owner: std::sync::atomic::AtomicU64,
    /// Lock-free audio envelopes written by realtime/network producers and
    /// drained by a fixed-rate UI emitter off the audio callback.
    pub orb_input_level: std::sync::Arc<std::sync::atomic::AtomicU32>,
    pub orb_output_level: std::sync::Arc<std::sync::atomic::AtomicU32>,
    pub tray_status_label: StdMutex<String>,
    pub tray_status_item: StdMutex<Option<tauri::menu::MenuItem<tauri::Wry>>>,
    pub tray_runtime_item: StdMutex<Option<tauri::menu::MenuItem<tauri::Wry>>>,
    /// The "Show/Hide Notifications" stateful toggle menu item. Its label flips
    /// with `notifications_enabled` and carries the pending-approval count when
    /// `> 0`. Stored so `set_pending_approval_count` + the toggle handler can
    /// `set_text` it without rebuilding the native menu. Mirrors the
    /// `tray_status_*` pattern.
    pub tray_notifications_item: StdMutex<Option<tauri::menu::MenuItem<tauri::Wry>>>,
    /// The platform tray icon handle, stored at `create_tray` time so
    /// `set_pending_approval_count` can call `set_title` to paint the pending
    /// count next to the icon.
    pub tray_icon: StdMutex<Option<tauri::tray::TrayIcon<tauri::Wry>>>,
    /// Number of approvals awaiting the user. When `> 0`, the pending count is
    /// painted on the tray icon (`set_title`) and appended to the notifications
    /// toggle label. Read synchronously by the tray handlers.
    pub pending_approval_count: StdMutex<u32>,
    /// Whether the notification overlay window may show. Default ON; mirrored
    /// from `config.general.notifications_enabled` at startup and persisted back
    /// there on toggle. Held as a sync flag so the tray handler (which runs off
    /// the async runtime) can read/flip it without awaiting the config lock.
    /// The overlay route gates its own window visibility on the same flag via
    /// `get_notifications_enabled` + the `notifications-enabled-changed` event.
    pub notifications_enabled: StdMutex<bool>,
    pub keep_awake: StdMutex<Option<keepawake::KeepAwake>>,
    /// Keychain-rooted monotonic Android Apps authority. It is isolated from
    /// the general host gateway and runs blocking Keychain/durable I/O only on
    /// Tokio's blocking pool under this single process owner.
    pub(crate) app_android_authority:
        Arc<StdMutex<app_android_authority::AppAndroidAuthorityOwner>>,
}

/// Convert an incoming `magican://<path>` deep link into an app UI path,
/// preserving query and fragment state used by global overlays.
fn deep_link_path(url: &tauri::Url) -> Option<String> {
    if url.scheme() != "magican" {
        return None;
    }
    // `magican://t/thread` → host="t", path="/thread" → "/t/thread"
    // `magican:///tasks`   → host="",  path="/tasks"  → "/tasks"
    let mut path = String::new();
    if let Some(host) = url.host_str() {
        if !host.is_empty() {
            path.push('/');
            path.push_str(host);
        }
    }
    path.push_str(url.path());
    if path.trim().is_empty() || path.trim() == "/" {
        path = "/home".to_string();
    }
    if let Some(query) = url.query() {
        path.push('?');
        path.push_str(query);
    }
    if let Some(fragment) = url.fragment() {
        path.push('#');
        path.push_str(fragment);
    }
    Some(path)
}

fn android_observation_deep_link_trust_mode(url: &tauri::Url) -> Option<&'static str> {
    match deep_link_path(url).as_deref() {
        Some("/android-observation") => Some("owner_pinned_private_build"),
        Some("/android-observation/play-integrity") => Some("play_integrity"),
        _ => None,
    }
}

fn desktop_edge_enrollment_uri(url: &tauri::Url) -> Option<String> {
    if url.scheme() != "magican"
        || url.host_str() != Some("connect")
        || !matches!(url.path(), "" | "/")
        || url.fragment().is_some()
    {
        return None;
    }
    let mut desktop_kind = false;
    for (name, value) in url.query_pairs() {
        if name == "kind" {
            if desktop_kind || value != "desktop" {
                return None;
            }
            desktop_kind = true;
        }
    }
    desktop_kind.then(|| url.as_str().to_owned())
}

fn open_deep_link(app: &tauri::AppHandle, url: &tauri::Url) {
    if let Some(enrollment_uri) = desktop_edge_enrollment_uri(url) {
        if let Err(error) = setup::show_setup_window(app) {
            warn!("Could not open setup for Desktop Edge enrollment: {error}");
            return;
        }
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let payload = match edge_client::enroll_edge_client_uri(&app, &enrollment_uri).await {
                Ok(_) => serde_json::json!({ "ok": true }),
                Err(error) => serde_json::json!({ "ok": false, "error": error }),
            };
            if let Err(error) = app.emit("desktop-edge-enrollment", payload) {
                warn!("Could not report Desktop Edge enrollment result: {error}");
            }
        });
    } else if let Some(trust_mode) = android_observation_deep_link_trust_mode(url) {
        tray::open_android_observation_approval(app, Some(trust_mode));
    } else if let Some(path) = deep_link_path(url) {
        tray::open_app_at_path(app, &path);
    }
}

#[cfg(test)]
mod deep_link_tests {
    use super::{
        android_observation_deep_link_trust_mode, deep_link_path, desktop_edge_enrollment_uri,
    };

    #[test]
    fn deep_link_path_preserves_attention_selection_and_fragment() {
        let url =
            tauri::Url::parse("magican://attention?attention=1&attention_item=pause%2F7#current")
                .unwrap();
        assert_eq!(
            deep_link_path(&url).as_deref(),
            Some("/attention?attention=1&attention_item=pause%2F7#current")
        );
    }

    #[test]
    fn deep_link_path_keeps_non_attention_queries() {
        let url = tauri::Url::parse("magican:///tasks?selected=task-4").unwrap();
        assert_eq!(
            deep_link_path(&url).as_deref(),
            Some("/tasks?selected=task-4")
        );
    }

    #[test]
    fn android_observation_deep_link_is_an_exact_native_action() {
        assert_eq!(
            android_observation_deep_link_trust_mode(
                &tauri::Url::parse("magican://android-observation").unwrap()
            ),
            Some("owner_pinned_private_build")
        );
        assert_eq!(
            android_observation_deep_link_trust_mode(
                &tauri::Url::parse("magican://android-observation/play-integrity").unwrap()
            ),
            Some("play_integrity")
        );
        assert_eq!(
            android_observation_deep_link_trust_mode(
                &tauri::Url::parse("magican://android-observation?next=/tasks").unwrap()
            ),
            None
        );
        assert_eq!(
            android_observation_deep_link_trust_mode(
                &tauri::Url::parse("https://example.test/android-observation").unwrap()
            ),
            None
        );
    }

    #[test]
    fn only_desktop_connect_links_trigger_native_edge_enrollment() {
        let desktop = tauri::Url::parse(
            "magican://connect?base=https%3A%2F%2Fconnect.magican.ai&kind=desktop&id=e-1&secret=s-1",
        )
        .unwrap();
        assert_eq!(
            desktop_edge_enrollment_uri(&desktop).as_deref(),
            Some(desktop.as_str())
        );
        for rejected in [
            "magican://connect?base=https%3A%2F%2Fconnect.magican.ai&kind=ios&id=e-1&secret=s-1",
            "magican://connect/extra?kind=desktop",
            "magican://connect?kind=desktop&kind=desktop",
            "https://connect.magican.ai/?kind=desktop",
        ] {
            assert_eq!(
                desktop_edge_enrollment_uri(&tauri::Url::parse(rejected).unwrap()),
                None
            );
        }
    }
}

fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "magician_desktop=info".into()),
        )
        .init();

    info!("Starting {} v{}", HOST_APP_NAME, env!("CARGO_PKG_VERSION"));

    let mut builder = tauri::Builder::default();
    if desktop_autostart_plugin_enabled() {
        builder = builder.plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ));
    } else {
        info!("Desktop autostart plugin disabled for local external-runtime mode");
    }

    builder = builder.plugin(tauri_plugin_shell::init());
    #[cfg(target_os = "macos")]
    {
        builder = builder.plugin(tauri_nspanel::init());
    }
    // OS-level deep links: `magican://<path>` from anywhere on the system
    // (links, notifications, the web UI's "open in app") routes into this app.
    builder = builder.plugin(tauri_plugin_deep_link::init());
    if desktop_runtime_plugins_enabled() {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    } else {
        info!("Desktop updater plugin disabled for local external-runtime mode");
    }

    let context = tauri::generate_context!();
    let initial_config = config::load_config().unwrap_or_default();
    let initial_orb_enabled = initial_config.orb.enabled;
    let initial_orb_wake_enabled = initial_config.orb.wake_enabled;

    builder = tray::register_desktop_app_protocol(builder);

    builder
        .manage(AppState {
            runtime: Mutex::new(None),
            config: Mutex::new(initial_config),
            pre_existing: Mutex::new(None),
            pending_app_update: Mutex::new(None),
            overlay_session: Mutex::new(overlay::OverlaySessionState::default()),
            screen_ask: Mutex::new(screen_ask::ScreenAskState::default()),
            host_gateway: Mutex::new(host_gateway::HostGatewayState::default()),
            voice_hotkeys: Mutex::new(voice_note::VoiceHotkeyState::default()),
            voice_gestures: Mutex::new(voice_gesture::VoiceGestureState::default()),
            // Dictation as the safe construction-time default (overwritten by the
            // persisted-config seed before the tray is built — see setup()). Never
            // default to Live: an unseeded value must not arm a live mic.
            ptt_mode: std::sync::atomic::AtomicU8::new(voice_note::PTT_MODE_DICTATE),
            native_wake: StdMutex::new(voice_wake::WakeController::default()),
            orb: StdMutex::new(orb_state::OrbMachine::default()),
            orb_window: StdMutex::new(orb_window::OrbWindowRuntime::default()),
            orb_keep_awake: StdMutex::new(None),
            orb_conversation_owner: std::sync::atomic::AtomicU64::new(0),
            orb_enabled: std::sync::atomic::AtomicBool::new(initial_orb_enabled),
            // Construction starts fail-closed. `async_setup` releases the gate
            // only after it has proved that setup is no longer pending.
            orb_setup_blocked: std::sync::atomic::AtomicBool::new(true),
            orb_wake_enabled: std::sync::atomic::AtomicBool::new(initial_orb_wake_enabled),
            orb_wake_handoff: std::sync::atomic::AtomicBool::new(false),
            orb_ptt_down: std::sync::atomic::AtomicBool::new(false),
            orb_ptt_session: std::sync::atomic::AtomicBool::new(false),
            external_voice_generation: std::sync::atomic::AtomicU64::new(0),
            external_voice_owner: std::sync::atomic::AtomicU64::new(0),
            orb_input_level: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            orb_output_level: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            tray_status_label: StdMutex::new(format!(
                "{} v{}",
                PRODUCT_NAME,
                env!("CARGO_PKG_VERSION")
            )),
            tray_status_item: StdMutex::new(None),
            tray_runtime_item: StdMutex::new(None),
            tray_notifications_item: StdMutex::new(None),
            tray_icon: StdMutex::new(None),
            pending_approval_count: StdMutex::new(0),
            notifications_enabled: StdMutex::new(true),
            keep_awake: StdMutex::new(None),
            app_android_authority: Arc::new(StdMutex::new(
                app_android_authority::AppAndroidAuthorityOwner::default(),
            )),
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_version,
            commands::get_status,
            commands::start_container,
            commands::stop_container,
            commands::restart_container,
            commands::get_config,
            commands::save_config,
            commands::restart_with_new_config,
            connect_route::get_connect_route_status,
            connect_route::set_connect_route,
            commands::get_hotkey_mappings,
            commands::get_media_providers,
            commands::set_audio_engine_enabled,
            commands::get_media_preferences,
            commands::save_media_preferences,
            tray::take_android_observation_approval_request,
            magician_auth::get_magician_bearer_token,
            magician_auth::get_magician_connection_auth,
            magician_auth::set_magician_bearer_token,
            magician_auth::sign_in_magician,
            magician_auth::logout_magician_session,
            magician_auth::magician_http_request,
            edge_client::get_edge_client_status,
            edge_client::enroll_edge_client,
            edge_client::revoke_edge_client,
            env_file::get_environment_snapshot,
            env_file::reveal_environment_value,
            env_file::save_environment_value,
            commands::get_logs,
            commands::check_for_updates,
            commands::perform_update,
            commands::perform_app_update,
            setup::get_setup_options,
            setup::onboarding_completion_pending,
            setup::restart_onboarding_for_current_engine,
            setup::open_remote_enrollment_page,
            setup::apply_setup_selection,
            setup::finish_setup,
            onboarding::get_onboarding_catalog,
            onboarding::plan_onboarding,
            onboarding::set_onboarding_confirmation,
            onboarding::save_onboarding_secret,
            onboarding::save_onboarding_setup_token,
            onboarding::clear_onboarding_setup_token,
            onboarding::save_onboarding_configuration_file,
            onboarding::install_onboarding_skill,
            onboarding::uninstall_onboarding_skill,
            onboarding::save_onboarding_skill_secret,
            onboarding::start_onboarding_component_install,
            onboarding::get_onboarding_component_install,
            onboarding::get_onboarding_model_runtime,
            onboarding::configure_onboarding_model_runtime,
            onboarding::prepare_onboarding_browser_extension,
            onboarding::install_onboarding_cua_driver,
            onboarding::get_onboarding_harnesses,
            onboarding::refresh_onboarding_harness,
            onboarding::configure_onboarding_harness,
            onboarding::start_onboarding_bot_pairing,
            onboarding::get_onboarding_bot_pairing,
            onboarding::configure_onboarding_component_auth,
            onboarding::submit_onboarding_bot_auth_input,
            onboarding::start_onboarding_skill_auth,
            onboarding::get_onboarding_skill_auth,
            onboarding::configure_onboarding_skill_auth,
            onboarding::submit_onboarding_skill_auth_input,
            onboarding::open_onboarding_target,
            commands::approve_setup,
            commands::uninstall,
            commands::open_setup,
            commands::check_ports,
            commands::free_ports,
            permissions::get_desktop_permissions,
            permissions::open_desktop_permission_settings,
            permissions::request_desktop_permission,
            overlay::get_overlay_state,
            overlay::show_overlay,
            overlay::hide_overlay,
            overlay::show_contextual_assist,
            overlay::set_contextual_assist_expanded,
            overlay::get_contextual_assist_target,
            overlay::get_contextual_assist_display,
            overlay::hide_contextual_assist,
            overlay::show_notify_overlay,
            overlay::hide_notify_overlay,
            overlay::set_notify_overlay_focusable,
            overlay::resize_notify_overlay,
            tray::set_pending_approval_count,
            tray::get_notifications_enabled,
            tray::open_app_at,
            contextual_assist::get_contextual_assist_action_catalog,
            contextual_assist::get_contextual_assist_permission_status,
            contextual_assist::insert_contextual_text,
            contextual_assist::invoke_contextual_assist_action,
            contextual_assist::cancel_contextual_assist_action,
            contextual_assist::update_contextual_assist_webview_context,
            overlay::select_overlay_execution,
            overlay::open_dashboard,
            overlay::start_overlay_automation,
            overlay::submit_overlay_resume,
            overlay::submit_overlay_clarification,
            overlay::resolve_overlay_approval,
            overlay::take_overlay_draw_shapes,
            overlay::take_tutor_overlay_status,
            overlay::set_draw_overlay_control_regions,
            overlay::show_tutor_overlay_status,
            screen_ask::hud_attach_screen_context,
            screen_ask::take_screen_ask_capture,
            screen_ask::cancel_screen_region_selection,
            screen_ask::complete_screen_region_selection,
            host_gateway::get_host_gateway_status,
            host_gateway::get_app_macos_host_pairing_status,
            host_gateway::begin_app_macos_host_identity_approval,
            host_gateway::approve_app_macos_host_pairing,
            host_gateway::revoke_app_macos_host_pairing,
            host_gateway::reset_app_macos_host_pairing,
            app_android_authority::get_app_android_authority_status,
            app_android_authority::begin_app_android_authority_identity_bootstrap,
            app_android_authority::complete_app_android_authority_identity_bootstrap,
            app_android_authority::begin_app_android_apps_enrollment,
            app_android_authority::cancel_app_android_apps_enrollment,
            app_android_authority::list_app_android_authority_targets,
            app_android_authority::propose_app_android_authority_review,
            app_android_authority::refresh_app_android_authority_pending,
            app_android_authority::confirm_app_android_authority_proposal,
            app_android_authority::begin_app_android_authority_recovery,
            app_android_authority::confirm_app_android_authority_recovery,
            app_memory_authority::sign_app_memory_owner_decision,
            voice_note::toggle_voice_note,
            voice_note::start_voice_note,
            voice_note::stop_voice_note,
            voice_note::run_voice_note_recording_test,
            voice_note::end_live_ptt,
            voice_note::toggle_live_ptt_mute,
            voice_note::toggle_voice_output_mute,
            voice_note::set_tutor_audio_focus,
            voice_note::interrupt_live_ptt,
            voice_note::set_ptt_mode,
            orb_window::get_orb_snapshot,
            orb_window::get_orb_intro,
            orb_window::get_orb_presentation,
            orb_window::orb_expand,
            orb_window::orb_spotlight,
            orb_window::orb_collapse,
            orb_window::orb_set_edge_open,
            orb_window::orb_reset_home,
            orb_window::orb_start_conversation,
            orb_window::orb_disarm,
            orb_window::orb_rearm,
            orb_window::orb_pause_one_hour,
            orb_window::orb_resume,
            orb_window::orb_set_reduced_motion,
            orb_window::orb_set_shortcut_recording,
            orb_window::orb_open_app,
            orb_window::orb_open_settings,
            theme::set_app_theme,
            theme::get_app_theme,
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            {
                if let Err(error) = dock_icon::install_runtime_dock_icon() {
                    warn!(error = %error, "Could not install runtime Dock icon");
                }
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }

            let app_handle = app.handle().clone();

            // Seed the tray's push-to-talk (voice) mode from persisted config
            // BEFORE the menu is built, so the FIRST paint reflects the user's
            // saved Dictation/Live choice instead of an in-memory default. The
            // backend media-preferences sync (async, post-boot) still reconciles
            // any later change, but seeding here removes the launch flash AND the
            // externally-managed-runtime race that left the tray stuck on Live
            // until a manual Settings re-save. Reads the desktop's own config
            // cache (no backend dependency); defaults to Dictation when unset.
            {
                let cfg = config::load_config().unwrap_or_default();
                let mode = voice_note::ptt_mode_from_voice_mode(&cfg.voice.voice_mode);
                app.state::<AppState>()
                    .ptt_mode
                    .store(mode, std::sync::atomic::Ordering::Relaxed);
            }

            // Create the macOS menu-bar item, Windows notification-area icon,
            // or Linux desktop-panel tray icon with one shared control menu.
            tray::create_tray(&app_handle).expect("Failed to create desktop tray surface");
            if let Err(error) = orb_window::initialize_orb_window(&app_handle) {
                warn!("Failed to initialize notch orb window: {error}");
            }

            // OS-level deep links: a `magican://<path>` URL opened anywhere on
            // the system opens the matching browser route, except Attention and
            // approval routes, which use the bounded native Attention window.
            // Live URLs arrive through the event listener; Windows/Linux
            // cold-start arguments are read via `get_current()` after the
            // listener is installed.
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                // Register the running binary as the `magican://` handler so an
                // unbundled dev run works too; the bundled Info.plist also
                // declares the scheme. Best-effort.
                let _ = app.deep_link().register("magican");
                let links_handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        let handle = links_handle.clone();
                        let _ = links_handle.run_on_main_thread(move || {
                            open_deep_link(&handle, &url);
                        });
                    }
                });
                match app.deep_link().get_current() {
                    Ok(Some(urls)) => {
                        for url in urls {
                            open_deep_link(app.handle(), &url);
                        }
                    },
                    Ok(None) => {},
                    Err(error) => warn!("Failed to read startup deep link: {error}"),
                }
            }

            // Intercept macOS dock right-click → Quit at the
            // NSApplicationDelegate level. Tauri 2.10 doesn't fire
            // `RunEvent::ExitRequested` for that signal — it goes
            // straight to `RunEvent::Exit` with no chance to cancel.
            // The objc2 swizzle returns NSTerminateCancel and hides
            // the window + demotes activation policy instead.
            #[cfg(target_os = "macos")]
            dock_quit_intercept::install(&app_handle);

            // Run async setup in background
            let setup_handle = app_handle.clone();
            std::thread::spawn(move || {
                // Let Tauri finish macOS applicationDidFinishLaunching before
                // touching managed state or runtime services. Scheduling
                // directly inside the delegate callback can panic across the
                // Objective-C boundary.
                std::thread::sleep(std::time::Duration::from_millis(750));
                tauri::async_runtime::block_on(async move {
                    if let Err(e) = async_setup(setup_handle).await {
                        error!("Setup failed: {}", e);
                    }
                });
            });

            Ok(())
        })
        .build(context)
        .unwrap_or_else(|error| panic!("Error while building {HOST_APP_NAME}: {error}"))
        .run(|app, event| {
            // Diagnostic — log every RunEvent so we can see what
            // macOS dock right-click → Quit actually fires.
            match &event {
                tauri::RunEvent::ExitRequested { code, .. } => {
                    info!("RunEvent::ExitRequested code={:?}", code);
                },
                tauri::RunEvent::Exit => {
                    info!("RunEvent::Exit");
                },
                tauri::RunEvent::WindowEvent {
                    label, event: we, ..
                } => {
                    info!("RunEvent::WindowEvent label={} event={:?}", label, we);
                },
                _ => {},
            }
            // macOS dock right-click → Quit should fire
            // `RunEvent::ExitRequested`. Intercept and prevent the
            // exit so the menu-bar tray stays running.
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                info!(
                    "ExitRequested code={:?} → intercepting={}",
                    code,
                    code.is_none()
                );
                if code.is_none() {
                    api.prevent_exit();
                    if let Some(window) =
                        app.get_webview_window(crate::tray::ATTENTION_WINDOW_LABEL)
                    {
                        let _ = window.hide();
                    }
                    #[cfg(target_os = "macos")]
                    {
                        let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
                    }
                }
            }
        });
}

/// Async setup: detect runtime, check container state, spawn background tasks.
async fn async_setup(app: tauri::AppHandle) -> Result<(), String> {
    // Load configuration
    let cfg = config::load_config().unwrap_or_else(|e| {
        tracing::warn!("Failed to load config, using defaults: {}", e);
        MagicianDesktopConfig::default()
    });
    // `load_config` first migrates a legacy desktop config, so determine first
    // run only after that migration has had a chance to preserve an existing
    // installation.
    let first_run = !config::config_file_path().exists();

    {
        // Apply persisted config (async tokio mutex). `state` is confined to this
        // scope so it is NOT held across the await and reused below — holding a
        // `State` (which borrows `app`) across the await and using it afterwards
        // is what tripped E0597.
        let state = app.state::<AppState>();
        let mut config = state.config.lock().await;
        *config = cfg.clone();
    }
    {
        // Seed the sync notification toggle from persisted config (no await held)
        // so the tray label + the overlay gating reflect the user's last choice.
        let state = app.state::<AppState>();
        // Single-expression lock+write: the guard temporary drops at this
        // statement's `;` (before `state`), avoiding the `if let` temporary-scope
        // extension whose destructor otherwise ran after `state` was dropped (E0597).
        let _ = state
            .notifications_enabled
            .lock()
            .map(|mut enabled| *enabled = cfg.general.notifications_enabled);
    }
    // Re-paint the tray menu now that the persisted toggle state is loaded, so
    // the notifications item shows Show vs Hide correctly on a fresh launch.
    tray::refresh_notifications_menu_item(&app);

    {
        let state = app.state::<AppState>();
        let mut ka = state.keep_awake.lock().unwrap();
        if cfg.general.prevent_sleep {
            *ka = keepawake::Builder::default()
                .display(true)
                .idle(true)
                .sleep(true)
                .create()
                .ok();
        } else {
            *ka = None;
        }
    }

    // The host gateway is an essential local service and must not wait behind
    // optional WebView creation, Accessibility probes, or shortcut setup.
    // Bind it before those surfaces so backend speech/media requests can use it
    // as soon as the menu-bar process is alive.
    if let Err(error) = app_android_authority::start_bootstrap_socket_listener(app.clone()).await {
        // Android Apps remains typed unavailable on unsigned/non-macOS or
        // unverifiable bootstrap transports; unrelated desktop services keep
        // starting normally.
        warn!(error = %error, "Android Apps owner bootstrap remains unavailable");
    }
    host_gateway::start_background_services(app.clone()).await?;
    edge_client::start(app.clone());

    if let Err(e) = overlay::initialize_overlay(&app, cfg.general.hud_prewarm) {
        tracing::warn!("Overlay initialization failed: {}", e);
    }

    // Notification, HUD, and contextual-assist WebViews stay lazy during boot;
    // each show path ensures its own window, avoiding several hidden WebKit
    // processes and main-thread round trips during a cold tray launch. The HUD
    // alone is pre-warmed a few seconds later by `initialize_overlay` so its
    // first summon is warm; `general.hud_prewarm = false` opts out of the
    // resident WebView.

    if let Err(error) =
        overlay::sync_overlay_shortcut(&app, &cfg.general.quick_overlay_shortcut).await
    {
        tracing::warn!(
            "Failed to apply configured overlay shortcut '{}': {}",
            cfg.general.quick_overlay_shortcut,
            error
        );
    }
    if let Err(error) = voice_note::sync_voice_shortcuts(&app, &cfg).await {
        tracing::warn!("Failed to apply configured voice shortcuts: {}", error);
    }
    if let Err(error) = orb_window::sync_orb_shortcut(&app, &cfg.orb.hotkey) {
        tracing::warn!(
            "Failed to apply configured orb shortcut '{}': {}",
            cfg.orb.hotkey,
            error
        );
    }
    orb_window::start_tick_loop(app.clone());
    if let Err(error) = screen_ask::sync_screen_chord(
        &app,
        screen_ask::ScreenChord::Screenshot,
        &cfg.general.screen_ask_shortcut,
    )
    .await
    {
        tracing::warn!(
            "Failed to apply configured screen-ask shortcut '{}': {}",
            cfg.general.screen_ask_shortcut,
            error
        );
    }
    if let Err(error) = screen_ask::sync_screen_chord(
        &app,
        screen_ask::ScreenChord::Clip,
        &cfg.general.screen_clip_shortcut,
    )
    .await
    {
        tracing::warn!(
            "Failed to apply configured screen-clip shortcut '{}': {}",
            cfg.general.screen_clip_shortcut,
            error
        );
    }
    if let Err(error) = screen_ask::sync_screen_chord(
        &app,
        screen_ask::ScreenChord::Region,
        &cfg.general.screen_region_shortcut,
    )
    .await
    {
        tracing::warn!(
            "Failed to apply configured screen-region shortcut '{}': {}",
            cfg.general.screen_region_shortcut,
            error
        );
    }
    if let Err(error) = screen_ask::sync_screen_chord(
        &app,
        screen_ask::ScreenChord::Watch,
        &cfg.general.screen_watch_shortcut,
    )
    .await
    {
        tracing::warn!(
            "Failed to apply configured screen-watch shortcut '{}': {}",
            cfg.general.screen_watch_shortcut,
            error
        );
    }
    if first_run {
        info!("First run detected, opening placement setup");
        tray::update_tray_state(&app, tray::TrayState::Stopped);
        setup::show_setup_window(&app)?;
        spawn_background_monitors(app.clone());
        return Ok(());
    }
    if cfg!(target_os = "macos")
        && crate::native_runtime::installed_by_desktop()
        && !cfg.is_remote_engine()
    {
        match crate::native_runtime::ensure_service_started().await {
            Ok(()) => {
                info!("Ensured the Desktop-managed native backend service is started");
                tray::update_tray_state(&app, tray::TrayState::Starting);
            },
            Err(error) => {
                warn!("Could not start the Desktop-managed native backend: {error}");
                tray::update_tray_state(&app, tray::TrayState::Stopped);
            },
        }
    }
    let onboarding_pending = onboarding::completion_pending_for_engine(&cfg.engine_base_url());
    if !crate::engine_roots::should_supervise_local_engine(&cfg)
        || !manage_runtime_stack_enabled(&cfg)
    {
        if cfg.is_remote_engine() {
            info!(
                "Remote engine at {}; skipping local container supervision",
                cfg.engine_base_url()
            );
        } else {
            info!(
                "Desktop runtime stack management disabled; using externally managed localhost services"
            );
        }
        spawn_background_monitors(app.clone());
        if onboarding_pending {
            setup::show_setup_window_mode(&app, Some("capabilities"))?;
        } else {
            orb_window::release_setup_gate(&app, &cfg.orb);
        }
        return Ok(());
    }

    // Detect container runtime
    let (detected, runtime) = container::detect::detect_runtime().await?;
    info!("Using runtime: {:?} ({})", detected, runtime.name());

    {
        let state = app.state::<AppState>();
        let mut rt = state.runtime.lock().await;
        *rt = Some(Arc::clone(&runtime));
    }

    // Check if container is already running
    let info = match runtime.container_info(&cfg.general.container_name).await {
        Ok(info) => info,
        Err(error) => {
            // Unknown CLI output is not evidence that the container is absent
            // or stopped. Do not enter either creation/replacement path.
            warn!("Cannot inspect configured container; leaving it untouched: {error}");
            spawn_background_monitors(app.clone());
            return Ok(());
        },
    };

    let runtime_ready = match info.status {
        container::ContainerStatus::Running => {
            info!(
                "Container '{}' is already running",
                cfg.general.container_name
            );
            tray::update_tray_state(&app, tray::TrayState::Running);
            true
        },
        container::ContainerStatus::Stopped => {
            info!(
                "Container '{}' exists but is stopped, starting...",
                cfg.general.container_name
            );
            tray::update_tray_state(&app, tray::TrayState::Starting);
            match runtime.start_existing(&cfg.general.container_name).await {
                Ok(()) => true,
                Err(e) => {
                    error!("Failed to start existing container: {}", e);
                    tray::update_tray_state(&app, tray::TrayState::Stopped);
                    false
                },
            }
        },
        container::ContainerStatus::NotFound => {
            let image_exists = runtime
                .image_exists(&cfg.general.container_image)
                .await
                .unwrap_or(false);

            if image_exists {
                info!("Image exists, starting container...");
                tray::update_tray_state(&app, tray::TrayState::Starting);
                let container_config = container::ContainerConfig::from_desktop_config(&cfg);
                // Free any stale ports before starting
                let port_result =
                    port_check::check_ports(&container_config.ports, &cfg.general.container_name)
                        .await;
                if !port_result.all_clear() {
                    let still = port_check::free_conflicted_ports(&port_result.conflicts).await;
                    if !still.is_empty() {
                        error!("Port(s) {:?} occupied — cannot start container", still);
                        let _ = app.emit("port-conflict", &port_result);
                        tray::update_tray_state(&app, tray::TrayState::Stopped);
                        return Ok(());
                    }
                }
                match runtime.start(&container_config).await {
                    Ok(()) => true,
                    Err(e) => {
                        error!("Failed to start container: {}", e);
                        tray::update_tray_state(&app, tray::TrayState::Stopped);
                        false
                    },
                }
            } else {
                info!("No managed image found, opening placement setup");
                tray::update_tray_state(&app, tray::TrayState::Stopped);
                setup::show_setup_window(&app)?;
                false
            }
        },
        container::ContainerStatus::Restarting => {
            info!("Container is restarting, waiting...");
            tray::update_tray_state(&app, tray::TrayState::Starting);
            false
        },
    };

    spawn_background_monitors(app.clone());
    if onboarding_pending {
        setup::show_setup_window_mode(&app, Some("capabilities"))?;
    } else if runtime_ready {
        orb_window::release_setup_gate(&app, &cfg.orb);
    }

    Ok(())
}

fn manage_runtime_stack_enabled(cfg: &MagicianDesktopConfig) -> bool {
    let requested = match std::env::var("MAGICIAN_DESKTOP_MANAGE_RUNTIME") {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => cfg.general.manage_runtime_stack,
    };
    cfg.should_manage_runtime_stack(requested)
}

fn desktop_autostart_plugin_enabled() -> bool {
    desktop_runtime_plugins_enabled()
}

fn desktop_runtime_plugins_enabled() -> bool {
    match std::env::var("MAGICIAN_DESKTOP_MANAGE_RUNTIME") {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

fn spawn_background_monitors(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(container_host_relay::monitor(app.clone()));
    commands::spawn_media_preferences_sync(app.clone());

    let health_handle = app.clone();
    tauri::async_runtime::spawn(health::health_monitor(health_handle));

    let update_handle = app.clone();
    tauri::async_runtime::spawn(updater::update_check_loop(update_handle));

    let overlay_handle = app.clone();
    tauri::async_runtime::spawn(overlay::overlay_monitor(overlay_handle));

    let contextual_assist_handle = app.clone();
    tauri::async_runtime::spawn(contextual_assist::contextual_assist_monitor(
        contextual_assist_handle,
    ));
}
