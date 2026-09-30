use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::info;

// The quick overlay is summoned by a double-tap of Left Option
// (`DEFAULT_OVERLAY_GESTURE`, handled by the native CGEventTap in
// `voice_gesture.rs`). The global-shortcut *chord* is an optional fallback and
// is empty by default: the old `Cmd+Z` default collided with the system-wide
// Undo across every app. Leave the chord empty unless a user opts into a
// non-conflicting one.
pub const DEFAULT_OVERLAY_SHORTCUT: &str = "";
pub const DEFAULT_OVERLAY_GESTURE: &str = "Double Left Option";
// Screen capture-and-ask: ⇧⌥S captures the main display, then summons the HUD
// with the shot staged for a question. Unlike the overlay chord this defaults
// ON — the shortcut IS the feature's only trigger (no gesture twin), and
// Shift+Option+S doesn't collide with a system-wide default. Set to
// `none`/`disabled` in config to turn it off.
pub const DEFAULT_SCREEN_ASK_SHORTCUT: &str = "shift+alt+s";
// Clip twin of the screen-ask chord: press starts a screen recording, press
// again (or the backend's 30s cap) stops it and opens the HUD with the clip
// staged. Same opt-out semantics.
pub const DEFAULT_SCREEN_CLIP_SHORTCUT: &str = "shift+alt+r";
// Region twin: macOS's native interactive picker first (drag = region,
// spacebar = window-pick), then the same capture-and-ask flow. Escape from
// the picker cancels silently. Same opt-out semantics.
pub const DEFAULT_SCREEN_REGION_SHORTCUT: &str = "shift+alt+a";
// Continuous observation toggle: press starts a notes-mode "watch my
// screen" session (narration into the screen-watch thread), press again
// stops it and opens the HUD on the session. Same opt-out semantics.
pub const DEFAULT_SCREEN_WATCH_SHORTCUT: &str = "shift+alt+w";
pub const DEFAULT_VOICE_NOTE_SHORTCUT: &str = "";
pub const DEFAULT_LIVE_PTT_SHORTCUT: &str = "";
// `voice_note_gesture` is the single push-to-talk hold. It is mode-aware: while
// held it drives a live voice turn or a dictation take depending on the web's
// universal Call/Dictate switch (mirrored to the tray via `set_ptt_mode`).
// Default is the Left-Control + Left-Option chord (not a bare modifier), which
// avoids shadowing ⌥-key typing.
pub const DEFAULT_VOICE_NOTE_GESTURE: &str = "Hold Left Control+Left Option";
pub const DEFAULT_VOICE_THREAD_ID: &str = "general";
pub const DEFAULT_LIVE_PTT_REALTIME_PROFILE: &str = "voice_realtime_openai_backend";
pub const DEFAULT_ORB_SHORTCUT: &str = "Alt+Space";
pub const DEFAULT_ORB_LEASH_MINUTES: u64 = 120;
pub const DEFAULT_ORB_FOLLOW_UP_SECONDS: u64 = 8;

