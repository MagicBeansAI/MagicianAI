//! Tauri command handlers for the Magician desktop app.
//!
//! These are invoked by the Svelte frontend via `@tauri-apps/api/core::invoke()`.

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt as AutostartExt;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, warn};

use crate::config::{normalize_config, save_config as save_config_to_disk, MagicianDesktopConfig};
use crate::container::{ContainerConfig, ContainerStatus};
use crate::updater::{
    check_for_updates as check_updates_impl, perform_container_update, UpdateState,
};
use crate::AppState;

/// Status response returned to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub container_running: bool,
    pub container_status: String,
    pub runtime_name: String,
    pub image: String,
    pub health: String,
    pub needs_setup: bool,
    #[serde(default)]
    pub runtime_managed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaProviderInfo {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    pub model: String,
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub voices: Vec<String>,
    #[serde(default)]
    pub formats: Vec<String>,
    #[serde(default)]
    pub streaming: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaAudioStageOption {
    pub option_id: String,
    pub stage: String,
    pub provider_id: String,
    pub engine_id: String,
    pub model_id: String,
    #[serde(default)]
    pub variant: Option<String>,
    pub label: String,
    pub availability: String,
    #[serde(default)]
    pub unavailable_reason: Option<String>,
    #[serde(default)]
    pub capabilities: Value,
}

/// One row of `/media/providers` → `realtime_voice_profiles`, the same shape
/// the web and mobile pickers render. Only what the Orb needs is kept.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RealtimeVoiceProfileOption {
    pub profile_id: String,
    pub label: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    /// `backend_proxied` or `direct_peer_to_peer`; the Orb's Live PTT runs
    /// through Magician, so only backend-proxied rows are offered.
    #[serde(default)]
    pub topology: String,
    /// `assistant` or `translation`; a translator is not a conversation.
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MediaProviderSnapshot {
    #[serde(default)]
    pub tts: Option<MediaProviderInfo>,
    #[serde(default)]
    pub tts_fallbacks: Option<Vec<MediaProviderInfo>>,
    #[serde(default)]
    pub stt: Option<MediaProviderInfo>,
    #[serde(default)]
    pub stt_fallbacks: Option<Vec<MediaProviderInfo>>,
    #[serde(default)]
    pub realtime_voice: Option<MediaProviderInfo>,
    #[serde(default)]
    pub audio_revision: Option<String>,
    #[serde(default)]
    pub stages: BTreeMap<String, Vec<MediaAudioStageOption>>,
    #[serde(default)]
    pub surface_profiles: BTreeMap<String, Value>,
    #[serde(default)]
    pub default_surface_profiles: BTreeMap<String, String>,
    #[serde(default)]
    pub engines: BTreeMap<String, Value>,
    /// Selectable realtime engines the backend advertises, in its picker
    /// order. The Orb's Live mode picks one of these instead of asking the
    /// user to remember a profile id.
    #[serde(default)]
    pub realtime_voice_profiles: Vec<RealtimeVoiceProfileOption>,
    #[serde(default)]
    pub realtime_voice_default_profile: Option<String>,
    #[serde(default)]
    pub resolved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaPreferences {
    #[serde(default = "default_media_preferences_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub auto_speak: bool,
    #[serde(default = "default_media_voice_mode")]
    pub voice_mode: String,
    #[serde(default)]
    pub surface_profiles: BTreeMap<String, String>,
    #[serde(default)]
    pub surface_stage_options: BTreeMap<String, BTreeMap<String, String>>,
}

fn default_media_preferences_schema_version() -> u32 {
    2
}

fn default_media_voice_mode() -> String {
    // Dictation, not Live: a fresh profile must not default to an armed live mic.
    // Mirrors the backend canonical default (media_rails::preferences).
    "recording".to_string()
}

const MEDIA_PREFERENCES_SYNC_RETRY: Duration = Duration::from_secs(3);
const HOTKEY_MAPPINGS_UPDATED_EVENT: &str = "hotkey-mappings-updated";

#[derive(Debug, Clone, Serialize)]
pub struct HotkeyMappingsResponse {
    pub updated_at_ms: u64,
    pub groups: Vec<HotkeyMappingGroup>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HotkeyMappingGroup {
    pub id: String,
    pub label: String,
    pub items: Vec<HotkeyMappingItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HotkeyMappingItem {
    pub id: String,
    pub label: String,
    pub description: String,
    pub trigger: String,
    pub source: String,
    pub status: String,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

// --- Tauri Commands ---

#[tauri::command]
pub async fn get_version() -> Result<String, String> {
    Ok(env!("CARGO_PKG_VERSION").to_string())
}

#[tauri::command]
pub async fn get_status(app: AppHandle) -> Result<StatusResponse, String> {
    let state = app.state::<AppState>();
    let config = state.config.lock().await.clone();
    if cfg!(target_os = "macos")
        && crate::native_runtime::installed_by_desktop()
        && !config.is_remote_engine()
    {
        return Ok(native_runtime_status(&config).await);
    }
    let runtime_managed = runtime_stack_managed(&config);

    if !runtime_managed {
        return Ok(external_runtime_status(&config).await);
    }

    let runtime = state.runtime.lock().await;

    if let Some(ref rt) = *runtime {
        let rt = Arc::clone(rt);
        drop(runtime);

        let info = rt
            .container_info(&config.general.container_name)
            .await
            .unwrap_or_else(|_| crate::container::ContainerInfo {
                name: config.general.container_name.clone(),
                image: String::new(),
                status: ContainerStatus::NotFound,
                ports: vec![],
                created_at: None,
            });

        let needs_setup = info.status == ContainerStatus::NotFound;

        Ok(StatusResponse {
            container_running: info.status == ContainerStatus::Running,
            container_status: format!("{:?}", info.status),
            runtime_name: rt.name().to_string(),
            image: config.general.container_image.clone(),
            health: if info.status == ContainerStatus::Running {
                "checking".to_string()
            } else {
                "stopped".to_string()
            },
            needs_setup,
            runtime_managed,
        })
    } else {
        drop(runtime);

        Ok(StatusResponse {
            container_running: false,
            container_status: "NoRuntime".to_string(),
            runtime_name: "none".to_string(),
            image: config.general.container_image.clone(),
            health: "unknown".to_string(),
            needs_setup: true,
            runtime_managed,
        })
    }
}

async fn external_runtime_status(config: &MagicianDesktopConfig) -> StatusResponse {
    let health = crate::health::check_local_services(config).await;
    let running = health.magician == crate::health::ServiceHealth::Healthy;
    let health_label = if health.all_healthy() {
        "healthy"
    } else if running || health.magicutor == crate::health::ServiceHealth::Healthy {
        "partial"
    } else {
        "unreachable"
    };

    StatusResponse {
        container_running: running,
        container_status: if running {
            "ExternallyManaged".to_string()
        } else {
            "ServicesUnreachable".to_string()
        },
        runtime_name: "local services".to_string(),
        image: config.general.container_image.clone(),
        health: health_label.to_string(),
        needs_setup: false,
        runtime_managed: false,
    }
}

async fn native_runtime_status(config: &MagicianDesktopConfig) -> StatusResponse {
    let mut status = external_runtime_status(config).await;
    status.container_status = if status.container_running {
        "NativeRunning".to_string()
    } else {
        "NativeStopped".to_string()
    };
    status.runtime_name = crate::native_runtime::RUNTIME_NAME.to_string();
    status.runtime_managed = true;
    status
}

#[tauri::command]
pub async fn start_container(app: AppHandle) -> Result<(), String> {
    start_container_inner(&app).await
}

#[tauri::command]
pub async fn stop_container(app: AppHandle) -> Result<(), String> {
    stop_container_inner(&app).await
}

#[tauri::command]
pub async fn restart_container(app: AppHandle) -> Result<(), String> {
    restart_container_inner(&app).await
}

#[tauri::command]
pub async fn get_config(app: AppHandle) -> Result<MagicianDesktopConfig, String> {
    let state = app.state::<AppState>();
    let config = state.config.lock().await;
    Ok(config.clone())
}

#[tauri::command]
pub async fn get_hotkey_mappings(app: AppHandle) -> Result<HotkeyMappingsResponse, String> {
    let state = app.state::<AppState>();
    let config = state.config.lock().await.clone();
    drop(state);

    let overlay_gesture =
        crate::voice_gesture::overlay_gesture_label(&config.general.quick_overlay_gesture);
    let overlay_shortcut = crate::overlay::current_overlay_shortcut(&app);
    let (screen_ask, screen_region, screen_clip, screen_watch) =
        crate::screen_ask::current_screen_chords(&app);

    let contextual_enabled = config.contextual_assist.enabled
        && (config.contextual_assist.show_on_selected_text
            || config.contextual_assist.show_in_writable_fields);
    let contextual_note = if !config.contextual_assist.enabled {
        Some("Contextual Assist is off in Settings.".to_string())
    } else if !config.contextual_assist.show_on_selected_text
        && !config.contextual_assist.show_in_writable_fields
    {
        Some("Selected-text and writable-field targets are both off.".to_string())
    } else {
        Some("Safari and other WebView apps use this explicit tap path; native apps may also show the passive chip when a target is detected.".to_string())
    };

    let mut groups = Vec::new();
    groups.push(HotkeyMappingGroup {
        id: "writing-assistant".to_string(),
        label: "Writing Assistant".to_string(),
        items: vec![
            hotkey_mapping_item(
                "contextual-assist-left-option",
                "Open writing options",
                "Open the contextual writing menu for selected text or the focused writable field.",
                Some("Single Left Option".to_string()),
                "macOS event tap",
                contextual_enabled,
                contextual_note,
            ),
            hotkey_mapping_item(
                "contextual-assist-passive-chip",
                "Passive writing chip",
                "Show the small writing-assistant chip when native Accessibility detection finds selected text or a writable field.",
                Some("Selected text / writable field".to_string()),
                "Accessibility monitor",
                config.contextual_assist.enabled
                    && !config.contextual_assist.explicit_hotkey_only
                    && (config.contextual_assist.show_on_selected_text
                        || config.contextual_assist.show_in_writable_fields),
                if config.contextual_assist.explicit_hotkey_only {
                    Some("Explicit-hotkey-only mode suppresses the passive chip.".to_string())
                } else {
                    None
                },
            ),
        ],
    });
    groups.push(HotkeyMappingGroup {
        id: "ambient-orb".to_string(),
        label: "Ambient Orb".to_string(),
        items: vec![hotkey_mapping_item(
            "ambient-orb-toggle",
            "Summon, collapse, or wake the Orb",
            "Bring the ambient voice surface forward, return it to its notch presence, or wake it from rest.",
            nonempty_string(&config.orb.hotkey),
            "Tauri global shortcut",
            !config.orb.hotkey.trim().is_empty(),
            if config.orb.enabled {
                note_if_empty(&config.orb.hotkey, "No orb shortcut is configured.")
            } else {
                Some("The Orb is resting; this management chord remains available to wake it.".to_string())
            },
        )],
    });
    groups.push(HotkeyMappingGroup {
        id: "quick-automate".to_string(),
        label: "Quick Automate".to_string(),
        items: vec![
            hotkey_mapping_item(
                "quick-automate-gesture",
                "Open quick automate",
                "Toggle the desktop HUD from anywhere.",
                overlay_gesture.clone(),
                "macOS event tap",
                overlay_gesture.is_some(),
                None,
            ),
            hotkey_mapping_item(
                "quick-automate-shortcut-fallback",
                "Quick automate shortcut fallback",
                "Optional global shortcut for opening the desktop HUD.",
                nonempty_string(&overlay_shortcut),
                "Tauri global shortcut",
                !overlay_shortcut.trim().is_empty(),
                note_if_empty(
                    &overlay_shortcut,
                    "No quick automate fallback shortcut is configured.",
                ),
            ),
        ],
    });
    groups.push(HotkeyMappingGroup {
        id: "chat-commands".to_string(),
        label: "Chat Commands".to_string(),
        items: vec![
            hotkey_mapping_item(
                "chat-command-tutor",
                "Personal Tutor",
                "Typed feature command for explanation-only visual tutoring. In normal chat it can use a blackboard; in the HUD it can explain the captured screen.",
                Some("@tutor".to_string()),
                "Chat composer",
                true,
                Some("Aliases: @tutur, hey tutor, hey tutur.".to_string()),
            ),
            hotkey_mapping_item(
                "chat-command-tutor-quick",
                "Personal Tutor quick path",
                "Typed feature command for the fastest useful first overlay while preserving the normal step-by-step tutor flow.",
                Some("@tutor #quick".to_string()),
                "Chat composer",
                true,
                Some("Composer chip: @tutor_quick. #quick is a separate flag; @tutor#quick is not equivalent.".to_string()),
            ),
            hotkey_mapping_item(
                "chat-command-copilot",
                "App Copilot",
                "Typed feature command for screen-grounded app guidance and bounded UI actions. It needs a current screenshot or HUD capture to ground on.",
                Some("@copilot".to_string()),
                "HUD composer",
                true,
                Some("Aliases: @app-copilot, @appcopilot, hey copilot, hey app copilot.".to_string()),
            ),
            hotkey_mapping_item(
                "chat-command-copilot-quick",
                "App Copilot quick path",
                "Typed feature command for fastest useful first app overlay before continuing the normal observe, draw, act, and verify loop.",
                Some("@copilot #quick".to_string()),
                "HUD composer",
                true,
                Some("#quick also works with the App Copilot aliases when it is a whitespace-delimited flag.".to_string()),
            ),
        ],
    });
    groups.push(HotkeyMappingGroup {
        id: "screen-capture".to_string(),
        label: "Screen Capture".to_string(),
        items: vec![
            hotkey_mapping_item(
                "screen-ask",
                "Screenshot and ask",
                "Capture the main display, stage it in chat, and open the HUD.",
                nonempty_string(&screen_ask),
                "Tauri global shortcut",
                !screen_ask.trim().is_empty(),
                note_if_empty(&screen_ask, "Screenshot and ask is disabled."),
            ),
            hotkey_mapping_item(
                "screen-region",
                "Region or window and ask",
                "Pick a region or window, stage the capture, and open the HUD.",
                nonempty_string(&screen_region),
                "Tauri global shortcut",
                !screen_region.trim().is_empty(),
                note_if_empty(&screen_region, "Region and ask is disabled."),
            ),
            hotkey_mapping_item(
                "screen-clip",
                "Clip and ask",
                "Start or stop screen recording and open the HUD with the clip staged.",
                nonempty_string(&screen_clip),
                "Tauri global shortcut",
                !screen_clip.trim().is_empty(),
                note_if_empty(&screen_clip, "Clip and ask is disabled."),
            ),
            hotkey_mapping_item(
                "screen-watch",
                "Watch screen",
                "Toggle continuous screen observation for notes-mode review.",
                nonempty_string(&screen_watch),
                "Tauri global shortcut",
                !screen_watch.trim().is_empty(),
                note_if_empty(&screen_watch, "Watch screen is disabled."),
            ),
        ],
    });

    Ok(HotkeyMappingsResponse {
        updated_at_ms: now_millis(),
        groups,
    })
}

#[tauri::command]
pub async fn get_media_providers(app: AppHandle) -> Result<MediaProviderSnapshot, String> {
    let state = app.state::<AppState>();
    let config = state.config.lock().await.clone();
    let url = magician_media_url(&config, "/providers");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|err| format!("provider client error: {err}"))?;
    let response = crate::magician_auth::authorize(client.get(&url))
        .send()
        .await
        .map_err(|err| format!("provider request failed: {err}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("provider request returned HTTP {status}"));
    }
    response
        .json::<MediaProviderSnapshot>()
        .await
        .map_err(|err| format!("provider response parse failed: {err}"))
}

#[tauri::command]
pub async fn set_audio_engine_enabled(
    app: AppHandle,
    engine_id: String,
    enabled: bool,
    expected_revision: String,
) -> Result<Value, String> {
    let engine_id = engine_id.trim();
    if engine_id.is_empty() || expected_revision.trim().is_empty() {
        return Err("audio engine id and expected revision are required".to_string());
    }
    let state = app.state::<AppState>();
    let config = state.config.lock().await.clone();
    let url = magician_media_url(&config, "/audio-settings");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| format!("audio settings client error: {err}"))?;
    let response = crate::magician_auth::authorize(client.put(&url))
        .json(&audio_engine_patch_body(
            engine_id,
            enabled,
            &expected_revision,
        ))
        .send()
        .await
        .map_err(|err| format!("audio settings request failed: {err}"))?;
    let status = response.status();
    let body = response
        .json::<Value>()
        .await
        .map_err(|err| format!("audio settings response parse failed: {err}"))?;
    if !status.is_success() {
        let message = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("audio engine update failed");
        return Err(format!("{message} (HTTP {status})"));
    }
    let _ = app.emit("media-config-updated", body.clone());
    Ok(body)
}

fn audio_engine_patch_body(engine_id: &str, enabled: bool, expected_revision: &str) -> Value {
    let mut engines = serde_json::Map::new();
    engines.insert(
        engine_id.to_string(),
        serde_json::json!({ "enabled": enabled }),
    );
    serde_json::json!({
        "expected_revision": expected_revision,
        "engines": engines,
    })
}

#[cfg(test)]
mod audio_engine_patch_tests {
    use super::audio_engine_patch_body;

    #[test]
    fn patch_uses_the_runtime_engine_id_as_the_object_key() {
        let body = audio_engine_patch_body("fluid_audio", false, "revision-1");
        assert_eq!(body["expected_revision"], "revision-1");
        assert_eq!(body["engines"]["fluid_audio"]["enabled"], false);
        assert!(body["engines"].get("engine_id").is_none());
    }
}

#[tauri::command]
pub async fn get_media_preferences(app: AppHandle) -> Result<MediaPreferences, String> {
    load_media_preferences_for_default_scope(&app).await
}

pub async fn load_media_preferences_for_default_scope(
    app: &AppHandle,
) -> Result<MediaPreferences, String> {
    load_media_preferences_for_default_scope_with_orb_seed(app, false).await
}

async fn load_media_preferences_for_default_scope_with_orb_seed(
    app: &AppHandle,
    seed_orb_voice_mode: bool,
) -> Result<MediaPreferences, String> {
    let state = app.state::<AppState>();
    let config = state.config.lock().await.clone();
    let url = magician_media_url(&config, "/preferences");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|err| format!("media preferences client error: {err}"))?;
    let response = crate::magician_auth::authorize(client.get(&url))
        .send()
        .await
        .map_err(|err| format!("media preferences request failed: {err}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("media preferences request returned HTTP {status}"));
    }
    let preferences = response
        .json::<MediaPreferences>()
        .await
        .map_err(|err| format!("media preferences response parse failed: {err}"))?;
    apply_media_preferences_to_desktop_with_orb_seed(app, &preferences, seed_orb_voice_mode)
        .await?;
    Ok(preferences)
}

#[tauri::command]
pub async fn save_media_preferences(
    app: AppHandle,
    preferences: MediaPreferences,
) -> Result<MediaPreferences, String> {
    save_media_preferences_for_default_scope(&app, preferences).await
}

pub async fn save_media_preferences_for_default_scope(
    app: &AppHandle,
    preferences: MediaPreferences,
) -> Result<MediaPreferences, String> {
    let state = app.state::<AppState>();
    let config = state.config.lock().await.clone();
    let url = magician_media_url(&config, "/preferences");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|err| format!("media preferences client error: {err}"))?;
    let response = crate::magician_auth::authorize(client.put(&url))
        .json(&serde_json::json!({
            "auto_speak": preferences.auto_speak,
            "voice_mode": preferences.voice_mode,
            "surface_profiles": preferences.surface_profiles,
            "surface_stage_options": preferences.surface_stage_options,
        }))
        .send()
        .await
        .map_err(|err| format!("media preferences request failed: {err}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("media preferences request returned HTTP {status}"));
    }
    let saved = response
        .json::<MediaPreferences>()
        .await
        .map_err(|err| format!("media preferences response parse failed: {err}"))?;
    apply_media_preferences_to_desktop(app, &saved).await?;
    let _ = app.emit("media-preferences-updated", saved.clone());
    Ok(saved)
}

fn magician_media_url(config: &crate::config::MagicianDesktopConfig, path: &str) -> String {
    config.engine_url(&format!("/api/magician/v2/media{path}"))
}

fn magician_realtime_url(config: &crate::config::MagicianDesktopConfig) -> String {
    config.engine_ws_path("/api/magician/v2/realtime/ws")
}

pub fn emit_hotkey_mappings_updated(app: &AppHandle) {
    let _ = app.emit(HOTKEY_MAPPINGS_UPDATED_EVENT, ());
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn nonempty_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn note_if_empty(value: &str, message: &str) -> Option<String> {
    value.trim().is_empty().then(|| message.to_string())
}

fn hotkey_mapping_item(
    id: &str,
    label: &str,
    description: &str,
    trigger: Option<String>,
    source: &str,
    active: bool,
    note: Option<String>,
) -> HotkeyMappingItem {
    HotkeyMappingItem {
        id: id.to_string(),
        label: label.to_string(),
        description: description.to_string(),
        trigger: trigger.unwrap_or_else(|| "Not set".to_string()),
        source: source.to_string(),
        status: if active { "active" } else { "disabled" }.to_string(),
        active,
        note,
    }
}

pub fn spawn_media_preferences_sync(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        run_media_preferences_sync(app).await;
    });
}

async fn run_media_preferences_sync(app: AppHandle) {
    loop {
        match load_media_preferences_for_default_scope(&app).await {
            Ok(preferences) => {
                let _ = app.emit("media-preferences-updated", preferences);
            },
            Err(error) => {
                debug!("media preferences bootstrap sync skipped: {}", error);
            },
        }

        let config = app.state::<AppState>().config.lock().await.clone();
        let url = magician_realtime_url(&config);
        let request = match crate::magician_auth::websocket_request(&url) {
            Ok(request) => request,
            Err(error) => {
                debug!("media preferences sync request rejected: {}", error);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            },
        };
        match connect_async(request).await {
            Ok((mut socket, _)) => {
                debug!("connected desktop media preferences sync stream");
                // The realtime stream pushes only future CHANGES, not the current
                // state, and the one-shot bootstrap GET above can run before the
                // backend has finished loading the saved preferences (returning the
                // default) — which left the tray stuck on Live PTT until the user
                // re-saved. Now that the stream is live the backend is fully up, so
                // re-fetch + apply the current preferences so the saved Live/Dictation
                // mode is reflected on launch without a manual Save.
                if let Ok(preferences) =
                    load_media_preferences_for_default_scope_with_orb_seed(&app, true).await
                {
                    let _ = app.emit("media-preferences-updated", preferences);
                }
                while let Some(message) = socket.next().await {
                    match message {
                        Ok(Message::Text(text)) => {
                            if let Some(preferences) = media_preferences_from_realtime_text(&text) {
                                apply_realtime_media_preferences_update(&app, preferences).await;
                            }
                        },
                        Ok(Message::Binary(bytes)) => {
                            if let Ok(text) = String::from_utf8(bytes) {
                                if let Some(preferences) =
                                    media_preferences_from_realtime_text(&text)
                                {
                                    apply_realtime_media_preferences_update(&app, preferences)
                                        .await;
                                }
                            }
                        },
                        Ok(Message::Close(_)) => break,
                        Ok(Message::Ping(_)) | Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {},
                        Err(error) => {
                            debug!("media preferences sync stream closed: {}", error);
                            break;
                        },
                    }
                }
            },
            Err(error) => {
                debug!("media preferences sync stream unavailable: {}", error);
            },
        }

        tokio::time::sleep(MEDIA_PREFERENCES_SYNC_RETRY).await;
    }
}

fn media_preferences_from_realtime_text(text: &str) -> Option<MediaPreferences> {
    let frame: Value = serde_json::from_str(text).ok()?;
    let event = match frame.get("event_type").and_then(Value::as_str) {
        Some("AgentEvent") => frame.get("data")?.get("event")?,
        Some("media.preferences.updated") => &frame,
        _ => return None,
    };
    if event.get("event_type").and_then(Value::as_str) != Some("media.preferences.updated") {
        return None;
    }
    let preferences = event.get("payload")?.get("preferences")?.clone();
    serde_json::from_value::<MediaPreferences>(preferences).ok()
}

async fn apply_realtime_media_preferences_update(app: &AppHandle, preferences: MediaPreferences) {
    if let Err(error) = apply_media_preferences_to_desktop(app, &preferences).await {
        warn!(
            "failed to apply realtime media preferences to desktop tray: {}",
            error
        );
        return;
    }
    let _ = app.emit("media-preferences-updated", preferences);
}

pub async fn apply_media_preferences_to_desktop(
    app: &AppHandle,
    preferences: &MediaPreferences,
) -> Result<(), String> {
    apply_media_preferences_to_desktop_with_orb_seed(app, preferences, true).await
}

async fn apply_media_preferences_to_desktop_with_orb_seed(
    app: &AppHandle,
    preferences: &MediaPreferences,
    seed_orb_voice_mode: bool,
) -> Result<(), String> {
    let ptt_mode = crate::voice_note::ptt_mode_from_voice_mode(&preferences.voice_mode);
    let previous_ptt_mode = app
        .state::<AppState>()
        .ptt_mode
        .swap(ptt_mode, std::sync::atomic::Ordering::Relaxed);
    let ptt_mode_changed = previous_ptt_mode != ptt_mode;

    let config_changed = {
        let state = app.state::<AppState>();
        let mut current = state.config.lock().await;
        let mut next = current.clone();
        // Cache the voice mode locally so the tray can seed it synchronously on
        // the NEXT launch (before the backend is reachable). The backend file
        // remains the cross-surface source of truth; this is just the startup
        // mirror, written here whenever a preference apply lands.
        next.voice.voice_mode = preferences.voice_mode.clone();
        seed_orb_voice_mode_if_needed(&mut next, &preferences.voice_mode, seed_orb_voice_mode);
        let config_changed = *current != next;
        if config_changed {
            save_config_to_disk(&next)?;
            *current = next.clone();
        }
        config_changed
    };

    if config_changed || ptt_mode_changed {
        let update_version = app
            .state::<AppState>()
            .pending_app_update
            .lock()
            .await
            .clone();
        crate::tray::refresh_menu(app, update_version.as_deref());
        emit_hotkey_mappings_updated(app);
    }

    Ok(())
}

fn seed_orb_voice_mode_if_needed(
    config: &mut MagicianDesktopConfig,
    backend_mode: &str,
    authoritative: bool,
) -> bool {
    if !authoritative || config.orb.voice_mode_seeded {
        return false;
    }
    config.orb.voice_mode = crate::config::normalize_orb_voice_mode(backend_mode);
    config.orb.voice_mode_seeded = true;
    true
}

/// Orb placement is native window state, not editable Settings form state.
///
/// The Settings window may remain open while the orb is dragged. Preserve the
/// latest coordinates from the live config so saving an older form snapshot
/// cannot move the orb back to its previous/default position.
fn preserve_runtime_orb_placement(
    config: &mut MagicianDesktopConfig,
    current_config: &MagicianDesktopConfig,
) {
    config.orb.resting_x = current_config.orb.resting_x;
    config.orb.resting_y = current_config.orb.resting_y;
    config.orb.expanded_x = current_config.orb.expanded_x;
    config.orb.expanded_y = current_config.orb.expanded_y;
}

/// `voice.voice_mode` is a startup mirror of the shared media preference.
/// Web Settings owns that preference. A desktop settings snapshot must not
/// write an older mirror back over the live one.
fn preserve_shared_voice_mode_mirror(
    config: &mut MagicianDesktopConfig,
    current_config: &MagicianDesktopConfig,
) {
    config.voice.voice_mode = current_config.voice.voice_mode.clone();
}

#[tauri::command]
pub async fn save_config(app: AppHandle, mut config: MagicianDesktopConfig) -> Result<(), String> {
    normalize_config(&mut config);
    config.validate_engine_base_url()?;
    crate::overlay::validate_overlay_shortcut(&config.general.quick_overlay_shortcut)?;
    validate_voice_shortcuts(&config)?;
    validate_orb_shortcut_collisions(&config)?;
    // Screen-capture chords (empty = disabled; non-empty must parse).
    crate::screen_ask::validate_screen_ask_shortcut(&config.general.screen_ask_shortcut)?;
    crate::screen_ask::validate_screen_ask_shortcut(&config.general.screen_clip_shortcut)?;
    crate::screen_ask::validate_screen_ask_shortcut(&config.general.screen_region_shortcut)?;
    crate::screen_ask::validate_screen_ask_shortcut(&config.general.screen_watch_shortcut)?;

    let state = app.state::<AppState>();
    let current_config = state.config.lock().await.clone();
    preserve_runtime_orb_placement(&mut config, &current_config);
    preserve_shared_voice_mode_mirror(&mut config, &current_config);
    let old_shortcut = current_config.general.quick_overlay_shortcut.clone();
    let new_shortcut = config.general.quick_overlay_shortcut.clone();
    let shortcut_changed = old_shortcut != new_shortcut;
    let voice_changed = current_config.voice != config.voice;
    // The overlay double-tap gesture lives in `general` but is part of the same
    // CGEventTap spec, so an overlay-gesture-only change must also re-sync it.
    let overlay_gesture_changed =
        current_config.general.quick_overlay_gesture != config.general.quick_overlay_gesture;
    let gestures_changed = voice_changed || overlay_gesture_changed;
    let orb_changed = current_config.orb != config.orb;
    let old_orb_shortcut = current_config.orb.hotkey.clone();
    let orb_shortcut_changed = old_orb_shortcut != config.orb.hotkey;
    let launch_at_login_changed =
        current_config.general.launch_at_login != config.general.launch_at_login;
    let engine_changed = current_config.engine_base_url() != config.engine_base_url();

    if shortcut_changed {
        crate::overlay::sync_overlay_shortcut(&app, &new_shortcut).await?;
    }
    if gestures_changed {
        if let Err(error) = crate::voice_note::sync_voice_shortcuts(&app, &config).await {
            if shortcut_changed {
                let _ = crate::overlay::sync_overlay_shortcut(&app, &old_shortcut).await;
            }
            return Err(error);
        }
    }
    if orb_shortcut_changed {
        if let Err(error) = crate::orb_window::sync_orb_shortcut(&app, &config.orb.hotkey) {
            if shortcut_changed {
                let _ = crate::overlay::sync_overlay_shortcut(&app, &old_shortcut).await;
            }
            if gestures_changed {
                let _ = crate::voice_note::sync_voice_shortcuts(&app, &current_config).await;
            }
            return Err(error);
        }
    }
    if launch_at_login_changed {
        if let Err(error) = apply_launch_at_login(&app, config.general.launch_at_login) {
            if shortcut_changed {
                let _ = crate::overlay::sync_overlay_shortcut(&app, &old_shortcut).await;
            }
            if gestures_changed {
                let _ = crate::voice_note::sync_voice_shortcuts(&app, &current_config).await;
            }
            if orb_shortcut_changed {
                let _ = crate::orb_window::sync_orb_shortcut(&app, &old_orb_shortcut);
            }
            return Err(error);
        }
    }

    if let Err(error) = save_config_to_disk(&config) {
        if shortcut_changed {
            let _ = crate::overlay::sync_overlay_shortcut(&app, &old_shortcut).await;
        }
        if gestures_changed {
            let _ = crate::voice_note::sync_voice_shortcuts(&app, &current_config).await;
        }
        if orb_shortcut_changed {
            let _ = crate::orb_window::sync_orb_shortcut(&app, &old_orb_shortcut);
        }
        if launch_at_login_changed {
            let _ = apply_launch_at_login(&app, current_config.general.launch_at_login);
        }
        return Err(error);
    }
    if engine_changed {
        crate::onboarding::mark_incomplete_for_engine(&config.engine_base_url())?;
    }

    {
        let state = app.state::<crate::AppState>();
        let mut ka = state.keep_awake.lock().unwrap();
        if config.general.prevent_sleep {
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

    // Live-apply the screen-capture chords from the saved config so edits take
    // effect without a restart. Validated above; `sync_screen_chord` is
    // idempotent (no-ops when unchanged) and self-restores its previous binding
    // on an OS-level conflict — the saved config is the source of truth, and
    // startup re-syncs from it, so a rare failure is best-effort (logged inside).
    use crate::screen_ask::{sync_screen_chord, ScreenChord};
    let _ = sync_screen_chord(
        &app,
        ScreenChord::Screenshot,
        &config.general.screen_ask_shortcut,
    )
    .await;
    let _ = sync_screen_chord(
        &app,
        ScreenChord::Clip,
        &config.general.screen_clip_shortcut,
    )
    .await;
    let _ = sync_screen_chord(
        &app,
        ScreenChord::Region,
        &config.general.screen_region_shortcut,
    )
    .await;
    let _ = sync_screen_chord(
        &app,
        ScreenChord::Watch,
        &config.general.screen_watch_shortcut,
    )
    .await;

    // Read container name from current config before acquiring runtime lock
    let container_name = current_config.general.container_name.clone();

    let is_running = {
        let runtime = state.runtime.lock().await;
        if let Some(ref rt) = *runtime {
            let rt = Arc::clone(rt);
            drop(runtime);
            rt.container_info(&container_name)
                .await
                .map(|info| info.status == ContainerStatus::Running)
                .unwrap_or(false)
        } else {
            false
        }
    };

    {
        let mut current = state.config.lock().await;
        *current = config.clone();
    }
    if orb_changed {
        crate::orb_window::arm_from_config(&app, &config.orb);
    }
    refresh_tray_menu(&app).await;
    emit_hotkey_mappings_updated(&app);

    if is_running && config_requires_container_restart(&current_config, &config) {
        let _ = app.emit("config-changed-restart-needed", ());
    }

    Ok(())
}

#[tauri::command]
pub async fn restart_with_new_config(
    app: AppHandle,
    mut config: MagicianDesktopConfig,
) -> Result<(), String> {
    normalize_config(&mut config);
    config.validate_engine_base_url()?;
    crate::overlay::validate_overlay_shortcut(&config.general.quick_overlay_shortcut)?;
    validate_voice_shortcuts(&config)?;
    validate_orb_shortcut_collisions(&config)?;
    // Screen-capture chords (empty = disabled; non-empty must parse).
    crate::screen_ask::validate_screen_ask_shortcut(&config.general.screen_ask_shortcut)?;
    crate::screen_ask::validate_screen_ask_shortcut(&config.general.screen_clip_shortcut)?;
    crate::screen_ask::validate_screen_ask_shortcut(&config.general.screen_region_shortcut)?;
    crate::screen_ask::validate_screen_ask_shortcut(&config.general.screen_watch_shortcut)?;

    let state = app.state::<AppState>();
    let current_config = state.config.lock().await.clone();
    preserve_runtime_orb_placement(&mut config, &current_config);
    preserve_shared_voice_mode_mirror(&mut config, &current_config);
    let old_shortcut = current_config.general.quick_overlay_shortcut.clone();
    let new_shortcut = config.general.quick_overlay_shortcut.clone();
    let shortcut_changed = old_shortcut != new_shortcut;
    let voice_changed = current_config.voice != config.voice;
    // The overlay double-tap gesture lives in `general` but is part of the same
    // CGEventTap spec, so an overlay-gesture-only change must also re-sync it.
    let overlay_gesture_changed =
        current_config.general.quick_overlay_gesture != config.general.quick_overlay_gesture;
    let gestures_changed = voice_changed || overlay_gesture_changed;
    let orb_changed = current_config.orb != config.orb;
    let old_orb_shortcut = current_config.orb.hotkey.clone();
    let orb_shortcut_changed = old_orb_shortcut != config.orb.hotkey;
    let launch_at_login_changed =
        current_config.general.launch_at_login != config.general.launch_at_login;
    let engine_changed = current_config.engine_base_url() != config.engine_base_url();

    if shortcut_changed {
        crate::overlay::sync_overlay_shortcut(&app, &new_shortcut).await?;
    }
    if gestures_changed {
        if let Err(error) = crate::voice_note::sync_voice_shortcuts(&app, &config).await {
            if shortcut_changed {
                let _ = crate::overlay::sync_overlay_shortcut(&app, &old_shortcut).await;
            }
            return Err(error);
        }
    }
    if orb_shortcut_changed {
        if let Err(error) = crate::orb_window::sync_orb_shortcut(&app, &config.orb.hotkey) {
            if shortcut_changed {
                let _ = crate::overlay::sync_overlay_shortcut(&app, &old_shortcut).await;
            }
            if gestures_changed {
                let _ = crate::voice_note::sync_voice_shortcuts(&app, &current_config).await;
            }
            return Err(error);
        }
    }
    if launch_at_login_changed {
        if let Err(error) = apply_launch_at_login(&app, config.general.launch_at_login) {
            if shortcut_changed {
                let _ = crate::overlay::sync_overlay_shortcut(&app, &old_shortcut).await;
            }
            if gestures_changed {
                let _ = crate::voice_note::sync_voice_shortcuts(&app, &current_config).await;
            }
            if orb_shortcut_changed {
                let _ = crate::orb_window::sync_orb_shortcut(&app, &old_orb_shortcut);
            }
            return Err(error);
        }
    }

    if let Err(error) = save_config_to_disk(&config) {
        if shortcut_changed {
            let _ = crate::overlay::sync_overlay_shortcut(&app, &old_shortcut).await;
        }
        if gestures_changed {
            let _ = crate::voice_note::sync_voice_shortcuts(&app, &current_config).await;
        }
        if orb_shortcut_changed {
            let _ = crate::orb_window::sync_orb_shortcut(&app, &old_orb_shortcut);
        }
        if launch_at_login_changed {
            let _ = apply_launch_at_login(&app, current_config.general.launch_at_login);
        }
        return Err(error);
    }
    if engine_changed {
        crate::onboarding::mark_incomplete_for_engine(&config.engine_base_url())?;
    }

    let saved_orb = config.orb.clone();
    {
        let mut current = state.config.lock().await;
        *current = config;
    }
    if orb_changed {
        crate::orb_window::arm_from_config(&app, &saved_orb);
    }
    refresh_tray_menu(&app).await;
    emit_hotkey_mappings_updated(&app);

    // Applying settings is the explicit replacement path; a plain restart
    // preserves the existing container and its writable layer.
    recreate_container_inner(&app).await
}

async fn refresh_tray_menu(app: &AppHandle) {
    let update_version = app
        .state::<AppState>()
        .pending_app_update
        .lock()
        .await
        .clone();
    crate::tray::refresh_menu(app, update_version.as_deref());
}

fn validate_voice_shortcuts(config: &MagicianDesktopConfig) -> Result<(), String> {
    crate::overlay::validate_overlay_shortcut(&config.general.quick_overlay_shortcut)?;
    crate::voice_gesture::validate_voice_gestures(config)?;
    Ok(())
}

fn validate_orb_shortcut_collisions(config: &MagicianDesktopConfig) -> Result<(), String> {
    let Some(orb) = crate::orb_window::validate_orb_shortcut(&config.orb.hotkey)? else {
        return Ok(());
    };
    let candidates = [
        ("Quick Overlay", &config.general.quick_overlay_shortcut),
        ("Screen Ask", &config.general.screen_ask_shortcut),
        ("Screen Clip", &config.general.screen_clip_shortcut),
        ("Screen Region", &config.general.screen_region_shortcut),
        ("Screen Watch", &config.general.screen_watch_shortcut),
    ];
    for (label, value) in candidates {
        let normalized = crate::config::normalize_optional_shortcut(value);
        if normalized.is_empty() {
            continue;
        }
        let candidate = normalized
            .parse::<tauri_plugin_global_shortcut::Shortcut>()
            .map_err(|error| format!("Invalid {label} shortcut '{normalized}': {error}"))?;
        if candidate == orb {
            return Err(format!(
                "Orb shortcut must differ from the {label} shortcut"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod media_provider_snapshot_tests {
    use super::*;

    /// The Orb's Live engine dropdown is fed by the same `/media/providers`
    /// catalog the web and phone pickers render. The snapshot must keep the
    /// realtime rows and the backend default, tolerate older backends that
    /// send neither, and ignore fields the desktop does not model.
    #[test]
    fn snapshot_keeps_realtime_profiles_and_default() {
        let snapshot: MediaProviderSnapshot = serde_json::from_value(serde_json::json!({
            "realtime_voice_default_profile": "voice_realtime_default",
            "realtime_voice_profiles": [
                {
                    "profile_id": "voice_realtime_gemini_38_live",
                    "label": "Gemini 3.8 Live",
                    "provider": "gemini_live",
                    "model": "gemini-3.8-live",
                    "topology": "backend_proxied",
                    "mode": "assistant",
                    "available": true,
                    "voices": [{ "id": "Kore", "label": "Kore" }]
                },
                {
                    "profile_id": "voice_realtime_default",
                    "label": "GPT Realtime 2.1 Mini (Default)",
                    "provider": "openai_realtime",
                    "model": "gpt-realtime-2.1-mini",
                    "topology": "direct_peer_to_peer",
                    "mode": "assistant",
                    "available": false,
                    "unavailable_reason": "OPENAI_API_KEY env var is required"
                }
            ]
        }))
        .expect("snapshot parses");
        assert_eq!(
            snapshot.realtime_voice_default_profile.as_deref(),
            Some("voice_realtime_default")
        );
        assert_eq!(snapshot.realtime_voice_profiles.len(), 2);
        let gemini = &snapshot.realtime_voice_profiles[0];
        assert_eq!(gemini.profile_id, "voice_realtime_gemini_38_live");
        assert_eq!(gemini.topology, "backend_proxied");
        assert!(gemini.available);
        let openai = &snapshot.realtime_voice_profiles[1];
        assert!(!openai.available);
        assert_eq!(
            openai.unavailable_reason.as_deref(),
            Some("OPENAI_API_KEY env var is required")
        );

        let legacy: MediaProviderSnapshot =
            serde_json::from_value(serde_json::json!({ "tts": null })).expect("older backend");
        assert!(legacy.realtime_voice_profiles.is_empty());
        assert!(legacy.realtime_voice_default_profile.is_none());
    }
}

#[cfg(test)]
mod orb_shortcut_tests {
    use super::*;

    #[test]
    fn orb_shortcut_rejects_collisions_after_parsing() {
        let mut config = MagicianDesktopConfig::default();
        config.orb.hotkey = "Shift+Alt+S".to_string();
        config.general.screen_ask_shortcut = "shift+alt+s".to_string();
        let error = validate_orb_shortcut_collisions(&config).unwrap_err();
        assert!(error.contains("Screen Ask"));
    }

    #[test]
    fn disabled_orb_shortcut_never_collides() {
        let mut config = MagicianDesktopConfig::default();
        config.orb.hotkey = "disabled".to_string();
        assert!(validate_orb_shortcut_collisions(&config).is_ok());
    }

    #[test]
    fn retired_voice_shortcuts_do_not_block_the_orb_shortcut() {
        let mut config = MagicianDesktopConfig::default();
        config.orb.hotkey = "Cmd+Shift+Space".to_string();
        config.voice.voice_note_shortcut = "Cmd+Shift+Space".to_string();
        config.voice.live_ptt_shortcut = "Cmd+Shift+Space".to_string();

        assert!(validate_orb_shortcut_collisions(&config).is_ok());
        assert!(validate_voice_shortcuts(&config).is_ok());
    }

    #[test]
    fn backend_voice_mode_seeds_ambient_once_without_overwriting_local_choice() {
        let mut config = MagicianDesktopConfig::default();
        assert!(!seed_orb_voice_mode_if_needed(
            &mut config,
            "realtime",
            false
        ));
        assert_eq!(config.orb.voice_mode, "hands_free");
        assert!(!config.orb.voice_mode_seeded);

        assert!(seed_orb_voice_mode_if_needed(
            &mut config,
            "recording",
            true
        ));
        assert_eq!(config.orb.voice_mode, "dictation");
        assert!(config.orb.voice_mode_seeded);

        config.orb.voice_mode = "realtime".to_string();
        assert!(!seed_orb_voice_mode_if_needed(
            &mut config,
            "hands_free",
            true
        ));
        assert_eq!(config.orb.voice_mode, "realtime");
    }

    #[test]
    fn settings_snapshot_cannot_overwrite_a_newer_dragged_orb_position() {
        let mut current = MagicianDesktopConfig::default();
        current.orb.resting_x = Some(0.15);
        current.orb.resting_y = Some(0.25);
        current.orb.expanded_x = Some(0.65);
        current.orb.expanded_y = Some(0.75);

        let mut stale_settings_snapshot = MagicianDesktopConfig::default();
        preserve_runtime_orb_placement(&mut stale_settings_snapshot, &current);

        assert_eq!(stale_settings_snapshot.orb.resting_x, Some(0.15));
        assert_eq!(stale_settings_snapshot.orb.resting_y, Some(0.25));
        assert_eq!(stale_settings_snapshot.orb.expanded_x, Some(0.65));
        assert_eq!(stale_settings_snapshot.orb.expanded_y, Some(0.75));
    }

    #[test]
    fn settings_snapshot_keeps_the_live_shared_voice_mode() {
        let mut current = MagicianDesktopConfig::default();
        current.voice.voice_mode = "realtime".to_string();
        let mut stale_settings_snapshot = MagicianDesktopConfig::default();
        stale_settings_snapshot.voice.voice_mode = "recording".to_string();
        stale_settings_snapshot.voice.default_thread_id = "thread-from-the-form".to_string();

        preserve_shared_voice_mode_mirror(&mut stale_settings_snapshot, &current);

        assert_eq!(stale_settings_snapshot.voice.voice_mode, "realtime");
        assert_eq!(
            stale_settings_snapshot.voice.default_thread_id,
            "thread-from-the-form"
        );
    }
}

#[tauri::command]
pub async fn get_logs(app: AppHandle, lines: Option<usize>) -> Result<String, String> {
    let lines = lines.unwrap_or(100);
    let state = app.state::<AppState>();
    let config = state.config.lock().await.clone();
    let runtime = state.runtime.lock().await;

    if let Some(ref rt) = *runtime {
        let rt = Arc::clone(rt);
        drop(runtime);
        rt.logs(&config.general.container_name, lines).await
    } else {
        Err("No runtime available".to_string())
    }
}

#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> Result<UpdateState, String> {
    check_for_updates_inner(&app).await
}

#[tauri::command]
pub async fn perform_update(app: AppHandle, update_type: String) -> Result<(), String> {
    match update_type.as_str() {
        "container" => {
            let state = app.state::<AppState>();
            let config = state.config.lock().await.clone();
            let runtime = state.runtime.lock().await;

            if let Some(ref rt) = *runtime {
                let rt_clone = Arc::clone(rt);
                drop(runtime);
                perform_container_update(&app, rt_clone, &config).await
            } else {
                Err("No runtime available".to_string())
            }
        },
        "app" => crate::updater::perform_app_update(&app).await,
        "all" => {
            let state = app.state::<AppState>();
            let config = state.config.lock().await.clone();
            let runtime = state.runtime.lock().await;
            if let Some(ref rt) = *runtime {
                let rt_clone = Arc::clone(rt);
                drop(runtime);
                crate::updater::perform_combined_update(&app, rt_clone, &config).await
            } else {
                Err("No runtime available".to_string())
            }
        },
        _ => Err(format!("Unknown update type: {}", update_type)),
    }
}

#[tauri::command]
pub async fn perform_app_update(app: AppHandle) -> Result<(), String> {
    crate::updater::perform_app_update(&app).await
}

#[tauri::command]
pub async fn uninstall(app: AppHandle, mode: String) -> Result<String, String> {
    let result = crate::cleanup::run_cleanup(&app, &mode).await?;

    // Signal that the app needs re-setup (container/image removed)
    let _ = app.emit("needs-setup", ());
    crate::tray::update_tray_state(&app, crate::tray::TrayState::Stopped);

    Ok(result)
}

#[tauri::command]
pub async fn open_setup(app: AppHandle, mode: Option<String>) -> Result<(), String> {
    // Close settings window if open
    if let Some(window) = app.get_webview_window("settings") {
        let _ = window.close();
    }

    crate::setup::show_setup_window_mode(&app, mode.as_deref())
}

#[tauri::command]
pub async fn approve_setup(
    app: AppHandle,
    selection: crate::setup::SetupSelection,
    replace_existing_container: Option<bool>,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let current_config = state.config.lock().await.clone();
    let config = crate::setup::configured_for_selection(&current_config, &selection)?;
    if !matches!(
        selection.placement,
        crate::setup::SetupPlacement::ManagedContainer
            | crate::setup::SetupPlacement::NativeInstall
    ) {
        return Err("Setup approval requires an installable local backend".to_string());
    }

    // Approval is the point at which the selected local root becomes durable
    // and machine changes may begin.
    save_config_to_disk(&config)?;
    apply_launch_at_login(&app, config.general.launch_at_login)?;
    *state.config.lock().await = config.clone();
    crate::onboarding::mark_incomplete_for_engine(&config.engine_base_url())?;

    if matches!(
        selection.placement,
        crate::setup::SetupPlacement::NativeInstall
    ) {
        crate::setup::execute_native_setup(&app, &config).await?;
        crate::tray::update_tray_state(&app, crate::tray::TrayState::Running);
        return Ok(());
    }

    let runtime = state.runtime.lock().await;

    if let Some(ref rt) = *runtime {
        let rt_clone = Arc::clone(rt);
        drop(runtime);
        crate::setup::execute_setup(
            &app,
            rt_clone,
            &config,
            replace_existing_container.unwrap_or(false),
        )
        .await?;

        // Update tray state to running after successful setup
        crate::tray::update_tray_state(&app, crate::tray::TrayState::Running);
        Ok(())
    } else {
        Err("No runtime detected".to_string())
    }
}

#[tauri::command]
pub async fn check_ports(app: AppHandle) -> Result<crate::port_check::PortCheckResult, String> {
    let state = app.state::<AppState>();
    let config = state.config.lock().await.clone();
    let container_config = ContainerConfig::from_desktop_config(&config);
    Ok(
        crate::port_check::check_ports(&container_config.ports, &config.general.container_name)
            .await,
    )
}

#[tauri::command]
pub async fn free_ports(app: AppHandle, pids: Vec<u32>) -> Result<Vec<u16>, String> {
    let state = app.state::<AppState>();
    let config = state.config.lock().await.clone();
    let container_config = ContainerConfig::from_desktop_config(&config);

    // Build conflict list from provided PIDs
    let conflicts: Vec<crate::port_check::PortConflict> = pids
        .into_iter()
        .zip(container_config.ports.iter().map(|(h, _)| *h))
        .map(|(pid, port)| crate::port_check::PortConflict {
            port,
            pid: Some(pid),
            process_name: None,
            is_own_container: false,
        })
        .collect();

    let still_blocked = crate::port_check::free_conflicted_ports(&conflicts).await;
    Ok(still_blocked)
}

fn config_requires_container_restart(
    current: &MagicianDesktopConfig,
    next: &MagicianDesktopConfig,
) -> bool {
    current.general.container_image != next.general.container_image
        || current.general.container_name != next.general.container_name
        || current.general.runtime_root != next.general.runtime_root
        || current.container != next.container
        || current.network != next.network
        || current.host_gateway.port != next.host_gateway.port
        || current.host_gateway.runtime_url != next.host_gateway.runtime_url
        || current.host_gateway.local_url != next.host_gateway.local_url
        || current.host_gateway.enabled != next.host_gateway.enabled
        || current.api_keys != next.api_keys
}

pub fn apply_launch_at_login(app: &AppHandle, enabled: bool) -> Result<(), String> {
    if autostart_plugin_disabled_for_local_run() {
        tracing::info!(
            "Skipping launch-at-login sync because desktop autostart plugin is disabled for local external-runtime mode"
        );
        return Ok(());
    }
    let manager = app.autolaunch();
    if enabled {
        manager
            .enable()
            .map_err(|e| format!("Failed to enable launch at login: {}", e))
    } else {
        manager
            .disable()
            .map_err(|e| format!("Failed to disable launch at login: {}", e))
    }
}

fn autostart_plugin_disabled_for_local_run() -> bool {
    !runtime_stack_managed_from_env().unwrap_or(true)
}

fn runtime_stack_managed(config: &MagicianDesktopConfig) -> bool {
    config.should_manage_runtime_stack(
        runtime_stack_managed_from_env().unwrap_or(config.general.manage_runtime_stack),
    )
}

fn runtime_stack_managed_from_env() -> Option<bool> {
    match std::env::var("MAGICIAN_DESKTOP_MANAGE_RUNTIME") {
        Ok(value) => Some(!matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )),
        Err(_) => None,
    }
}

// --- Internal helpers (used by both commands and tray menu) ---

pub async fn start_container_inner(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let config = state.config.lock().await.clone();
    if cfg!(target_os = "macos")
        && crate::native_runtime::installed_by_desktop()
        && !config.is_remote_engine()
    {
        crate::tray::update_tray_state(app, crate::tray::TrayState::Starting);
        crate::native_runtime::ensure_service_started().await?;
        crate::tray::update_tray_state(app, crate::tray::TrayState::Running);
        return Ok(());
    }
    let runtime = state.runtime.lock().await;

    if let Some(ref rt) = *runtime {
        let rt = Arc::clone(rt);
        drop(runtime);

        crate::tray::update_tray_state(app, crate::tray::TrayState::Starting);

        match rt
            .container_info(&config.general.container_name)
            .await?
            .status
        {
            ContainerStatus::Running => return Ok(()),
            ContainerStatus::Stopped => {
                rt.start_existing(&config.general.container_name).await?;
                crate::tray::update_tray_state(app, crate::tray::TrayState::Running);
                return Ok(());
            },
            ContainerStatus::Restarting => {
                return Err("Container is already restarting".to_string())
            },
            ContainerStatus::NotFound => {},
        }

        let container_config = ContainerConfig::from_desktop_config(&config);

        // Pre-flight port check
        let port_result =
            crate::port_check::check_ports(&container_config.ports, &config.general.container_name)
                .await;

        if !port_result.all_clear() {
            // Try to auto-resolve: kill own stale processes (docker-proxy, etc.)
            let own_conflicts: Vec<_> = port_result
                .conflicts
                .iter()
                .filter(|c| c.is_own_container)
                .cloned()
                .collect();
            let foreign_conflicts: Vec<_> = port_result
                .conflicts
                .iter()
                .filter(|c| !c.is_own_container)
                .cloned()
                .collect();

            if !own_conflicts.is_empty() {
                tracing::info!(
                    "Auto-freeing {} port(s) held by stale backend processes",
                    own_conflicts.len()
                );
                let still_blocked = crate::port_check::free_conflicted_ports(&own_conflicts).await;
                if !still_blocked.is_empty() {
                    let _ = app.emit("port-conflict", &port_result);
                    crate::tray::update_tray_state(app, crate::tray::TrayState::Stopped);
                    return Err(format!(
                        "Port(s) {} still in use after cleanup. Stop the conflicting process or change ports in Settings.",
                        still_blocked
                            .iter()
                            .map(|p| p.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }

            if !foreign_conflicts.is_empty() {
                let _ = app.emit("port-conflict", &port_result);
                crate::tray::update_tray_state(app, crate::tray::TrayState::Stopped);
                let detail: Vec<String> = foreign_conflicts
                    .iter()
                    .map(|c| {
                        let who = c.process_name.as_deref().unwrap_or("unknown process");
                        match c.pid {
                            Some(pid) => format!("port {} used by {} (pid {})", c.port, who, pid),
                            None => format!("port {} used by {}", c.port, who),
                        }
                    })
                    .collect();
                return Err(format!(
                    "Cannot start: {}. Free the port(s) or change them in Settings.",
                    detail.join("; ")
                ));
            }
        }

        rt.start(&container_config).await?;

        crate::tray::update_tray_state(app, crate::tray::TrayState::Running);

        Ok(())
    } else {
        Err("No runtime available".to_string())
    }
}

pub async fn stop_container_inner(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let config = state.config.lock().await.clone();
    if cfg!(target_os = "macos")
        && crate::native_runtime::installed_by_desktop()
        && !config.is_remote_engine()
    {
        crate::native_runtime::stop_service().await?;
        crate::tray::update_tray_state(app, crate::tray::TrayState::Stopped);
        return Ok(());
    }
    let runtime = state.runtime.lock().await;

    if let Some(ref rt) = *runtime {
        let rt = Arc::clone(rt);
        drop(runtime);

        rt.stop(&config.general.container_name).await?;
        crate::tray::update_tray_state(app, crate::tray::TrayState::Stopped);
        Ok(())
    } else {
        Err("No runtime available".to_string())
    }
}

pub async fn restart_container_inner(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let config = state.config.lock().await.clone();
    if cfg!(target_os = "macos")
        && crate::native_runtime::installed_by_desktop()
        && !config.is_remote_engine()
    {
        crate::tray::update_tray_state(app, crate::tray::TrayState::Starting);
        crate::native_runtime::restart_service().await?;
        crate::tray::update_tray_state(app, crate::tray::TrayState::Running);
        return Ok(());
    }
    let name = config.general.container_name;
    let runtime = state
        .runtime
        .lock()
        .await
        .clone()
        .ok_or_else(|| "No runtime available".to_string())?;
    crate::tray::update_tray_state(app, crate::tray::TrayState::Starting);
    runtime.restart_existing(&name).await?;
    crate::tray::update_tray_state(app, crate::tray::TrayState::Running);
    Ok(())
}

async fn recreate_container_inner(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let config = state.config.lock().await.clone();
    let runtime = state.runtime.lock().await;

    if let Some(ref rt) = *runtime {
        let rt = Arc::clone(rt);
        drop(runtime);

        let container_config = ContainerConfig::from_desktop_config(&config);
        rt.prepare_keyring(&container_config).await?;

        crate::tray::update_tray_state(app, crate::tray::TrayState::Starting);
        let name = &config.general.container_name;

        let info = rt.container_info(name).await?;
        if info.status == ContainerStatus::Running {
            rt.stop(name).await?;
        }
        if info.status != ContainerStatus::NotFound {
            rt.remove(name).await?;
        }

        // Brief wait for ports to be released after container stop
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Port check after stop — ports may linger from non-clean shutdown
        let port_result =
            crate::port_check::check_ports(&container_config.ports, &config.general.container_name)
                .await;

        if !port_result.all_clear() {
            // Auto-free any stale processes
            let still_blocked =
                crate::port_check::free_conflicted_ports(&port_result.conflicts).await;
            if !still_blocked.is_empty() {
                let _ = app.emit("port-conflict", &port_result);
                crate::tray::update_tray_state(app, crate::tray::TrayState::Stopped);
                return Err(format!(
                    "Port(s) {} still in use after cleanup. Stop the conflicting process or change ports in Settings.",
                    still_blocked
                        .iter()
                        .map(|p| p.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }

        rt.start(&container_config).await?;

        crate::tray::update_tray_state(app, crate::tray::TrayState::Running);

        Ok(())
    } else {
        Err("No runtime available".to_string())
    }
}

pub async fn check_for_updates_inner(app: &AppHandle) -> Result<UpdateState, String> {
    let state = app.state::<AppState>();
    let config = state.config.lock().await.clone();
    let runtime = state.runtime.lock().await;

    if let Some(ref rt) = *runtime {
        let rt = Arc::clone(rt);
        drop(runtime);
        check_updates_impl(app, rt.as_ref(), &config).await
    } else {
        Err("No runtime available".to_string())
    }
}