/// Desktop app configuration, stored in ~/magician_data_v3/config/magician.toml
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MagicianDesktopConfig {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub contextual_assist: ContextualAssistConfig,
    #[serde(default)]
    pub container: ContainerResourceConfig,
    #[serde(default)]
    pub network: NetworkConfig,
    #[serde(default)]
    pub host_gateway: HostGatewayConfig,
    #[serde(default)]
    pub voice: VoiceConfig,
    #[serde(default)]
    pub orb: OrbConfig,
    #[serde(default)]
    pub api_keys: ApiKeysConfig,
    #[serde(default)]
    pub updates: UpdateConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextualAssistConfig {
    /// Master switch for the host-native contextual writing/task assist chip.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Show the passive chip when the user has selected text.
    #[serde(default = "default_true")]
    pub show_on_selected_text: bool,
    /// Show the passive chip when a writable field is focused.
    #[serde(default = "default_true")]
    pub show_in_writable_fields: bool,
    /// Keep the feature globally invokable but suppress ambient chips.
    #[serde(default)]
    pub explicit_hotkey_only: bool,
    /// Selected personality for contextual drafts. `active` follows the current
    /// active assistant personality rather than locking a separate writing style.
    #[serde(default = "default_contextual_assist_personality")]
    pub default_personality: String,
    /// Basic per-app suppression list. Native app matching lands in a later slice.
    #[serde(default)]
    pub excluded_apps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceConfig {
    /// Retired global shortcut for voice-note mode. Retained so older desktop
    /// TOML files continue to deserialize without losing their shape.
    #[serde(default = "default_voice_note_shortcut")]
    pub voice_note_shortcut: String,
    /// Retired macOS-native voice-note gesture compatibility value.
    #[serde(default = "default_voice_note_gesture")]
    pub voice_note_gesture: String,
    /// Retired global live push-to-talk shortcut compatibility value.
    #[serde(default = "default_live_ptt_shortcut")]
    pub live_ptt_shortcut: String,
    /// Default chat thread used when no active desktop chat thread is known.
    #[serde(default = "default_voice_thread_id")]
    pub default_thread_id: String,
    /// Realtime voice profile used by host-native live PTT. Defaults
    /// to a backend-proxied profile so native PTT never accidentally
    /// tries to use the browser-only OpenAI WebRTC path.
    #[serde(default = "default_live_ptt_realtime_profile")]
    pub live_ptt_realtime_profile: String,
    /// Whether raw voice-note audio may be retained as an artifact.
    /// Defaults false so Phase 1 remains transcript-first.
    #[serde(default)]
    pub retain_voice_note_audio: bool,
    /// Mute host-native assistant audio playback while keeping transcripts and
    /// mascot bubbles visible.
    #[serde(default)]
    pub output_muted: bool,
    /// Push-to-talk voice mode last applied from the backend media preference:
    /// `recording` (Dictation, default) or `realtime` (Live voice). This is a
    /// LOCAL startup cache, not the source of truth — the backend's
    /// `media/preferences.json` stays canonical. The tray seeds `ptt_mode` from
    /// this at launch so the menu paints the saved mode on the FIRST frame,
    /// without waiting on (or racing) the backend coming up.
    #[serde(default = "default_voice_mode")]
    pub voice_mode: String,
}

/// Host-owned ambient voice surface. These values live with the desktop app,
/// because they govern native wake capture, global shortcuts, and AppKit window
/// behavior rather than the container runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrbConfig {
    /// Whether the visible Orb lifecycle is currently armed. Turning this off
    /// hides the Orb but does not disable an independently enabled wake spotter.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Device-local native wake-word switch. Off by default: the Orb is
    /// press-to-talk (hold Left Option) and does not listen until asked. When
    /// enabled, the spotter remains available while the Orb is hidden/off and
    /// a valid phrase cold-starts the Orb before admitting the conversation.
    #[serde(default = "default_false")]
    pub wake_enabled: bool,
    /// Device-local conversation engine used after a wake hit. Kept separate
    /// from `voice.voice_mode` so changing Chat/PTT never changes the ambient
    /// listener. Supports `dictation`, `hands_free`, and `realtime`.
    #[serde(default = "default_orb_voice_mode")]
    pub voice_mode: String,
    /// Whether the device-local ambient choice has been initialized. Until it
    /// is, the first authoritative backend media-preference snapshot seeds the
    /// choice; later backend changes never overwrite the local selection.
    #[serde(default)]
    pub voice_mode_seeded: bool,
    /// Global chord that expands/collapses the orb panel.
    #[serde(default = "default_orb_shortcut")]
    pub hotkey: String,
    /// Hard cap on an unattended armed window.
    #[serde(default = "default_orb_leash_minutes")]
    pub leash_minutes: u64,
    /// Follow-up window after a conversation disconnects.
    #[serde(default = "default_orb_follow_up_seconds")]
    pub follow_up_seconds: u64,
    /// Native Vosk phrases. The first phrase is the current detector phrase;
    /// retaining a list keeps the settings contract ready for multi-phrase Vosk.
    #[serde(default = "default_orb_wake_phrases")]
    pub wake_phrases: Vec<String>,
    /// Whether the listener may remain armed while running on battery.
    #[serde(default = "default_true")]
    pub armed_on_battery: bool,
    /// High-salience wake moments arrive at center screen, then settle home.
    #[serde(default = "default_true")]
    pub auto_expand_on_wake: bool,
    /// Normalized device-local parking position for the compact orb. `None`
    /// docks into the hardware notch or notchless software-notch fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resting_x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resting_y: Option<f64>,
    /// Normalized device-local parking position for the expanded orb. `None`
    /// grows downward from the dock; detached placement remains independent so
    /// expanding does not overwrite where the compact orb lives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded_x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded_y: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeneralConfig {
    /// Launch on system startup
    #[serde(default = "default_true")]
    pub launch_at_login: bool,
    /// Show notifications for status changes
    #[serde(default = "default_true")]
    pub notifications_enabled: bool,
    /// Prevent mac from sleeping while app is open
    #[serde(default = "default_true")]
    pub prevent_sleep: bool,
    /// Container image to use
    #[serde(default = "default_image")]
    pub container_image: String,
    /// Container name
    #[serde(default = "default_container_name")]
    pub container_name: String,
    /// Whether the desktop app owns the runtime stack lifecycle.
    #[serde(default = "default_manage_runtime_stack")]
    pub manage_runtime_stack: bool,
    /// Host directory used by a desktop-managed local engine. `None` keeps the
    /// project-wide `~/MagicianNotes` default (or an explicit process
    /// `MAGICIAN_ROOT_DIR`). Remote engines never read this directory.
    #[serde(default)]
    pub runtime_root: Option<String>,
    /// Optional global-shortcut chord that summons the quick overlay. Empty by
    /// default — the primary trigger is `quick_overlay_gesture`.
    #[serde(default = "default_overlay_shortcut")]
    pub quick_overlay_shortcut: String,
    /// macOS-native gesture that summons the quick overlay (default
    /// `Double Left Option`). Handled by the `voice_gesture.rs` CGEventTap.
    #[serde(default = "default_overlay_gesture")]
    pub quick_overlay_gesture: String,
    /// Global chord for screen capture-and-ask (default `shift+alt+s`):
    /// captures the main display, then summons the HUD with the shot staged.
    /// `none`/`disabled` turns it off.
    #[serde(default = "default_screen_ask_shortcut")]
    pub screen_ask_shortcut: String,
    /// Global chord for clip capture-and-ask (default `shift+alt+r`):
    /// press to start recording, press again to stop and ask.
    /// `none`/`disabled` turns it off.
    #[serde(default = "default_screen_clip_shortcut")]
    pub screen_clip_shortcut: String,
    /// Global chord for region/window capture-and-ask (default
    /// `shift+alt+a`): the native macOS picker selects the area, then the
    /// HUD opens with the selection staged. `none`/`disabled` turns it off.
    #[serde(default = "default_screen_region_shortcut")]
    pub screen_region_shortcut: String,
    /// Global chord toggling continuous screen observation (default
    /// `shift+alt+w`): press to start a notes-mode session, press again to
    /// stop and review in the HUD. `none`/`disabled` turns it off.
    #[serde(default = "default_screen_watch_shortcut")]
    pub screen_watch_shortcut: String,
    /// Pre-warm the hidden HUD WebView a few seconds after boot so the first
    /// summon is warm. Costs one resident parked WebKit process; `false`
    /// restores fully lazy first-use.
    #[serde(default = "default_true")]
    pub hud_prewarm: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContainerResourceConfig {
    /// CPU cores allocated to the container
    #[serde(default = "default_cpu_limit")]
    pub cpu_cores: f64,
    /// Memory limit in GB
    #[serde(default = "default_memory_gb")]
    pub memory_gb: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Magician API port
    #[serde(default = "default_magician_port")]
    pub magician_port: u16,
    /// Magicutor port
    #[serde(default = "default_magicutor_port")]
    pub magicutor_port: u16,
    /// Optional override for the Magician engine origin. `None` derives
    /// `http://127.0.0.1:{magician_port}` (current local behavior). Track A
    /// does not expose a Settings control; tests and Task 16B set this field.
    #[serde(default)]
    pub engine_base_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostGatewayConfig {
    /// Expose a host-native control gateway for Magician runtime services.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Local interface the gateway listens on.
    #[serde(default = "default_host_gateway_bind_host")]
    pub bind_host: String,
    /// Local gateway port on the host.
    #[serde(default = "default_host_gateway_port")]
    pub port: u16,
    /// URL runtime services should use to call back into this host gateway.
    #[serde(default = "default_host_gateway_runtime_url")]
    pub runtime_url: String,
    /// URL the tray app uses locally for self-description and diagnostics.
    #[serde(default = "default_host_gateway_local_url")]
    pub local_url: String,
    /// URL the native host child should use for the browser UI.
    #[serde(default = "default_ui_url")]
    pub ui_url: String,
    /// Launch the macOS presence host after Magician is reachable.
    #[serde(default = "default_true")]
    pub macos_presence_host_enabled: bool,
    /// Mascot follows the draw-overlay storyboard: when the copilot/tutor
    /// draws a step on screen, the orb glides beside the highlighted shape
    /// (presence plan Phase 7 embodiment) and docks again when the overlay
    /// clears. Disable to keep the orb wherever the user parked it.
    #[serde(default = "default_true")]
    pub mascot_follows_draw: bool,
    /// User explicitly stopped the mascot (tray "Stop Mascot Host"). Persisted so
    /// the stop survives an app relaunch — autostart and chat/voice control events
    /// honor it. Cleared when the user explicitly Starts/Restarts the mascot.
    #[serde(default)]
    pub macos_presence_host_user_disabled: bool,
    /// Optional explicit binary path for the macOS presence host.
    #[serde(default)]
    pub macos_presence_host_bin: String,
    /// Optional explicit binary path for the macOS Speech transcription helper.
    #[serde(default)]
    pub macos_speech_helper_bin: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiKeysConfig {
    #[serde(default)]
    pub openai_api_key: String,
    #[serde(default)]
    pub anthropic_api_key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateConfig {
    /// Automatically check for updates
    #[serde(default = "default_true")]
    pub auto_check: bool,
    /// Automatically apply container image updates
    #[serde(default)]
    pub auto_update_container: bool,
}

// Default value helpers
fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

fn default_manage_runtime_stack() -> bool {
    true
}
fn default_image() -> String {
    "ghcr.io/magicbeanbs100x/magician:latest".to_string()
}
fn default_container_name() -> String {
    "magician".to_string()
}
pub fn default_overlay_shortcut() -> String {
    DEFAULT_OVERLAY_SHORTCUT.to_string()
}

pub fn default_orb_shortcut() -> String {
    DEFAULT_ORB_SHORTCUT.to_string()
}

fn default_orb_leash_minutes() -> u64 {
    DEFAULT_ORB_LEASH_MINUTES
}

fn default_orb_follow_up_seconds() -> u64 {
    DEFAULT_ORB_FOLLOW_UP_SECONDS
}

fn default_orb_wake_phrases() -> Vec<String> {
    vec!["hey assistant".to_string()]
}

fn default_orb_voice_mode() -> String {
    "hands_free".to_string()
}

pub fn normalize_orb_voice_mode(mode: &str) -> String {
    match mode.trim().to_ascii_lowercase().as_str() {
        "realtime" | "live" => "realtime".to_string(),
        "recording" | "dictation" | "dictate" => "dictation".to_string(),
        _ => default_orb_voice_mode(),
    }
}

pub fn default_overlay_gesture() -> String {
    DEFAULT_OVERLAY_GESTURE.to_string()
}
pub fn default_screen_ask_shortcut() -> String {
    DEFAULT_SCREEN_ASK_SHORTCUT.to_string()
}
pub fn default_screen_clip_shortcut() -> String {
    DEFAULT_SCREEN_CLIP_SHORTCUT.to_string()
}
pub fn default_screen_region_shortcut() -> String {
    DEFAULT_SCREEN_REGION_SHORTCUT.to_string()
}
pub fn default_screen_watch_shortcut() -> String {
    DEFAULT_SCREEN_WATCH_SHORTCUT.to_string()
}
pub fn default_voice_note_shortcut() -> String {
    DEFAULT_VOICE_NOTE_SHORTCUT.to_string()
}
pub fn default_live_ptt_shortcut() -> String {
    DEFAULT_LIVE_PTT_SHORTCUT.to_string()
}
pub fn default_voice_note_gesture() -> String {
    DEFAULT_VOICE_NOTE_GESTURE.to_string()
}
pub fn default_voice_thread_id() -> String {
    DEFAULT_VOICE_THREAD_ID.to_string()
}
pub fn default_live_ptt_realtime_profile() -> String {
    DEFAULT_LIVE_PTT_REALTIME_PROFILE.to_string()
}
/// Default push-to-talk voice mode when none is persisted: Dictation. A fresh
/// install must not default to a live mic; this also matches the backend's
/// canonical default (`media_rails::preferences::default_voice_mode`).
pub fn default_voice_mode() -> String {
    "recording".to_string()
}
pub fn default_contextual_assist_personality() -> String {
    "active".to_string()
}
fn default_cpu_limit() -> f64 {
    2.0
}
fn default_memory_gb() -> u32 {
    4
}
fn default_magician_port() -> u16 {
    3002
}
fn default_magicutor_port() -> u16 {
    3003
}
fn default_host_gateway_bind_host() -> String {
    "127.0.0.1".to_string()
}
fn default_host_gateway_port() -> u16 {
    3017
}
fn default_host_gateway_runtime_url() -> String {
    "http://127.0.0.1:3017".to_string()
}
fn default_host_gateway_local_url() -> String {
    "http://127.0.0.1:3017".to_string()
}
fn default_ui_url() -> String {
    "http://127.0.0.1:5173".to_string()
}

pub fn normalize_shortcut(shortcut: &str, default_value: &str) -> String {
    let trimmed = shortcut.trim();
    if trimmed.is_empty() {
        default_value.to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn normalize_optional_shortcut(shortcut: &str) -> String {
    let trimmed = shortcut.trim();
    if trimmed.eq_ignore_ascii_case("none") || trimmed.eq_ignore_ascii_case("disabled") {
        String::new()
    } else {
        trimmed.to_string()
    }
}

pub fn normalize_gesture(gesture: &str, default_value: &str) -> String {
    let trimmed = gesture.trim();
    if trimmed.is_empty() {
        default_value.to_string()
    } else if trimmed.eq_ignore_ascii_case("none") {
        "Disabled".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn normalize_overlay_shortcut(shortcut: &str) -> String {
    // Optional: empty / none / disabled all mean "no chord" (the overlay is
    // summoned by `quick_overlay_gesture` instead).
    normalize_optional_shortcut(shortcut)
}

pub fn normalize_config(config: &mut MagicianDesktopConfig) {
    if let Some(root) = config.general.runtime_root.as_mut() {
        let trimmed = root.trim().to_string();
        if trimmed.is_empty() {
            config.general.runtime_root = None;
        } else {
            *root = trimmed;
        }
    }
    config.general.quick_overlay_shortcut =
        normalize_overlay_shortcut(&config.general.quick_overlay_shortcut);
    config.general.screen_ask_shortcut =
        normalize_optional_shortcut(&config.general.screen_ask_shortcut);
    config.general.screen_clip_shortcut =
        normalize_optional_shortcut(&config.general.screen_clip_shortcut);
    config.general.screen_region_shortcut =
        normalize_optional_shortcut(&config.general.screen_region_shortcut);
    config.general.screen_watch_shortcut =
        normalize_optional_shortcut(&config.general.screen_watch_shortcut);
    config.voice.voice_note_shortcut =
        normalize_optional_shortcut(&config.voice.voice_note_shortcut);
    config.voice.live_ptt_shortcut = normalize_optional_shortcut(&config.voice.live_ptt_shortcut);
    config.orb.hotkey = normalize_optional_shortcut(&config.orb.hotkey);
    config.orb.voice_mode = normalize_orb_voice_mode(&config.orb.voice_mode);
    config.orb.leash_minutes = config.orb.leash_minutes.clamp(5, 240);
    config.orb.follow_up_seconds = config.orb.follow_up_seconds.clamp(1, 60);
    config.orb.resting_x = normalize_orb_coordinate(config.orb.resting_x);
    config.orb.resting_y = normalize_orb_coordinate(config.orb.resting_y);
    config.orb.expanded_x = normalize_orb_coordinate(config.orb.expanded_x);
    config.orb.expanded_y = normalize_orb_coordinate(config.orb.expanded_y);
    let mut phrases = Vec::new();
    for phrase in &config.orb.wake_phrases {
        let phrase = phrase.trim();
        if !phrase.is_empty()
            && !phrases
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(phrase))
        {
            phrases.push(phrase.to_string());
        }
    }
    if phrases.is_empty() {
        phrases = default_orb_wake_phrases();
    }
    config.orb.wake_phrases = phrases;
    config.voice.voice_note_gesture =
        normalize_gesture(&config.voice.voice_note_gesture, DEFAULT_VOICE_NOTE_GESTURE);
    config.general.quick_overlay_gesture = normalize_gesture(
        &config.general.quick_overlay_gesture,
        DEFAULT_OVERLAY_GESTURE,
    );
    if let Some(url) = config.network.engine_base_url.as_mut() {
        let trimmed = url.trim().trim_end_matches('/').to_string();
        if trimmed.is_empty() {
            config.network.engine_base_url = None;
        } else {
            *url = trimmed;
        }
    }
    config.contextual_assist.default_personality =
        match config.contextual_assist.default_personality.trim() {
            "" => default_contextual_assist_personality(),
            value => value.to_string(),
        };
    config.contextual_assist.excluded_apps = config
        .contextual_assist
        .excluded_apps
        .iter()
        .map(|app| app.trim())
        .filter(|app| !app.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    if config.voice.default_thread_id.trim().is_empty() {
        config.voice.default_thread_id = default_voice_thread_id();
    } else {
        config.voice.default_thread_id = config.voice.default_thread_id.trim().to_string();
    }
    if config.voice.live_ptt_realtime_profile.trim().is_empty() {
        config.voice.live_ptt_realtime_profile = default_live_ptt_realtime_profile();
    } else {
        config.voice.live_ptt_realtime_profile =
            config.voice.live_ptt_realtime_profile.trim().to_string();
    }
    if config.host_gateway.bind_host.trim().is_empty() {
        config.host_gateway.bind_host = default_host_gateway_bind_host();
    }
    let port_scoped_runtime_url = format!(
        "http://{}:{}",
        config.host_gateway.bind_host, config.host_gateway.port
    );
    if config.host_gateway.runtime_url.trim().is_empty()
        || config.host_gateway.runtime_url == default_host_gateway_runtime_url()
    {
        config.host_gateway.runtime_url = port_scoped_runtime_url.clone();
    }
    let port_scoped_local_url = format!(
        "http://{}:{}",
        config.host_gateway.bind_host, config.host_gateway.port
    );
    if config.host_gateway.local_url.trim().is_empty()
        || config.host_gateway.local_url == default_host_gateway_local_url()
    {
        config.host_gateway.local_url = port_scoped_local_url;
    }
    if config.host_gateway.ui_url.trim().is_empty() {
        config.host_gateway.ui_url = default_ui_url();
    }
}

impl Default for ContextualAssistConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            show_on_selected_text: true,
            show_in_writable_fields: true,
            explicit_hotkey_only: false,
            default_personality: default_contextual_assist_personality(),
            excluded_apps: Vec::new(),
        }
    }
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            voice_note_shortcut: default_voice_note_shortcut(),
            voice_note_gesture: default_voice_note_gesture(),
            live_ptt_shortcut: default_live_ptt_shortcut(),
            live_ptt_realtime_profile: default_live_ptt_realtime_profile(),
            default_thread_id: default_voice_thread_id(),
            retain_voice_note_audio: false,
            output_muted: false,
            voice_mode: default_voice_mode(),
        }
    }
}

impl Default for OrbConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            wake_enabled: false,
            voice_mode: default_orb_voice_mode(),
            voice_mode_seeded: false,
            hotkey: default_orb_shortcut(),
            leash_minutes: default_orb_leash_minutes(),
            follow_up_seconds: default_orb_follow_up_seconds(),
            wake_phrases: default_orb_wake_phrases(),
            armed_on_battery: true,
            auto_expand_on_wake: true,
            resting_x: None,
            resting_y: None,
            expanded_x: None,
            expanded_y: None,
        }
    }
}

fn normalize_orb_coordinate(value: Option<f64>) -> Option<f64> {
    value
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 1.0))
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            launch_at_login: default_true(),
            notifications_enabled: default_true(),
            prevent_sleep: default_true(),
            container_image: default_image(),
            container_name: default_container_name(),
            manage_runtime_stack: default_manage_runtime_stack(),
            runtime_root: None,
            quick_overlay_shortcut: default_overlay_shortcut(),
            quick_overlay_gesture: default_overlay_gesture(),
            screen_ask_shortcut: default_screen_ask_shortcut(),
            screen_clip_shortcut: default_screen_clip_shortcut(),
            screen_region_shortcut: default_screen_region_shortcut(),
            screen_watch_shortcut: default_screen_watch_shortcut(),
            hud_prewarm: default_true(),
        }
    }
}

impl Default for ContainerResourceConfig {
    fn default() -> Self {
        Self {
            cpu_cores: default_cpu_limit(),
            memory_gb: default_memory_gb(),
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            magician_port: default_magician_port(),
            magicutor_port: default_magicutor_port(),
            engine_base_url: None,
        }
    }
}

impl MagicianDesktopConfig {
    pub fn validate_runtime_root(&self) -> Result<(), String> {
        let Some(root) = self
            .general
            .runtime_root
            .as_deref()
            .map(str::trim)
            .filter(|root| !root.is_empty())
        else {
            return Ok(());
        };
        if !std::path::Path::new(root).is_absolute() {
            return Err("Magician data folder must be an absolute path".to_string());
        }
        Ok(())
    }

    pub fn apply_runtime_root_to_process(&self) {
        match self
            .general
            .runtime_root
            .as_deref()
            .map(str::trim)
            .filter(|root| !root.is_empty())
        {
            Some(root) => std::env::set_var(crate::runtime_paths::DESKTOP_ROOT_ENV_KEY, root),
            None => std::env::remove_var(crate::runtime_paths::DESKTOP_ROOT_ENV_KEY),
        }
    }

    /// Effective Magician engine origin. Empty override falls back to loopback.
    pub fn engine_base_url(&self) -> String {
        self.network
            .engine_base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.trim_end_matches('/').to_string())
            .unwrap_or_else(|| format!("http://127.0.0.1:{}", self.network.magician_port))
    }

    /// WebSocket origin for the Magician engine (`http`→`ws`, `https`→`wss`).
    pub fn engine_ws_url(&self) -> String {
        let http = self.engine_base_url();
        if let Some(rest) = http.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = http.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            http
        }
    }

    pub fn engine_url(&self, path: &str) -> String {
        let path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        format!("{}{path}", self.engine_base_url())
    }

    pub fn engine_ws_path(&self, path: &str) -> String {
        let path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        format!("{}{path}", self.engine_ws_url())
    }

    /// True when the engine origin is not loopback. Local supervision must not
    /// spawn a container and must not read engine-owned files from this disk.
    pub fn is_remote_engine(&self) -> bool {
        match reqwest::Url::parse(&self.engine_base_url()) {
            Ok(parsed) => !matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "::1")),
            Err(_) => true,
        }
    }

    /// Remote engines are always externally managed. A stale local-container
    /// preference or environment override must never create a second backend
    /// after the user selects a remote origin.
    pub fn should_manage_runtime_stack(&self, requested: bool) -> bool {
        requested && !self.is_remote_engine()
    }

    /// Validate the operator-selected engine as an origin, not an arbitrary
    /// request URL. Remote desktop sessions carry a bearer and therefore
    /// require TLS; plaintext remains available only for loopback development.
    pub fn validate_engine_base_url(&self) -> Result<(), String> {
        let Some(raw) = self
            .network
            .engine_base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(());
        };
        let parsed = reqwest::Url::parse(raw)
            .map_err(|_| "Magician engine URL must be a valid HTTP(S) origin".to_string())?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || !matches!(parsed.path(), "" | "/")
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(
                "Magician engine URL must contain only an HTTP(S) scheme, host, and optional port"
                    .to_string(),
            );
        }
        if self.is_remote_engine() && parsed.scheme() != "https" {
            return Err("Remote Magician engines require HTTPS".to_string());
        }
        Ok(())
    }

    pub fn apply_engine_location_to_process(&self) {
        crate::magician_auth::apply_engine_location(
            self.is_remote_engine(),
            &self.engine_base_url(),
        );
    }
}

impl Default for HostGatewayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bind_host: default_host_gateway_bind_host(),
            port: default_host_gateway_port(),
            runtime_url: default_host_gateway_runtime_url(),
            local_url: default_host_gateway_local_url(),
            ui_url: default_ui_url(),
            macos_presence_host_enabled: true,
            mascot_follows_draw: true,
            macos_presence_host_user_disabled: false,
            macos_presence_host_bin: String::new(),
            macos_speech_helper_bin: String::new(),
        }
    }
}

impl Default for ApiKeysConfig {
    fn default() -> Self {
        Self {
            openai_api_key: String::new(),
            anthropic_api_key: String::new(),
        }
    }
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            auto_check: true,
            auto_update_container: false,
        }
    }
}

impl Default for MagicianDesktopConfig {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            contextual_assist: ContextualAssistConfig::default(),
            container: ContainerResourceConfig::default(),
            network: NetworkConfig::default(),
            host_gateway: HostGatewayConfig::default(),
            voice: VoiceConfig::default(),
            orb: OrbConfig::default(),
            api_keys: ApiKeysConfig::default(),
            updates: UpdateConfig::default(),
        }
    }
}

/// Returns the base data directory: the macOS Application Support bundle dir
/// (`~/Library/Application Support/dev.magician.desktop`), shared with the
/// install manifest. Historically this was `~/magician_data_v3`, which collided
/// by name with the backend's project-local data dir and polluted the home
/// directory. `migrate_legacy_data_dir` relocates any leftover on first launch.
pub fn data_dir() -> PathBuf {
    crate::manifest::manifest_dir()
}

/// Returns the config file path: `<data_dir>/config/magician.toml`.
pub fn config_file_path() -> PathBuf {
    data_dir().join("config").join("magician.toml")
}

/// Returns the log directory: `<data_dir>/logs`.
pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

/// One-time migration: relocate the desktop config out of the legacy
/// `~/magician_data_v3` (which collided by name with the backend's project-local
/// data dir and polluted the home directory) into the Application Support
/// location now returned by [`data_dir`]. Moves the config file (preserving
/// `api_keys` etc.), then removes the now-empty legacy directories so
/// `~/magician_data_v3` disappears. A populated container volume under the
/// legacy path is left intact — `remove_dir` only deletes EMPTY dirs. Idempotent
/// and best-effort: any failure is logged and never blocks startup.
fn migrate_legacy_data_dir() {
    let legacy_root = match dirs::home_dir() {
        Some(home) => home.join("magician_data_v3"),
        None => return,
    };
    if !legacy_root.exists() {
        return;
    }
    let legacy_config = legacy_root.join("config").join("magician.toml");
    let new_config = config_file_path();
    if !new_config.exists() && legacy_config.exists() {
        if let Some(parent) = new_config.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(
                    "config migration: cannot create {}: {}",
                    parent.display(),
                    e
                );
                return;
            }
        }
        // Prefer an atomic rename; fall back to copy+remove across volumes (EXDEV).
        let moved = std::fs::rename(&legacy_config, &new_config).or_else(|_| {
            std::fs::copy(&legacy_config, &new_config).map(|_| {
                let _ = std::fs::remove_file(&legacy_config);
            })
        });
        match moved {
            Ok(()) => tracing::info!(
                "Migrated desktop config to {} (from legacy {})",
                new_config.display(),
                legacy_config.display()
            ),
            Err(e) => {
                tracing::warn!("config migration: failed to move legacy config: {}", e);
                return;
            },
        }
    }
    // Remove now-empty legacy dirs so ~/magician_data_v3 is gone. `remove_dir`
    // only succeeds on empty dirs, so a populated container volume is preserved.
    let _ = std::fs::remove_dir(legacy_root.join("config"));
    let _ = std::fs::remove_dir(legacy_root.join("logs"));
    let _ = std::fs::remove_dir(legacy_root.join("data"));
    if std::fs::remove_dir(&legacy_root).is_ok() {
        tracing::info!("Removed empty legacy data dir {}", legacy_root.display());
    }
}

/// Load configuration from disk, returning defaults if file doesn't exist.
pub fn load_config() -> Result<MagicianDesktopConfig, String> {
    migrate_legacy_data_dir();
    let path = config_file_path();
    if !path.exists() {
        info!(
            "Config file not found at {}, using defaults",
            path.display()
        );
        let config = MagicianDesktopConfig::default();
        config.apply_runtime_root_to_process();
        config.apply_engine_location_to_process();
        return Ok(config);
    }

    let contents = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read config at {}: {}", path.display(), e))?;

    let mut config: MagicianDesktopConfig =
        toml::from_str(&contents).map_err(|e| format!("Failed to parse config: {}", e))?;
    normalize_config(&mut config);
    config.validate_runtime_root()?;
    config.validate_engine_base_url()?;
    config.apply_runtime_root_to_process();
    config.apply_engine_location_to_process();

    info!("Loaded config from {}", path.display());
    Ok(config)
}

/// Save configuration to disk, creating parent directories if needed.
pub fn save_config(config: &MagicianDesktopConfig) -> Result<(), String> {
    let mut config = config.clone();
    normalize_config(&mut config);
    config.validate_runtime_root()?;
    config.validate_engine_base_url()?;
    config.apply_runtime_root_to_process();
    config.apply_engine_location_to_process();
    let path = config_file_path();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config directory: {}", e))?;
    }

    let toml_string = toml::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;

    // Atomic write: write to a temp file first, then rename into place.
    // This prevents a crash mid-write from corrupting the config.
    let tmp_path = path.with_extension("toml.tmp");
    std::fs::write(&tmp_path, &toml_string)
        .map_err(|e| format!("Failed to write temporary config: {}", e))?;
    std::fs::rename(&tmp_path, &path)
        .map_err(|e| format!("Failed to rename config into place: {}", e))?;

    info!("Saved config to {}", path.display());
    Ok(())
}

/// Ensure all required data directories exist.
pub fn ensure_data_dirs() -> Result<(), String> {
    let dirs_to_create = [
        data_dir(),
        data_dir().join("config"),
        log_dir(),
        data_dir().join("data"),
    ];

    for dir in &dirs_to_create {
        if !dir.exists() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("Failed to create {}: {}", dir.display(), e))?;
            info!("Created directory: {}", dir.display());
        }
    }

    Ok(())
}

#[cfg(test)]
mod orb_config_tests {
    use super::*;

    #[test]
    fn orb_defaults_are_armed_but_bounded() {
        let orb = OrbConfig::default();
        assert!(orb.enabled);
        assert!(!orb.wake_enabled);
        assert_eq!(orb.voice_mode, "hands_free");
        assert!(!orb.voice_mode_seeded);
        assert_eq!(orb.hotkey, "Alt+Space");
        assert_eq!(orb.leash_minutes, 120);
        assert_eq!(orb.follow_up_seconds, 8);
        assert_eq!(orb.wake_phrases, vec!["hey assistant"]);
    }

    #[test]
    fn legacy_orb_config_leaves_wake_off_until_turned_on() {
        let orb: OrbConfig = toml::from_str("enabled = false").expect("legacy orb config");
        assert!(!orb.enabled);
        assert!(!orb.wake_enabled);
        let explicit: OrbConfig = toml::from_str("wake_enabled = true").expect("explicit wake");
        assert!(explicit.wake_enabled);
    }

    #[test]
    fn orb_normalization_clamps_and_deduplicates_user_input() {
        let mut config = MagicianDesktopConfig::default();
        config.orb.hotkey = " disabled ".to_string();
        config.orb.voice_mode = " LIVE ".to_string();
        config.orb.leash_minutes = 999;
        config.orb.follow_up_seconds = 0;
        config.orb.resting_x = Some(-2.0);
        config.orb.resting_y = Some(4.0);
        config.orb.expanded_x = Some(f64::NAN);
        config.orb.wake_phrases = vec![
            " Hey Magician ".to_string(),
            "hey magician".to_string(),
            "".to_string(),
            "Hello Merlin".to_string(),
        ];
        normalize_config(&mut config);
        assert!(config.orb.hotkey.is_empty());
        assert_eq!(config.orb.voice_mode, "realtime");
        assert_eq!(config.orb.leash_minutes, 240);
        assert_eq!(config.orb.follow_up_seconds, 1);
        assert_eq!(config.orb.resting_x, Some(0.0));
        assert_eq!(config.orb.resting_y, Some(1.0));
        assert_eq!(config.orb.expanded_x, None);
        assert_eq!(
            config.orb.wake_phrases,
            vec!["Hey Magician".to_string(), "Hello Merlin".to_string()]
        );
    }

    #[test]
    fn orb_voice_mode_normalization_preserves_all_three_ambient_engines() {
        assert_eq!(normalize_orb_voice_mode(" recording "), "dictation");
        assert_eq!(normalize_orb_voice_mode("DICTATE"), "dictation");
        assert_eq!(normalize_orb_voice_mode("handsfree"), "hands_free");
        assert_eq!(normalize_orb_voice_mode(" LIVE "), "realtime");
        assert_eq!(normalize_orb_voice_mode("unknown"), "hands_free");
    }

    #[test]
    fn empty_phrase_sets_restore_the_safe_default() {
        let mut config = MagicianDesktopConfig::default();
        config.orb.wake_phrases = vec![" ".to_string()];
        normalize_config(&mut config);
        assert_eq!(config.orb.wake_phrases, vec!["hey assistant"]);
    }
}

#[cfg(test)]
mod engine_base_url_tests {
    use super::*;

    #[test]
    fn default_engine_base_url_is_loopback_magician_port() {
        let config = MagicianDesktopConfig::default();
        assert_eq!(config.engine_base_url(), "http://127.0.0.1:3002");
        assert_eq!(config.engine_ws_url(), "ws://127.0.0.1:3002");
        assert!(!config.is_remote_engine());
        assert_eq!(
            config.engine_url("/api/magician/v2/health"),
            "http://127.0.0.1:3002/api/magician/v2/health"
        );
    }

    #[test]
    fn override_engine_base_url_maps_http_and_https() {
        let mut config = MagicianDesktopConfig::default();
        config.network.engine_base_url = Some("https://engine.example:8443/".into());
        normalize_config(&mut config);
        assert_eq!(config.engine_base_url(), "https://engine.example:8443");
        assert_eq!(config.engine_ws_url(), "wss://engine.example:8443");
        assert!(config.is_remote_engine());
        assert_eq!(
            config.engine_ws_path("/api/magician/v2/realtime/ws"),
            "wss://engine.example:8443/api/magician/v2/realtime/ws"
        );
    }

    #[test]
    fn empty_override_keeps_local_default() {
        let mut config = MagicianDesktopConfig::default();
        config.network.engine_base_url = Some("  ".into());
        normalize_config(&mut config);
        assert_eq!(config.network.engine_base_url, None);
        assert!(!config.is_remote_engine());
    }

    #[test]
    fn remote_engine_never_manages_a_local_runtime_stack() {
        let mut config = MagicianDesktopConfig::default();
        assert!(config.should_manage_runtime_stack(true));
        assert!(!config.should_manage_runtime_stack(false));

        config.network.engine_base_url = Some("https://connect.magican.ai".into());
        assert!(!config.should_manage_runtime_stack(true));
    }

    #[test]
    fn engine_url_accepts_loopback_http_and_remote_https_origins_only() {
        for accepted in [
            None,
            Some("http://127.0.0.1:3002"),
            Some("http://localhost:3002"),
            Some("https://connect.magican.ai"),
        ] {
            let mut config = MagicianDesktopConfig::default();
            config.network.engine_base_url = accepted.map(str::to_string);
            assert!(config.validate_engine_base_url().is_ok(), "{accepted:?}");
        }
        for rejected in [
            "http://connect.magican.ai",
            "https://user:password@connect.magican.ai",
            "https://connect.magican.ai/api",
            "https://connect.magican.ai?workspace=other",
            "file:///tmp/magician",
        ] {
            let mut config = MagicianDesktopConfig::default();
            config.network.engine_base_url = Some(rejected.to_string());
            assert!(
                config.validate_engine_base_url().is_err(),
                "accepted {rejected}"
            );
        }
    }
}
