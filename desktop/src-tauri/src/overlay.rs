use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::utils::config::Color;
use tauri::{AppHandle, Emitter, Manager, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};
use tokio::time::interval;
use tracing::{info, warn};

const OVERLAY_WINDOW_LABEL: &str = "overlay";
const DRAW_OVERLAY_WINDOW_LABEL: &str = "draw-overlay";
const CONTEXTUAL_ASSIST_WINDOW_LABEL: &str = "contextual-assist";
const OVERLAY_DRAW_EVENT: &str = "overlay-draw-shape";
const OVERLAY_REPLAY_EVENT: &str = "overlay-replay-request";
const OVERLAY_EXPLAIN_DEEPER_EVENT: &str = "overlay-explain-deeper-request";
const OVERLAY_KEEP_SHOWING_EVENT: &str = "overlay-keep-showing-request";
const OVERLAY_DISMISS_EVENT: &str = "overlay-dismiss-request";
const OVERLAY_COPILOT_USER_ACTION_EVENT: &str = "overlay-copilot-user-action";
const TUTOR_OVERLAY_STATUS_EVENT: &str = "tutor-overlay-status";
/// HUD URL — `/hud` is a thin wrapper around the reusable
/// `$lib/magician/chat/ChatPanel.svelte` component. The chat
/// behavior (composer, transcript, send, message-type rendering,
/// state subscriptions) lives in ChatPanel; the /hud route owns
/// only the HUD aesthetic (transparent background, fade above
/// stream, centered composer when empty). /chat will adopt the
/// same ChatPanel in a future pass — single source of truth.
const OVERLAY_HUD_DEV_URL: &str = "http://localhost:5173/hud";
const DRAW_OVERLAY_DEV_URL: &str = "http://localhost:5173/draw-overlay";
const CONTEXTUAL_ASSIST_DEV_URL: &str = "http://localhost:5173/contextual-assist";
const OVERLAY_HUD_APP_PATH: &str = "/hud";
const DRAW_OVERLAY_APP_PATH: &str = "/draw-overlay";
const CONTEXTUAL_ASSIST_APP_PATH: &str = "/contextual-assist";
// Notification overlay — push-style cards pinned bottom-right (the stack grows
// upward), ABOVE the draw/tutor overlay. The window itself is just a transport;
// the card stack, content height reporting, and dismiss behavior live in the
// `/notify-overlay` Svelte route.
const NOTIFY_OVERLAY_WINDOW_LABEL: &str = "notify-overlay";
const NOTIFY_OVERLAY_DEV_URL: &str = "http://localhost:5173/notify-overlay";
const NOTIFY_OVERLAY_APP_PATH: &str = "/notify-overlay";
/// Fixed narrow column, pinned to the bottom-right corner.
const NOTIFY_OVERLAY_WIDTH: f64 = 300.0;
/// Tiny until the route reports its real content height via `resize_notify_overlay`.
const NOTIFY_OVERLAY_INIT_HEIGHT: f64 = 1.0;
/// Margin from the screen edges when pinning the overlay bottom-right.
const NOTIFY_OVERLAY_SCREEN_MARGIN: f64 = 12.0;
/// Extra gap from the bottom edge so the bottom-anchored stack clears a typical
/// Dock (tunable).
const NOTIFY_OVERLAY_BOTTOM_MARGIN: f64 = 24.0;
// ─── HUD stage geometry ───
// The window is a generous TRANSPARENT STAGE; the themed panel is a smaller box
// centred inside it (`.hud` in the `/hud` route). The stage exists purely so
// popups have somewhere to render: a window cannot paint outside itself, so a
// window shrink-wrapped to the panel would clip the theme dropdown, the session
// more-menu, the profile picker and the mention list — every one of which is
// taller than a collapsed composer.
//
// An earlier iteration sized the window to the panel's measured height and grew
// it as popups opened. That fought the layout constantly (a popup has to be
// visible before it can be measured) and is replaced by simply giving the stage
// room up front.
/// Stage width. Also the webview viewport width, which must clear unified-ui's
/// phone breakpoint (`max-width: 767px`) — below it the composer renders its
/// TOUCH layout (Do|Plan hidden, 40px targets, profile reduced to an icon).
const OVERLAY_HUD_WIDTH: f64 = 1000.0;
/// Stage height. Room for the panel plus a dropdown opening beneath it.
const OVERLAY_HUD_HEIGHT: f64 = 760.0;
/// Keep the stage inside the monitor on short screens.
const OVERLAY_HUD_MAX_SCREEN_FRACTION: f64 = 0.9;
const OVERLAY_STATE_EVENT: &str = "overlay-state";
const OVERLAY_FOCUS_EVENT: &str = "overlay-focus-input";
const CONTEXTUAL_ASSIST_TARGET_EVENT: &str = "contextual-assist-target";
const CONTEXTUAL_ASSIST_DISPLAY_EVENT: &str = "contextual-assist-display";
const CONTEXTUAL_ASSIST_DISMISS_REQUEST_EVENT: &str = "contextual-assist-dismiss-request";
const OVERLAY_MONITOR_INTERVAL_SECS: u64 = 3;
/// Delay from boot completion to pre-warming the hidden HUD WebView. Long
/// enough to stay clear of tray/shortcut/gateway boot work on the main thread;
/// short enough that a first summon lands on the warm path.
const OVERLAY_PREWARM_DELAY: Duration = Duration::from_secs(5);
const DRAW_OVERLAY_MODEL_MAX_SIDE: f64 = 2048.0;
const DRAW_OVERLAY_MIN_TTL_MS: u64 = 20_000;
const DRAW_OVERLAY_DEFAULT_TTL_MS: u64 = 60_000;
const DRAW_OVERLAY_DEFAULT_DRAW_DURATION_MS: u64 = 1_600;
const DRAW_OVERLAY_MIN_DRAW_DURATION_MS: u64 = 1_300;
const DRAW_OVERLAY_MAX_DRAW_DURATION_MS: u64 = 3_600;
const DRAW_OVERLAY_DELAY_SCALE_NUMERATOR: u64 = 5;
const DRAW_OVERLAY_DELAY_SCALE_DENOMINATOR: u64 = 3;
const DRAW_OVERLAY_IMPLICIT_REVEAL_STEP_MS: u64 = 2_400;
const DRAW_OVERLAY_MAX_DELAY_MS: u64 = 60_000;
const DRAW_OVERLAY_REPLAY_BUTTON_WIDTH: f64 = 118.0;
const DRAW_OVERLAY_REPLAY_BUTTON_HEIGHT: f64 = 34.0;
const DRAW_OVERLAY_REPLAY_BUTTON_RIGHT: f64 = 26.0;
const DRAW_OVERLAY_REPLAY_BUTTON_BOTTOM: f64 = 26.0;
const DRAW_OVERLAY_DISMISS_BUTTON_WIDTH: f64 = 128.0;
const DRAW_OVERLAY_CONTROL_BUTTON_GAP: f64 = 10.0;
const DRAW_OVERLAY_CONTROL_HIT_PADDING: f64 = 16.0;
const CONTEXTUAL_ASSIST_SCREEN_MARGIN: f64 = 12.0;
const CONTEXTUAL_ASSIST_CHIP_WIDTH: u32 = 38;
const CONTEXTUAL_ASSIST_CHIP_HEIGHT: u32 = 38;
const CONTEXTUAL_ASSIST_MENU_WIDTH: u32 = 456;
const CONTEXTUAL_ASSIST_MENU_HEIGHT: u32 = 520;
const DRAW_OVERLAY_GROUP_MAX_DEPTH: usize = 8;
const DRAW_OVERLAY_MAX_PATH_CHARS: usize = 8_000;
const DRAW_OVERLAY_GENERATION_FIELD: &str = "_draw_generation";
const DRAW_OVERLAY_INHERITED_GROUP_KEYS: &[&str] = &[
    "group_id",
    "storyboard_step_id",
    "reveal_id",
    "reveal_order",
    "tutor_step_label",
    "step_label",
    "narration",
    "wait_for_voice",
    "clear_previous",
    "persist_until_step",
    "z_index",
    "duration_ms",
    "delay_ms",
    "animate",
    "style",
    "color",
    "ttl_ms",
    "persist",
    "canvas_mode",
    "source_entity_ids",
    "coordinate_space",
    "capture_screen_rect",
    "capture_image_size",
];
static OVERLAY_DRAW_ID_COUNTER: AtomicU64 = AtomicU64::new(1);
static CONTEXTUAL_ASSIST_EXPANDED: AtomicBool = AtomicBool::new(false);
static CONTEXTUAL_ASSIST_MANUAL_GRACE_UNTIL_MS: AtomicU64 = AtomicU64::new(0);
static CONTEXTUAL_ASSIST_TARGET: OnceLock<
    StdMutex<Option<crate::contextual_assist::NativeAssistTarget>>,
> = OnceLock::new();
static CONTEXTUAL_ASSIST_CHIP_RECT: OnceLock<StdMutex<Option<ContextualAssistWindowRect>>> =
    OnceLock::new();

#[derive(Debug, Clone, Copy)]
struct ContextualAssistWindowRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl ContextualAssistWindowRect {
    fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x && x <= self.x + self.width && y >= self.y && y <= self.y + self.height
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ContextualAssistScreenBounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ContextualAssistWindowPlacement {
    x: f64,
    y: f64,
    logical_width: f64,
    logical_height: f64,
    physical_width: f64,
    physical_height: f64,
}

#[derive(Debug, Default)]
pub struct OverlaySessionState {
    pub tracked_executions: Vec<TrackedOverlayExecution>,
    pub registered_shortcut: Option<String>,
    pub selected_execution_id: Option<String>,
    /// Draw commands emitted before the draw-overlay webview has mounted its
    /// event listeners. The draw overlay drains this queue on mount and
    /// deduplicates by shape id against live events.
    pub pending_draw_shapes: Vec<Value>,
    pub active_draw_shapes: Vec<ActiveOverlayDrawShape>,
    /// Monotonic token for cancelling delayed/retry emissions from older draw
    /// batches after a clear, action transition, or newer tutor draw.
    pub draw_generation: u64,
    /// Latest pre-draw tutor status. The draw overlay pulls this on mount so a
    /// just-created webview does not miss the initial "working" event.
    pub tutor_overlay_status: Option<TutorOverlayStatusPayload>,
    /// Exact SVG/model-space rectangles for draw-overlay controls as rendered by
    /// the webview. Native click interception uses these instead of duplicating
    /// frontend visibility and layout rules.
    pub draw_overlay_controls: DrawOverlayControlsState,
}

#[derive(Debug, Clone)]
pub struct ActiveOverlayDrawShape {
    pub shape: Value,
    pub created_at: Instant,
}

impl ActiveOverlayDrawShape {
    fn new(shape: Value) -> Self {
        Self {
            shape,
            created_at: Instant::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TutorOverlayStatusPayload {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canvas_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rail: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
pub struct DrawOverlayControlRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DrawOverlayControlsState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismiss_rect: Option<DrawOverlayControlRect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_showing_rect: Option<DrawOverlayControlRect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_rect: Option<DrawOverlayControlRect>,
    /// "Explain deeper". The overlay reports this only while the control is
    /// visible and safe to invoke. Unlike permanent controls, the native host
    /// must never guess this region from a constant fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deeper_rect: Option<DrawOverlayControlRect>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub action_rects: Vec<DrawOverlayControlRect>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrackedOverlayExecution {
    pub execution_id: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct OverlayOption {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct OverlayPrompt {
    pub execution_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pause_state_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    pub input_type: String,
    pub question: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<OverlayOption>,
    #[serde(default)]
    pub is_retry: bool,
    #[serde(default)]
    pub retry_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_answer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirm_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deny_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pause_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycle_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OverlayClarification {
    pub execution_id: String,
    pub question_id: String,
    pub question_text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_snippets: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<OverlayOption>,
    #[serde(default)]
    pub urgency: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_slot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_total: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct OverlayPendingAction {
    pub step_id: String,
    pub action_description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct OverlayApproval {
    pub approval_id: String,
    pub agent_id: String,
    pub goal_id: String,
    pub cycle_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_actions: Vec<OverlayPendingAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct OverlayTrackedExecutionSummary {
    pub execution_id: String,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_on: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting_question: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OverlaySnapshot {
    pub shortcut: String,
    pub needs_setup: bool,
    pub container_running: bool,
    pub container_status: String,
    pub service_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attention_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_execution_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_execution_status: Option<String>,
    pub can_start: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracked_executions: Vec<OverlayTrackedExecutionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_prompt: Option<OverlayPrompt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_clarification: Option<OverlayClarification>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_approval: Option<OverlayApproval>,
    #[serde(default)]
    pub pending_approval_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayStartResponse {
    pub execution_id: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct OverlayResumeRequest {
    pub execution_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_state_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    pub input_type: String,
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OverlayClarificationSubmitRequest {
    pub execution_id: String,
    pub question_id: String,
    pub response_text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OverlayApprovalResolveRequest {
    pub approval_id: String,
    pub decision: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
struct OverlayExecutionRuntimeState {
    tracked: TrackedOverlayExecution,
    status: Option<String>,
    pending_prompt: Option<OverlayPrompt>,
    pending_clarification: Option<OverlayClarification>,
}

pub fn validate_shortcut(shortcut: &str, default_value: &str) -> Result<String, String> {
    let normalized = crate::config::normalize_shortcut(shortcut, default_value);
    normalized
        .parse::<Shortcut>()
        .map_err(|e| format!("Invalid shortcut '{}': {}", normalized, e))?;
    Ok(normalized)
}

fn overlay_route_webview_url(
    route_name: &str,
    dev_url: &str,
    app_path: &str,
) -> Result<tauri::WebviewUrl, String> {
    fn bundled_app_url(route_name: &str, app_path: &str) -> Result<tauri::WebviewUrl, String> {
        let path = app_path.trim();
        if path.is_empty() {
            return Err(format!("{route_name} app path cannot be empty"));
        }
        Ok(crate::tray::desktop_app_webview_url(path))
    }

    #[cfg(debug_assertions)]
    {
        // Debug default is the Vite dev server (live reload). Setting
        // MAGICIAN_DESKTOP_BUNDLED_OVERLAYS serves the built desktop/dist
        // through the custom protocol instead — the same assets the
        // Settings/Logs windows already load in debug — so overlay-latency
        // checks and non-CSS iteration don't pay Vite's on-demand transform.
        let use_bundled = std::env::var("MAGICIAN_DESKTOP_BUNDLED_OVERLAYS")
            .is_ok_and(|value| !value.is_empty() && value != "0" && value != "false");
        if !use_bundled {
            let url: tauri::Url = dev_url
                .parse()
                .map_err(|e| format!("Invalid {route_name} dev URL '{dev_url}': {e}"))?;
            return Ok(tauri::WebviewUrl::External(url));
        }
        bundled_app_url(route_name, app_path)
    }

    #[cfg(not(debug_assertions))]
    {
        let _ = dev_url;
        bundled_app_url(route_name, app_path)
    }
}

pub fn validate_overlay_shortcut(shortcut: &str) -> Result<String, String> {
    // Optional chord: empty / none / disabled → no chord (the overlay is summoned
    // by the double-tap Left-Option gesture instead). A non-empty value must parse
    // as a real chord.
    let normalized = crate::config::normalize_optional_shortcut(shortcut);
    if normalized.is_empty() {
        return Ok(String::new());
    }
    validate_shortcut(&normalized, crate::config::DEFAULT_OVERLAY_SHORTCUT)
}

pub fn current_overlay_shortcut(app: &AppHandle) -> String {
    let state = app.state::<crate::AppState>();
    if let Ok(overlay) = state.overlay_session.try_lock() {
        if let Some(shortcut) = overlay.registered_shortcut.as_ref() {
            return shortcut.clone();
        }
    }
    crate::config::DEFAULT_OVERLAY_SHORTCUT.to_string()
}

/// Display label for the configured quick-overlay double-tap gesture (e.g.
/// "Double Left ⌥"), or None when the gesture is disabled.
pub fn current_overlay_gesture_label(app: &AppHandle) -> Option<String> {
    let state = app.state::<crate::AppState>();
    let gesture = state
        .config
        .try_lock()
        .ok()
        .map(|config| config.general.quick_overlay_gesture.clone())?;
    crate::voice_gesture::overlay_gesture_menu_label(&gesture)
}

fn handle_overlay_shortcut(app: &AppHandle, _shortcut: &Shortcut, event: ShortcutEvent) {
    // Only react to the keydown edge — not keyup. Without this the
    // hotkey fires twice per press (down + up) and the second event
    // immediately re-hides the just-shown HUD.
    if event.state != ShortcutState::Pressed {
        return;
    }
    trigger_overlay_toggle(app);
}

/// Toggle the overlay window (show if hidden, hide if shown). Safe to call from
/// any thread — including the native gesture-tap thread — because it offloads to
/// the async runtime. Used by both the optional global-shortcut chord and the
/// double-tap Left-Option gesture (`voice_gesture.rs`).
pub fn trigger_overlay_toggle(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // `is_visible()` returns false when the window was never created or was
        // hidden — either case routes to show.
        let currently_visible = app
            .get_webview_window(OVERLAY_WINDOW_LABEL)
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false);
        let result = if currently_visible {
            hide_overlay_window(&app)
        } else {
            show_overlay_window(&app)
        };
        if let Err(err) = result {
            warn!("Failed to toggle overlay: {}", err);
        }
    });
}

pub async fn sync_overlay_shortcut(app: &AppHandle, shortcut: &str) -> Result<String, String> {
    let desired = validate_overlay_shortcut(shortcut)?;
    let desired_opt = if desired.is_empty() {
        None
    } else {
        Some(desired.clone())
    };
    let state = app.state::<crate::AppState>();
    let previous = {
        let overlay = state.overlay_session.lock().await;
        overlay.registered_shortcut.clone()
    };

    if previous != desired_opt {
        if let Some(current) = previous.as_deref() {
            if app.global_shortcut().is_registered(current) {
                app.global_shortcut()
                    .unregister(current)
                    .map_err(|e| format!("Failed to unregister shortcut {}: {}", current, e))?;
            }
        }

        if let Some(desired) = desired_opt.as_deref() {
            if let Err(error) = app
                .global_shortcut()
                .on_shortcut(desired, handle_overlay_shortcut)
            {
                if let Some(current) = previous.as_deref() {
                    let _ = app
                        .global_shortcut()
                        .on_shortcut(current, handle_overlay_shortcut);
                }
                return Err(format!(
                    "Failed to register global shortcut {}: {}",
                    desired, error
                ));
            }
            info!("Registered global shortcut {}", desired);
        } else {
            info!("Quick-overlay chord disabled (summon via double-tap gesture)");
        }
    }

    {
        let mut overlay = state.overlay_session.lock().await;
        overlay.registered_shortcut = desired_opt.clone();
    }

    let update_version = state.pending_app_update.lock().await.clone();
    crate::tray::refresh_menu(app, update_version.as_deref());

    Ok(desired)
}

pub fn initialize_overlay(app: &AppHandle, prewarm_enabled: bool) -> Result<(), String> {
    // Hidden WebViews are intentionally NOT created during boot: constructing
    // the HUD and contextual-assist windows there serializes WebKit/AppKit
    // work onto the main thread and can hold up unrelated essentials such as
    // the host gateway, making a cold tray launch visibly janky. The HUD is
    // instead pre-warmed `OVERLAY_PREWARM_DELAY` after boot (below) so its
    // first summon hits the warm path. Every show path still calls its
    // `ensure_*_window`, so a summon during the delay — or after macOS purges
    // the parked WebView under memory pressure — simply takes the cold path
    // and remains correct.
    app.plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .map_err(|e| format!("Failed to initialize global shortcut plugin: {}", e))?;

    // The default chord is empty (the overlay is summoned by the double-tap
    // Left-Option gesture); only register a chord if one is explicitly set.
    match validate_overlay_shortcut(crate::config::DEFAULT_OVERLAY_SHORTCUT) {
        Ok(shortcut) if !shortcut.is_empty() => {
            if let Err(e) = app
                .global_shortcut()
                .on_shortcut(shortcut.as_str(), handle_overlay_shortcut)
            {
                warn!(
                    "Failed to register global shortcut {}: {}. The overlay remains available from the tray menu and the double-tap gesture.",
                    shortcut, e
                );
            } else {
                if let Ok(mut overlay) = app.state::<crate::AppState>().overlay_session.try_lock() {
                    overlay.registered_shortcut = Some(shortcut.clone());
                }
                info!("Registered global shortcut {}", shortcut);
            }
        },
        Ok(_) => {
            info!("Quick-overlay chord disabled by default — summon via double-tap Left Option");
        },
        Err(e) => warn!("Invalid default overlay shortcut: {}", e),
    }

    if prewarm_enabled {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(OVERLAY_PREWARM_DELAY).await;
            // A summon during the delay already created the window; that race
            // resolves to Ok here. Losing a build race reports an error while
            // the window exists, so log-only is correct either way.
            match ensure_overlay_window(&app) {
                Ok(()) => info!("HUD WebView ready (pre-warm)"),
                Err(err) => warn!("HUD WebView pre-warm failed: {}", err),
            }
        });
    }

    Ok(())
}

pub fn ensure_overlay_window(app: &AppHandle) -> Result<(), String> {
    if app.get_webview_window(OVERLAY_WINDOW_LABEL).is_some() {
        return Ok(());
    }

    // Quick Automate is now a full-monitor HUD loaded from the unified-ui's
    // `/hud` route. The window is just a transport — every visual
    // decision (glass effect, themes, animations, layout) lives in the
    // Svelte page. No global NSVisualEffectView, no duplicate Svelte app,
    // no custom tray-owned chrome. See
    // `docs/plans/2026-05-22-tauri-unified-ui-consolidation.md` for the
    // consolidation rationale.
    let hud_url = overlay_route_webview_url("HUD", OVERLAY_HUD_DEV_URL, OVERLAY_HUD_APP_PATH)?;
    // Borderless window — not `fullscreen(true)`. macOS's
    // `fullscreen(true)` triggers the Spaces "Enter Full Screen"
    // treatment: a separate desktop, an animated zoom transition, the
    // menu bar auto-hides, the app icon shrinks. That's not the
    // immersive-HUD feel we want; it's the "movie-mode" feel.
    //
    // The right approach for a Jarvis-style HUD: borderless window
    // sized to the monitor, layered above everything, no chrome. The
    // monitor sizing happens in `show_overlay_window` after creation
    // — `current_monitor()` returns None before the window is shown,
    // so we can't size in the builder.
    let window = tauri::WebviewWindowBuilder::new(app, OVERLAY_WINDOW_LABEL, hud_url)
        .title("Magican HUD")
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        // Tauri enables its native drag-drop handler by default, which makes
        // WKWebView intercept file drags and prevents HTML5 `drop` from ever
        // seeing `dataTransfer.files`. Disabling it lets ChatPanel stage
        // drag-and-dropped files through its normal attachment upload path.
        .disable_drag_drop_handler()
        // Transparent so the HUD Svelte page can paint local glass surfaces
        // over the live desktop rather than a hard rectangular window. The
        // `macos-private-api` Tauri feature + `macOSPrivateApi: true` in
        // `tauri.conf.json` are required for transparency on macOS.
        .transparent(true)
        .build()
        .map_err(|e| format!("Failed to create overlay window: {}", e))?;

    let close_app = app.clone();
    window.on_window_event(move |event| {
        match event {
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = hide_overlay_window(&close_app);
            },
            // Click-outside dismiss. When the user clicks outside the
            // full-monitor overlay window, it loses focus. Hide it so
            // the HUD doesn't linger in the background — same UX as
            // Spotlight / a popover.
            WindowEvent::Focused(false) => {
                let _ = hide_overlay_window(&close_app);
            },
            _ => {},
        }
    });

    Ok(())
}

pub fn ensure_draw_overlay_window(app: &AppHandle) -> Result<(), String> {
    if app.get_webview_window(DRAW_OVERLAY_WINDOW_LABEL).is_some() {
        return Ok(());
    }

    let draw_url =
        overlay_route_webview_url("draw overlay", DRAW_OVERLAY_DEV_URL, DRAW_OVERLAY_APP_PATH)?;
    let window = tauri::WebviewWindowBuilder::new(app, DRAW_OVERLAY_WINDOW_LABEL, draw_url)
        .title("Magican Tutor Overlay")
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .transparent(true)
        .background_color(Color(0, 0, 0, 0))
        .focused(false)
        .focusable(false)
        .build()
        .map_err(|e| format!("Failed to create draw overlay window: {}", e))?;

    // Tao's Linux backend requires the GTK window to be realized before an
    // input shape can be installed. Linux applies click-through after show.
    #[cfg(target_os = "macos")]
    let _ = window.set_ignore_cursor_events(true);
    let close_app = app.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = hide_draw_overlay_window(&close_app);
        }
    });

    Ok(())
}

pub fn ensure_contextual_assist_window(app: &AppHandle) -> Result<(), String> {
    if app
        .get_webview_window(CONTEXTUAL_ASSIST_WINDOW_LABEL)
        .is_some()
    {
        return Ok(());
    }
    if current_thread_is_main_thread() {
        return ensure_contextual_assist_window_on_main(app);
    }

    let (tx, rx) = mpsc::channel();
    let app_for_main = app.clone();
    app.run_on_main_thread(move || {
        let _ = tx.send(ensure_contextual_assist_window_on_main(&app_for_main));
    })
    .map_err(|error| {
        format!("Failed to schedule contextual assist window creation on main thread: {error}")
    })?;
    rx.recv_timeout(Duration::from_secs(5)).map_err(|error| {
        format!("Timed out waiting for contextual assist window creation on main thread: {error}")
    })?
}

fn current_thread_is_main_thread() -> bool {
    #[cfg(target_os = "macos")]
    {
        objc2::MainThreadMarker::new().is_some()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

fn ensure_contextual_assist_window_on_main(app: &AppHandle) -> Result<(), String> {
    if app
        .get_webview_window(CONTEXTUAL_ASSIST_WINDOW_LABEL)
        .is_some()
    {
        return Ok(());
    }

    let assist_url = overlay_route_webview_url(
        "contextual assist",
        CONTEXTUAL_ASSIST_DEV_URL,
        CONTEXTUAL_ASSIST_APP_PATH,
    )?;
    let window = tauri::WebviewWindowBuilder::new(app, CONTEXTUAL_ASSIST_WINDOW_LABEL, assist_url)
        .title("Magican Contextual Assist")
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .transparent(true)
        .background_color(Color(0, 0, 0, 0))
        .shadow(false)
        .focused(false)
        .focusable(false)
        .inner_size(
            CONTEXTUAL_ASSIST_CHIP_WIDTH as f64,
            CONTEXTUAL_ASSIST_CHIP_HEIGHT as f64,
        )
        .build()
        .map_err(|e| format!("Failed to create contextual assist window: {e}"))?;
    let _ = window.set_shadow(false);

    let close_app = app.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            emit_contextual_assist_dismiss_request(&close_app, "close_requested");
        },
        WindowEvent::Focused(false) => {
            if contextual_assist_is_expanded() {
                // Insertion deliberately resigns this window's key focus to
                // hand it to the user's app; that is not a dismiss-worthy
                // focus loss — the draft must survive both the paste and any
                // abort. The insert command clears the flag when it finishes
                // (and its own success path dismisses the menu).
                if crate::contextual_assist::contextual_insert_in_progress() {
                    return;
                }
                emit_contextual_assist_dismiss_request(&close_app, "focus_lost");
            } else {
                let _ = hide_contextual_assist_window(&close_app);
            }
        },
        _ => {},
    });

    Ok(())
}

/// Create the notification overlay window if it doesn't already exist.
///
/// Modeled on the contextual-assist window: borderless, transparent,
/// always-on-top, passive (non-focusable) by default. It sits ABOVE the
/// draw/tutor overlay (which uses `NSFloatingWindowLevel` = 3 and dims the
/// screen when active) by raising its NSWindow level to `NSStatusWindowLevel`
/// (25) so notification cards always paint above the dim.
///
/// Click-through everywhere else is achieved by keeping the window SMALL —
/// `resize_notify_overlay` hugs the card stack — so the rest of the screen
/// (including the draw canvas) is never under this window and stays fully
/// interactive. We deliberately do NOT `set_ignore_cursor_events(true)`,
/// because the cards themselves must be clickable.
///
/// macOS window creation + NSWindow setters are strictly main-thread, so this
/// hops to the main thread when called from a worker (e.g. `async_setup`).
pub fn initialize_notify_overlay(app: &AppHandle) -> Result<(), String> {
    if app
        .get_webview_window(NOTIFY_OVERLAY_WINDOW_LABEL)
        .is_some()
    {
        return Ok(());
    }
    if current_thread_is_main_thread() {
        return initialize_notify_overlay_on_main(app);
    }

    let (tx, rx) = mpsc::channel();
    let app_for_main = app.clone();
    app.run_on_main_thread(move || {
        let _ = tx.send(initialize_notify_overlay_on_main(&app_for_main));
    })
    .map_err(|error| {
        format!("Failed to schedule notify overlay window creation on main thread: {error}")
    })?;
    rx.recv_timeout(Duration::from_secs(5)).map_err(|error| {
        format!("Timed out waiting for notify overlay window creation on main thread: {error}")
    })?
}

fn initialize_notify_overlay_on_main(app: &AppHandle) -> Result<(), String> {
    if app
        .get_webview_window(NOTIFY_OVERLAY_WINDOW_LABEL)
        .is_some()
    {
        return Ok(());
    }

    let url = overlay_route_webview_url(
        "notify overlay",
        NOTIFY_OVERLAY_DEV_URL,
        NOTIFY_OVERLAY_APP_PATH,
    )?;
    let window = tauri::WebviewWindowBuilder::new(app, NOTIFY_OVERLAY_WINDOW_LABEL, url)
        .title("Magican Notifications")
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .transparent(true)
        .background_color(Color(0, 0, 0, 0))
        .shadow(false)
        .focused(false)
        // Passive by default; toggled `true` (via `set_notify_overlay_focusable`)
        // only when a card needs keyboard input (e.g. an inline approval reply).
        .focusable(false)
        .inner_size(NOTIFY_OVERLAY_WIDTH, NOTIFY_OVERLAY_INIT_HEIGHT)
        .build()
        .map_err(|e| format!("Failed to create notify overlay window: {e}"))?;
    let _ = window.set_shadow(false);

    #[cfg(target_os = "macos")]
    {
        use objc2::msg_send;
        use objc2::runtime::AnyObject;
        if let Ok(ns_window) = window.ns_window() {
            let ns_window = ns_window as *mut AnyObject;
            // ABOVE the draw/tutor overlay (NSFloatingWindowLevel = 3, which dims
            // the screen dark when active) so notification cards always paint
            // above the dim. NSStatusWindowLevel = 25.
            let level: i64 = 25;
            unsafe {
                let _: () = msg_send![ns_window, setLevel: level];
            }
        }
    }

    Ok(())
}

/// Center the HUD on whatever monitor the cursor is currently on.
/// Resolution order:
///   1. Cursor position → containing monitor (the user is here right
///      now; this is where they expect the HUD to appear).
///   2. Window's last-known monitor (fallback if cursor lookup fails).
///   3. Primary monitor (last-resort fallback).
/// The monitor the cursor is currently over, falling back to primary.
///
/// Manual rect containment rather than `monitor_from_point`, which
/// intermittently returns `None` in the owner's multi-display setup even when
/// the cursor is clearly inside a monitor's bounds.
fn cursor_monitor(app: &AppHandle, window: &tauri::WebviewWindow) -> Option<tauri::Monitor> {
    let monitors = app.available_monitors().unwrap_or_default();
    let found = app.cursor_position().ok().and_then(|pos| {
        monitors
            .iter()
            .find(|m| {
                let size = m.size();
                let position = m.position();
                let (mx, my) = (position.x as f64, position.y as f64);
                pos.x >= mx
                    && pos.x < mx + size.width as f64
                    && pos.y >= my
                    && pos.y < my + size.height as f64
            })
            .cloned()
    });
    if found.is_none() {
        warn!("HUD cursor monitor lookup failed; falling back to primary");
    }
    found.or_else(|| window.primary_monitor().ok().flatten())
}

/// Size the transparent stage and centre it on the cursor's monitor.
///
/// The stage is a fixed size, clamped to the monitor so it still fits on a
/// short screen. The themed panel inside is centred by CSS, so the panel
/// re-centres with the stage for free.
fn center_hud_window(app: &AppHandle, window: &tauri::WebviewWindow) {
    let monitor = cursor_monitor(app, window);

    let (mut width, mut height) = (OVERLAY_HUD_WIDTH, OVERLAY_HUD_HEIGHT);
    if let Some(monitor) = monitor.as_ref() {
        let screen = monitor.size().to_logical::<f64>(monitor.scale_factor());
        width = width.min(screen.width * OVERLAY_HUD_MAX_SCREEN_FRACTION);
        height = height.min(screen.height * OVERLAY_HUD_MAX_SCREEN_FRACTION);
    }

    let _ = window.set_size(tauri::LogicalSize::new(width, height));

    if let Some(monitor) = monitor.as_ref() {
        let scale = monitor.scale_factor();
        let screen = monitor.size().to_logical::<f64>(scale);
        let origin = monitor.position().to_logical::<f64>(scale);
        let _ = window.set_position(tauri::LogicalPosition::new(
            origin.x + (screen.width - width) / 2.0,
            origin.y + (screen.height - height) / 2.0,
        ));
    }

    apply_hud_window_level(app, window);
}

/// macOS: `NSFloatingWindowLevel` (3) — above normal app windows but BELOW the
/// menu bar (24) and dock (20), so system affordances stay interactive. An
/// earlier iteration used `NSStatusWindowLevel` (25), which covered the menu
/// bar and blocked every system affordance; the owner pushed back on that.
///
/// AppKit setters are strictly main-thread, and tray handlers may run on a
/// worker, so this hops threads.
fn apply_hud_window_level(app: &AppHandle, window: &tauri::WebviewWindow) {
    #[cfg(target_os = "macos")]
    {
        let window_clone = window.clone();
        let _ = app.run_on_main_thread(move || {
            use objc2::msg_send;
            use objc2::runtime::AnyObject;
            if let Ok(ns_window) = window_clone.ns_window() {
                let ns_window = ns_window as *mut AnyObject;
                let level: i64 = 3; // NSFloatingWindowLevel
                unsafe {
                    let _: () = msg_send![ns_window, setLevel: level];
                }
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, window);
    }
}

fn fill_screen(app: &AppHandle, window: &tauri::WebviewWindow, cursor_monitor_first: bool) {
    // Cursor-driven monitor pick. Manual rect lookup is more
    // reliable than `monitor_from_point` (which intermittently
    // returns None in the user's setup even when the cursor is
    // clearly inside a monitor's bounds). Iterate all monitors and
    // pick the one whose physical rect contains the physical cursor.
    let monitors = app.available_monitors().unwrap_or_default();
    let monitor = if cursor_monitor_first {
        let cursor_pos = app.cursor_position().ok();
        let cursor_monitor = cursor_pos.and_then(|pos| {
            monitors
                .iter()
                .find(|m| {
                    let size = m.size();
                    let position = m.position();
                    let mx = position.x as f64;
                    let my = position.y as f64;
                    let mw = size.width as f64;
                    let mh = size.height as f64;
                    pos.x >= mx && pos.x < mx + mw && pos.y >= my && pos.y < my + mh
                })
                .cloned()
        });
        if let Some(pos) = cursor_pos.as_ref() {
            info!(
                "HUD cursor lookup: cursor at ({}, {}) over {} monitors → found: {}",
                pos.x,
                pos.y,
                monitors.len(),
                cursor_monitor.is_some()
            );
        } else {
            warn!("HUD cursor lookup: app.cursor_position() returned None");
        }
        cursor_monitor.or_else(|| {
            warn!("HUD cursor monitor lookup failed; falling back to primary");
            window.primary_monitor().ok().flatten()
        })
    } else {
        info!("Draw overlay using primary monitor to match screen-capture coordinates");
        window
            .primary_monitor()
            .ok()
            .flatten()
            .or_else(|| monitors.first().cloned())
    };
    if let Some(monitor) = monitor {
        // Cover the full monitor so CSS `100vh`
        // equals the actual visible screen height. The HUD's
        // half-screen outer box and dock offset are then driven by
        // CSS (`50vh`, `bottom: 80px`, etc.), which guarantees the
        // visible chat container is ~50% of the actual screen
        // regardless of any synthetic logical resolution reported by
        // display utilities.
        let monitor_size = monitor.size();
        let monitor_position = monitor.position();
        let _ = window.set_position(tauri::PhysicalPosition::new(
            monitor_position.x,
            monitor_position.y,
        ));
        let _ = window.set_size(tauri::PhysicalSize::new(
            monitor_size.width,
            monitor_size.height,
        ));
        info!(
            "Overlay covers full monitor: {}x{} at ({}, {})",
            monitor_size.width, monitor_size.height, monitor_position.x, monitor_position.y
        );
    }

    // macOS: raise the NSWindow level above the menu bar so the HUD
    // covers every pixel including the area normally claimed by the
    // system menu bar. NSStatusWindowLevel sits above the menu bar
    // (value 25) but below the screen saver. This is what gives the
    // "fully immersed" feel without using Spaces fullscreen.
    //
    // Must hop to the main thread: AppKit/NSWindow APIs are strictly
    // main-thread on macOS. Tray-click handlers may run on a worker
    // thread, and calling NSWindow setters from there silently fails
    // (we hit this with `apply_vibrancy` earlier in Phase 1).
    #[cfg(target_os = "macos")]
    {
        let window_clone = window.clone();
        let _ = app.run_on_main_thread(move || {
            // 1) NSWindow level — `NSFloatingWindowLevel` (3) puts the
            // HUD above normal application windows but BELOW the menu
            // bar (24) and dock (20). That keeps system UI interactive
            // while the HUD is up: user can still click menu-bar items,
            // launch from dock, switch via mission control. An earlier
            // iteration used `NSStatusWindowLevel` (25) which covered
            // the menu bar too — looked more immersive but blocked
            // every system affordance, which the user pushed back on.
            use objc2::msg_send;
            use objc2::runtime::AnyObject;
            if let Ok(ns_window) = window_clone.ns_window() {
                let ns_window = ns_window as *mut AnyObject;
                let level: i64 = 3; // NSFloatingWindowLevel
                unsafe {
                    let _: () = msg_send![ns_window, setLevel: level];
                }
            }

            // 2) NO global vibrancy. The window is `transparent(true)`
            // and that's it — the desktop + other app windows show
            // through sharp, no system-wide blur. Each panel in the
            // HUD page applies its own CSS `backdrop-filter: blur(...)`
            // for a local glass-card feel, so the panels still read as
            // frosted-glass cards but everything between/around them
            // shows the live desktop crisply.
            //
            // Earlier iterations applied NSVisualEffectView with
            // `HudWindow` or `FullScreenUI` material, but those blurred
            // the entire screen behind the HUD which was too heavy.
            // Keeping the import around in case we want to dial in
            // a subtle global tint later via a different material.
            let _ = &window_clone; // silence unused-var when no vibrancy
        });
    }

    #[cfg(not(target_os = "macos"))]
    let _ = app; // unused outside macOS
}

fn contextual_assist_size(expanded: bool) -> (u32, u32) {
    if expanded {
        (CONTEXTUAL_ASSIST_MENU_WIDTH, CONTEXTUAL_ASSIST_MENU_HEIGHT)
    } else {
        (CONTEXTUAL_ASSIST_CHIP_WIDTH, CONTEXTUAL_ASSIST_CHIP_HEIGHT)
    }
}

fn contextual_assist_screen_bounds(monitor: &tauri::Monitor) -> ContextualAssistScreenBounds {
    let position = monitor.position();
    let size = monitor.size();
    ContextualAssistScreenBounds {
        x: position.x as f64,
        y: position.y as f64,
        width: size.width as f64,
        height: size.height as f64,
        scale: monitor.scale_factor().max(1.0),
    }
}

fn contextual_assist_window_placement(
    requested_width: u32,
    requested_height: u32,
    expanded: bool,
    anchor: Option<(f64, f64)>,
    bounds: Option<ContextualAssistScreenBounds>,
) -> ContextualAssistWindowPlacement {
    let offset = if expanded { 18.0 } else { 8.0 };
    let (mut x, mut y) = match anchor {
        Some((anchor_x, anchor_y)) => (anchor_x + offset, anchor_y + offset),
        None => (120.0, 120.0),
    };
    let mut logical_width = requested_width as f64;
    let mut logical_height = requested_height as f64;

    if let Some(bounds) = bounds {
        let scale = bounds.scale.max(1.0);
        let margin_x = contextual_assist_axis_margin(bounds.width);
        let margin_y = contextual_assist_axis_margin(bounds.height);
        let max_physical_width = (bounds.width - (margin_x * 2.0)).max(1.0);
        let max_physical_height = (bounds.height - (margin_y * 2.0)).max(1.0);
        logical_width = logical_width.min(max_physical_width / scale).max(1.0);
        logical_height = logical_height.min(max_physical_height / scale).max(1.0);

        let physical_width = (logical_width * scale).ceil().min(max_physical_width);
        let physical_height = (logical_height * scale).ceil().min(max_physical_height);
        let min_x = bounds.x + margin_x;
        let min_y = bounds.y + margin_y;
        let max_x = bounds.x + bounds.width - margin_x - physical_width;
        let max_y = bounds.y + bounds.height - margin_y - physical_height;
        x = contextual_assist_clamp_origin(x, min_x, max_x);
        y = contextual_assist_clamp_origin(y, min_y, max_y);

        return ContextualAssistWindowPlacement {
            x,
            y,
            logical_width,
            logical_height,
            physical_width,
            physical_height,
        };
    }

    ContextualAssistWindowPlacement {
        x: x.round(),
        y: y.round(),
        logical_width,
        logical_height,
        physical_width: logical_width,
        physical_height: logical_height,
    }
}

fn contextual_assist_centered_window_placement(
    requested_width: u32,
    requested_height: u32,
    center: (f64, f64),
    bounds: Option<ContextualAssistScreenBounds>,
) -> ContextualAssistWindowPlacement {
    let mut logical_width = requested_width as f64;
    let mut logical_height = requested_height as f64;

    if let Some(bounds) = bounds {
        let scale = bounds.scale.max(1.0);
        let margin_x = contextual_assist_axis_margin(bounds.width);
        let margin_y = contextual_assist_axis_margin(bounds.height);
        let max_physical_width = (bounds.width - (margin_x * 2.0)).max(1.0);
        let max_physical_height = (bounds.height - (margin_y * 2.0)).max(1.0);
        logical_width = logical_width.min(max_physical_width / scale).max(1.0);
        logical_height = logical_height.min(max_physical_height / scale).max(1.0);

        let physical_width = (logical_width * scale).ceil().min(max_physical_width);
        let physical_height = (logical_height * scale).ceil().min(max_physical_height);
        let min_x = bounds.x + margin_x;
        let min_y = bounds.y + margin_y;
        let max_x = bounds.x + bounds.width - margin_x - physical_width;
        let max_y = bounds.y + bounds.height - margin_y - physical_height;
        let x = contextual_assist_clamp_origin(center.0 - (physical_width / 2.0), min_x, max_x);
        let y = contextual_assist_clamp_origin(center.1 - (physical_height / 2.0), min_y, max_y);

        return ContextualAssistWindowPlacement {
            x,
            y,
            logical_width,
            logical_height,
            physical_width,
            physical_height,
        };
    }

    ContextualAssistWindowPlacement {
        x: (center.0 - (logical_width / 2.0)).round(),
        y: (center.1 - (logical_height / 2.0)).round(),
        logical_width,
        logical_height,
        physical_width: logical_width,
        physical_height: logical_height,
    }
}

fn contextual_assist_axis_margin(axis_size: f64) -> f64 {
    if axis_size > CONTEXTUAL_ASSIST_SCREEN_MARGIN * 2.0 {
        CONTEXTUAL_ASSIST_SCREEN_MARGIN
    } else {
        0.0
    }
}

fn contextual_assist_clamp_origin(origin: f64, min_origin: f64, max_origin: f64) -> f64 {
    let max_origin = max_origin.max(min_origin);
    let min_integer_origin = min_origin.ceil();
    let max_integer_origin = max_origin.floor();
    if min_integer_origin <= max_integer_origin {
        return origin
            .clamp(min_origin, max_origin)
            .round()
            .clamp(min_integer_origin, max_integer_origin);
    }
    origin.clamp(min_origin, max_origin)
}

fn contextual_assist_target_store(
) -> &'static StdMutex<Option<crate::contextual_assist::NativeAssistTarget>> {
    CONTEXTUAL_ASSIST_TARGET.get_or_init(|| StdMutex::new(None))
}

fn contextual_assist_chip_rect_store() -> &'static StdMutex<Option<ContextualAssistWindowRect>> {
    CONTEXTUAL_ASSIST_CHIP_RECT.get_or_init(|| StdMutex::new(None))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn set_contextual_assist_manual_grace(duration: Duration) {
    let until = now_millis().saturating_add(duration.as_millis() as u64);
    CONTEXTUAL_ASSIST_MANUAL_GRACE_UNTIL_MS.store(until, Ordering::SeqCst);
}

fn clear_contextual_assist_manual_grace() {
    CONTEXTUAL_ASSIST_MANUAL_GRACE_UNTIL_MS.store(0, Ordering::SeqCst);
}

pub fn contextual_assist_manual_grace_active() -> bool {
    let until = CONTEXTUAL_ASSIST_MANUAL_GRACE_UNTIL_MS.load(Ordering::SeqCst);
    until > now_millis()
}

pub fn contextual_assist_is_expanded() -> bool {
    CONTEXTUAL_ASSIST_EXPANDED.load(Ordering::SeqCst)
}

pub fn update_contextual_assist_target(
    app: &AppHandle,
    target: Option<crate::contextual_assist::NativeAssistTarget>,
) {
    if let Ok(mut stored) = contextual_assist_target_store().lock() {
        *stored = target.clone();
    }
    if let Some(target) = target {
        let _ = app.emit(CONTEXTUAL_ASSIST_TARGET_EVENT, target);
    }
}

fn emit_contextual_assist_display(app: &AppHandle, display: &str) {
    let _ = app.emit(
        CONTEXTUAL_ASSIST_DISPLAY_EVENT,
        json!({ "display": display }),
    );
}

fn emit_contextual_assist_dismiss_request(app: &AppHandle, reason: &str) {
    let _ = app.emit(
        CONTEXTUAL_ASSIST_DISMISS_REQUEST_EVENT,
        json!({ "reason": reason }),
    );
}

fn current_contextual_assist_display(app: &AppHandle) -> &'static str {
    let Some(window) = app.get_webview_window(CONTEXTUAL_ASSIST_WINDOW_LABEL) else {
        return "hidden";
    };
    if window.is_visible().unwrap_or(false) {
        if contextual_assist_is_expanded() {
            "menu"
        } else {
            "chip"
        }
    } else {
        "hidden"
    }
}

fn run_contextual_assist_window_update<F>(
    app: &AppHandle,
    reason: &str,
    update: F,
) -> Result<(), String>
where
    F: FnOnce(AppHandle) + Send + 'static,
{
    let app_for_main = app.clone();
    app.run_on_main_thread(move || update(app_for_main))
        .map_err(|error| format!("Failed to update contextual assist window for {reason}: {error}"))
}

pub fn current_contextual_assist_target() -> Option<crate::contextual_assist::NativeAssistTarget> {
    contextual_assist_target_store()
        .lock()
        .ok()
        .and_then(|stored| stored.clone())
}

pub fn handle_contextual_assist_global_click(
    app: &AppHandle,
    screen_x: f64,
    screen_y: f64,
) -> bool {
    if contextual_assist_is_expanded() {
        return false;
    }
    let rect = contextual_assist_chip_rect_store()
        .lock()
        .ok()
        .and_then(|rect| *rect);
    let Some(rect) = rect else {
        return false;
    };
    if !rect.contains(screen_x, screen_y) {
        return false;
    }
    if app
        .get_webview_window(CONTEXTUAL_ASSIST_WINDOW_LABEL)
        .is_none()
    {
        return false;
    }
    if let Err(error) = set_contextual_assist_expanded_window(app, true) {
        warn!("failed to expand contextual assist from native click: {error}");
        return false;
    }
    true
}

fn position_contextual_assist_window(
    app: &AppHandle,
    window: &tauri::WebviewWindow,
    expanded: bool,
) {
    position_contextual_assist_window_at(app, window, expanded, None);
}

fn position_contextual_assist_window_at(
    app: &AppHandle,
    window: &tauri::WebviewWindow,
    expanded: bool,
    anchor: Option<(f64, f64)>,
) {
    let (requested_width, requested_height) = contextual_assist_size(expanded);
    let monitors = app.available_monitors().unwrap_or_default();
    let anchor_pos = anchor
        .map(|(x, y)| tauri::PhysicalPosition { x, y })
        .or_else(|| app.cursor_position().ok());
    let monitor = anchor_pos
        .as_ref()
        .and_then(|pos| {
            monitors
                .iter()
                .find(|monitor| {
                    let size = monitor.size();
                    let position = monitor.position();
                    let x = position.x as f64;
                    let y = position.y as f64;
                    pos.x >= x
                        && pos.x < x + size.width as f64
                        && pos.y >= y
                        && pos.y < y + size.height as f64
                })
                .cloned()
        })
        .or_else(|| window.current_monitor().ok().flatten())
        .or_else(|| window.primary_monitor().ok().flatten())
        .or_else(|| monitors.first().cloned());

    let placement = contextual_assist_window_placement(
        requested_width,
        requested_height,
        expanded,
        anchor_pos.map(|pos| (pos.x, pos.y)),
        monitor.as_ref().map(contextual_assist_screen_bounds),
    );
    let position = tauri::PhysicalPosition::new(placement.x as i32, placement.y as i32);
    let _ = window.set_position(position);
    let _ = window.set_size(tauri::LogicalSize::new(
        placement.logical_width,
        placement.logical_height,
    ));
    let _ = window.set_position(position);
    if let Ok(mut rect) = contextual_assist_chip_rect_store().lock() {
        *rect = if expanded {
            None
        } else {
            Some(ContextualAssistWindowRect {
                x: placement.x,
                y: placement.y,
                width: placement.physical_width,
                height: placement.physical_height,
            })
        };
    }
}

fn position_contextual_assist_window_centered_at(
    app: &AppHandle,
    window: &tauri::WebviewWindow,
    center: (f64, f64),
) {
    let monitors = app.available_monitors().unwrap_or_default();
    let center_pos = tauri::PhysicalPosition {
        x: center.0,
        y: center.1,
    };
    let monitor = monitors
        .iter()
        .find(|monitor| {
            let size = monitor.size();
            let position = monitor.position();
            let x = position.x as f64;
            let y = position.y as f64;
            center_pos.x >= x
                && center_pos.x < x + size.width as f64
                && center_pos.y >= y
                && center_pos.y < y + size.height as f64
        })
        .cloned()
        .or_else(|| window.current_monitor().ok().flatten())
        .or_else(|| window.primary_monitor().ok().flatten())
        .or_else(|| monitors.first().cloned());

    let placement = contextual_assist_centered_window_placement(
        CONTEXTUAL_ASSIST_MENU_WIDTH,
        CONTEXTUAL_ASSIST_MENU_HEIGHT,
        center,
        monitor.as_ref().map(contextual_assist_screen_bounds),
    );
    let position = tauri::PhysicalPosition::new(placement.x as i32, placement.y as i32);
    let _ = window.set_position(position);
    let _ = window.set_size(tauri::LogicalSize::new(
        placement.logical_width,
        placement.logical_height,
    ));
    let _ = window.set_position(position);
    if let Ok(mut rect) = contextual_assist_chip_rect_store().lock() {
        *rect = None;
    }
}

fn fill_screen_with_monitor_transform(
    window: &tauri::WebviewWindow,
    monitor: DrawOverlayMonitorTransform,
) {
    let _ = window.set_position(tauri::PhysicalPosition::new(
        monitor.x as i32,
        monitor.y as i32,
    ));
    let _ = window.set_size(tauri::PhysicalSize::new(
        monitor.width.max(1.0) as u32,
        monitor.height.max(1.0) as u32,
    ));
    info!(
        "Draw overlay covers target monitor: {}x{} at ({}, {})",
        monitor.width, monitor.height, monitor.x, monitor.y
    );
}

pub fn show_overlay_window(app: &AppHandle) -> Result<(), String> {
    ensure_overlay_window(app)?;
    if let Some(window) = app.get_webview_window(OVERLAY_WINDOW_LABEL) {
        // Centre on the monitor under the cursor every time we show — handles
        // multi-display setups where the user moved between displays since the
        // last show. Position is deliberately NOT remembered across summons.
        //
        // Sized BEFORE show() so the WebView lays out at its real size for the
        // first paint rather than reflowing from a stale one.
        center_hud_window(app, &window);
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    // Note: ESC dismiss is handled in the frontend's keydown listener
    // (see `ui/unified-ui/src/routes/hud/+page.svelte`). The
    // earlier implementation registered ESC as a *global* shortcut, but
    // that produced a deadlock on macOS:
    // `handle_overlay_dismiss_shortcut` fires on a Tauri-managed thread
    // and called `hide_overlay_window`, which then tried to unregister
    // the same shortcut whose callback was still running — the unregister
    // would wait for the callback to complete, the callback would wait
    // for unregister, and the WebView hung. Global ESC is also redundant
    // because the overlay window takes focus on `show()`.
    let _ = app.emit(OVERLAY_FOCUS_EVENT, ());
    Ok(())
}

pub fn show_contextual_assist_window(app: &AppHandle) -> Result<(), String> {
    ensure_contextual_assist_window(app)?;
    CONTEXTUAL_ASSIST_EXPANDED.store(false, Ordering::SeqCst);
    set_contextual_assist_manual_grace(Duration::from_secs(5));
    emit_contextual_assist_display(app, "chip");
    run_contextual_assist_window_update(app, "show chip", move |app| {
        let Some(window) = app.get_webview_window(CONTEXTUAL_ASSIST_WINDOW_LABEL) else {
            return;
        };
        let _ = window.set_shadow(false);
        let _ = window.set_background_color(Some(Color(0, 0, 0, 0)));
        let _ = window.set_focusable(false);
        position_contextual_assist_window(&app, &window, false);
        let _ = window.show();
        let _ = window.unminimize();
    })
}

pub fn show_contextual_assist_menu_centered_at_point(
    app: &AppHandle,
    x: f64,
    y: f64,
    target: crate::contextual_assist::NativeAssistTarget,
) -> Result<(), String> {
    ensure_contextual_assist_window(app)?;
    update_contextual_assist_target(app, Some(target));
    clear_contextual_assist_manual_grace();
    CONTEXTUAL_ASSIST_EXPANDED.store(true, Ordering::SeqCst);
    emit_contextual_assist_display(app, "menu");
    run_contextual_assist_window_update(app, "show menu", move |app| {
        let Some(window) = app.get_webview_window(CONTEXTUAL_ASSIST_WINDOW_LABEL) else {
            return;
        };
        let _ = window.set_shadow(false);
        let _ = window.set_background_color(Some(Color(0, 0, 0, 0)));
        let _ = window.set_focusable(true);
        position_contextual_assist_window_centered_at(&app, &window, (x, y));
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    })
}

fn show_draw_overlay_window_on_monitor(
    app: &AppHandle,
    monitor_override: Option<DrawOverlayMonitorTransform>,
) -> Result<(), String> {
    ensure_draw_overlay_window(app)?;
    if let Some(window) = app.get_webview_window(DRAW_OVERLAY_WINDOW_LABEL) {
        if let Some(monitor) = monitor_override {
            fill_screen_with_monitor_transform(&window, monitor);
        } else {
            fill_screen(app, &window, false);
        }
        #[cfg(target_os = "macos")]
        let _ = window.set_ignore_cursor_events(true);
        let _ = window.set_background_color(Some(Color(0, 0, 0, 0)));
        let _ = window.show();
        let _ = window.unminimize();
        let app_clone = app.clone();
        let window_clone = window.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if let Some(monitor) = monitor_override {
                fill_screen_with_monitor_transform(&window_clone, monitor);
            } else {
                fill_screen(&app_clone, &window_clone, false);
            }
            let _ = window_clone.set_ignore_cursor_events(true);
        });
    }
    Ok(())
}

pub fn hide_overlay_window(app: &AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(OVERLAY_WINDOW_LABEL) {
        let _ = window.hide();
    }
    Ok(())
}

pub fn hide_draw_overlay_window(app: &AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(DRAW_OVERLAY_WINDOW_LABEL) {
        let _ = window.hide();
    }
    Ok(())
}

pub fn hide_contextual_assist_window(app: &AppHandle) -> Result<(), String> {
    CONTEXTUAL_ASSIST_EXPANDED.store(false, Ordering::SeqCst);
    clear_contextual_assist_manual_grace();
    update_contextual_assist_target(app, None);
    emit_contextual_assist_display(app, "hidden");
    if let Ok(mut rect) = contextual_assist_chip_rect_store().lock() {
        *rect = None;
    }
    run_contextual_assist_window_update(app, "hide", move |app| {
        let Some(window) = app.get_webview_window(CONTEXTUAL_ASSIST_WINDOW_LABEL) else {
            return;
        };
        let _ = window.hide();
    })
}

pub fn set_contextual_assist_expanded_window(
    app: &AppHandle,
    expanded: bool,
) -> Result<(), String> {
    ensure_contextual_assist_window(app)?;
    let was_expanded = CONTEXTUAL_ASSIST_EXPANDED.load(Ordering::SeqCst);
    CONTEXTUAL_ASSIST_EXPANDED.store(expanded, Ordering::SeqCst);
    if expanded {
        clear_contextual_assist_manual_grace();
    }
    emit_contextual_assist_display(app, if expanded { "menu" } else { "chip" });
    run_contextual_assist_window_update(app, "set expanded", move |app| {
        let Some(window) = app.get_webview_window(CONTEXTUAL_ASSIST_WINDOW_LABEL) else {
            return;
        };
        let _ = window.set_shadow(false);
        let _ = window.set_background_color(Some(Color(0, 0, 0, 0)));
        let _ = window.set_focusable(expanded);
        if expanded && was_expanded {
            if let (Ok(position), Ok(size)) = (window.outer_position(), window.outer_size()) {
                position_contextual_assist_window_centered_at(
                    &app,
                    &window,
                    (
                        position.x as f64 + (size.width as f64 / 2.0),
                        position.y as f64 + (size.height as f64 / 2.0),
                    ),
                );
            } else {
                position_contextual_assist_window(&app, &window, expanded);
            }
        } else {
            position_contextual_assist_window(&app, &window, expanded);
        }
        if expanded {
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
    })
}

/// Hand keyboard focus to (or take it back from) the contextual-assist menu
/// WITHOUT changing its display state. Insertion uses this: the menu must
/// resign key focus so a synthesized paste lands in the user's app, but it
/// stays fully visible — an aborted insert leaves the draft on screen.
/// Re-enabling restores key focus so the preview card stays operable.
pub fn set_contextual_assist_menu_keyboard_focus(
    app: &AppHandle,
    focused: bool,
) -> Result<(), String> {
    run_contextual_assist_window_update(app, "set keyboard focus", move |app| {
        let Some(window) = app.get_webview_window(CONTEXTUAL_ASSIST_WINDOW_LABEL) else {
            return;
        };
        let _ = window.set_focusable(focused);
        if focused {
            let _ = window.set_focus();
        }
    })
}

fn reported_draw_overlay_click_action(
    points: &[DrawOverlayModelPoint],
    controls: &DrawOverlayControlsState,
) -> Option<DrawOverlayClickAction> {
    // Exact ownership always wins before padded hit targets are considered.
    // The 10px visual gap is slightly narrower than two padded regions, so an
    // order-only resolver could otherwise turn a click visibly inside Deeper
    // into Dismiss/Keep Showing. Rect containment is half-open, which also
    // gives a shared boundary to exactly one neighbor.
    let exact = |rect: &Option<DrawOverlayControlRect>| {
        rect.as_ref()
            .is_some_and(|rect| draw_overlay_any_control_rect_contains(points, rect))
    };
    if exact(&controls.keep_showing_rect) {
        return Some(DrawOverlayClickAction::KeepShowing);
    }
    if exact(&controls.dismiss_rect) {
        return Some(DrawOverlayClickAction::Dismiss(None));
    }
    if exact(&controls.deeper_rect) {
        return Some(DrawOverlayClickAction::ExplainDeeper);
    }
    if exact(&controls.replay_rect) {
        return Some(DrawOverlayClickAction::Replay);
    }

    let padded = |rect: &Option<DrawOverlayControlRect>| {
        rect.as_ref()
            .is_some_and(|rect| draw_overlay_any_control_rect_hit(points, rect))
    };
    if padded(&controls.keep_showing_rect) {
        Some(DrawOverlayClickAction::KeepShowing)
    } else if padded(&controls.dismiss_rect) {
        Some(DrawOverlayClickAction::Dismiss(None))
    } else if padded(&controls.deeper_rect) {
        Some(DrawOverlayClickAction::ExplainDeeper)
    } else if padded(&controls.replay_rect) {
        Some(DrawOverlayClickAction::Replay)
    } else {
        None
    }
}

fn resolve_draw_overlay_click_action(
    overlay: &mut OverlaySessionState,
    points: &[DrawOverlayModelPoint],
    monitor: DrawOverlayMonitorTransform,
    screen_x: f64,
    screen_y: f64,
) -> DrawOverlayClickAction {
    prune_expired_active_draw_shapes(overlay);
    let has_active_shapes = !overlay.active_draw_shapes.is_empty();
    let tutor_session_id = overlay
        .tutor_overlay_status
        .as_ref()
        .and_then(|status| status.session_id.clone());
    let tutor_canvas_mode = overlay
        .tutor_overlay_status
        .as_ref()
        .and_then(|status| status.canvas_mode.clone());
    let tutor_rail = overlay
        .tutor_overlay_status
        .as_ref()
        .and_then(|status| status.rail.clone());
    let tutor_working = overlay
        .tutor_overlay_status
        .as_ref()
        .map(|status| status.status == "working")
        .unwrap_or(false);
    let controls = overlay.draw_overlay_controls.clone();
    let has_control_regions = controls.dismiss_rect.is_some()
        || controls.keep_showing_rect.is_some()
        || controls.replay_rect.is_some()
        || controls.deeper_rect.is_some()
        || !controls.action_rects.is_empty();
    if !has_active_shapes && !tutor_working && !has_control_regions && tutor_session_id.is_none() {
        return DrawOverlayClickAction::None;
    }

    let mut action = reported_draw_overlay_click_action(points, &controls).unwrap_or_else(|| {
        if controls.dismiss_rect.is_none()
            && draw_overlay_any_dismiss_button_hit(points, monitor, has_active_shapes)
        {
            DrawOverlayClickAction::Dismiss(None)
        } else if controls.replay_rect.is_none()
            && has_active_shapes
            && draw_overlay_any_replay_button_hit(points, monitor)
        {
            DrawOverlayClickAction::Replay
        } else if tutor_rail.as_deref() == Some("copilot")
            && tutor_working
            && controls
                .action_rects
                .iter()
                .any(|rect| draw_overlay_any_control_rect_hit(points, rect))
        {
            DrawOverlayClickAction::CopilotUserAction {
                session_id: tutor_session_id.clone(),
                screen_x,
                screen_y,
                model_x: points[0].x,
                model_y: points[0].y,
            }
        } else {
            DrawOverlayClickAction::None
        }
    });

    if matches!(action, DrawOverlayClickAction::Dismiss(_)) {
        overlay.tutor_overlay_status = Some(TutorOverlayStatusPayload {
            status: "idle".to_string(),
            session_id: tutor_session_id.clone(),
            canvas_mode: tutor_canvas_mode,
            rail: tutor_rail,
        });
        action = DrawOverlayClickAction::Dismiss(tutor_session_id);
    }
    action
}

pub fn handle_draw_overlay_global_click(app: &AppHandle, screen_x: f64, screen_y: f64) -> bool {
    let Some(monitor) = draw_overlay_monitor_transform_for_point(app, screen_x, screen_y)
        .or_else(|| draw_overlay_monitor_transform(app))
    else {
        return false;
    };
    let points =
        draw_overlay_screen_point_to_model_points_with_transform(&monitor, screen_x, screen_y);
    if points.is_empty() {
        return false;
    }
    let click_action = {
        let state = app.state::<crate::AppState>();
        let Ok(mut overlay) = state.overlay_session.try_lock() else {
            return false;
        };
        resolve_draw_overlay_click_action(&mut overlay, &points, monitor, screen_x, screen_y)
    };
    if click_action == DrawOverlayClickAction::None {
        return false;
    }
    let consume_click = !matches!(
        click_action,
        DrawOverlayClickAction::CopilotUserAction { .. }
    );
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match click_action {
            DrawOverlayClickAction::CopilotUserAction {
                session_id,
                screen_x,
                screen_y,
                model_x,
                model_y,
            } => {
                if let Err(error) = app.emit(
                    OVERLAY_COPILOT_USER_ACTION_EVENT,
                    json!({
                        "session_id": session_id,
                        "screen_x": screen_x,
                        "screen_y": screen_y,
                        "model_x": model_x,
                        "model_y": model_y,
                    }),
                ) {
                    warn!("Failed to emit copilot user action request: {}", error);
                }
            },
            DrawOverlayClickAction::Replay => {
                if let Err(error) = app.emit(OVERLAY_REPLAY_EVENT, json!({})) {
                    warn!("Failed to emit draw overlay replay request: {}", error);
                }
            },
            DrawOverlayClickAction::ExplainDeeper => {
                if let Err(error) = app.emit(OVERLAY_EXPLAIN_DEEPER_EVENT, json!({})) {
                    warn!(
                        "Failed to emit draw overlay explain-deeper request: {}",
                        error
                    );
                }
            },
            DrawOverlayClickAction::KeepShowing => {
                if let Err(error) = app.emit(OVERLAY_KEEP_SHOWING_EVENT, json!({})) {
                    warn!(
                        "Failed to emit draw overlay keep showing request: {}",
                        error
                    );
                }
            },
            DrawOverlayClickAction::Dismiss(session_id) => {
                if let Err(error) = app.emit(
                    OVERLAY_DISMISS_EVENT,
                    json!({
                        "session_id": session_id,
                    }),
                ) {
                    warn!("Failed to emit draw overlay dismiss request: {}", error);
                }
                if let Err(error) = emit_overlay_draw_shape(&app, json!({ "type": "clear" })).await
                {
                    warn!("Failed to clear draw overlay after dismiss: {}", error);
                }
            },
            DrawOverlayClickAction::None => {},
        }
    });
    consume_click
}

#[derive(Debug, Clone, PartialEq)]
enum DrawOverlayClickAction {
    None,
    Replay,
    KeepShowing,
    ExplainDeeper,
    Dismiss(Option<String>),
    CopilotUserAction {
        session_id: Option<String>,
        screen_x: f64,
        screen_y: f64,
        model_x: f64,
        model_y: f64,
    },
}

#[derive(Debug, Clone, Copy)]
struct DrawOverlayModelPoint {
    x: f64,
    y: f64,
}

#[derive(Debug, Clone, Copy)]
struct DrawOverlayMonitorTransform {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale: f64,
    device_scale: f64,
}

#[derive(Debug, Clone, Copy)]
struct DrawOverlayCaptureRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Debug, Clone, Copy)]
struct DrawOverlayImageSize {
    width: f64,
    height: f64,
}

#[derive(Debug, Clone, Copy)]
enum DrawOverlayCoordinateMapper {
    Screen {
        monitor: DrawOverlayMonitorTransform,
    },
    Capture {
        monitor: DrawOverlayMonitorTransform,
        rect: DrawOverlayCaptureRect,
        image: DrawOverlayImageSize,
    },
}

/// Horizontal nudge so the orb parks BESIDE a highlighted shape instead of on
/// top of it, and vertical nudge approximating half the orb panel height so
/// the glide target vertically centers the orb on the anchor.
const MASCOT_DRAW_GLIDE_OFFSET_X: f64 = 24.0;
const MASCOT_DRAW_GLIDE_OFFSET_Y: f64 = 40.0;
/// Storyboard steps arrive as shape batches; one glide per step is presence,
/// more is jitter. Batches inside this window keep the current perch.
const MASCOT_DRAW_GLIDE_MIN_INTERVAL_MS: u64 = 900;

/// Epoch ms of the last storyboard glide (throttle) and whether the orb is
/// currently perched beside a drawn shape (so a `clear` batch docks it home).
static MASCOT_LAST_DRAW_GLIDE_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
static MASCOT_DRAW_GLIDE_ENGAGED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Model-space anchor for one draw-overlay shape: the center of the geometric
/// bounding box across every coordinate convention the tutor shape vocabulary
/// uses (`x/y[+width/height|w/h]`, `cx/cy`, `x1..y2`, `points` as pairs or
/// objects). Shapes reaching this are already normalized into model space by
/// `emit_overlay_draw_shape`. Non-geometric payloads (`clear`,
/// narration-only) yield `None`.
fn draw_shape_model_anchor(shape: &Value) -> Option<(f64, f64)> {
    let field = |key: &str| shape.get(key).and_then(Value::as_f64);
    let mut xs: Vec<f64> = Vec::new();
    let mut ys: Vec<f64> = Vec::new();

    if let Some(points) = shape.get("points").and_then(Value::as_array) {
        for point in points {
            let pair = match point {
                Value::Array(pair) => pair
                    .first()
                    .and_then(Value::as_f64)
                    .zip(pair.get(1).and_then(Value::as_f64)),
                Value::Object(map) => map
                    .get("x")
                    .and_then(Value::as_f64)
                    .zip(map.get("y").and_then(Value::as_f64)),
                _ => None,
            };
            if let Some((x, y)) = pair {
                xs.push(x);
                ys.push(y);
            }
        }
    }
    if let (Some(x1), Some(y1)) = (field("x1"), field("y1")) {
        xs.push(x1);
        ys.push(y1);
        if let (Some(x2), Some(y2)) = (field("x2"), field("y2")) {
            xs.push(x2);
            ys.push(y2);
        }
    }
    if let (Some(cx), Some(cy)) = (field("cx"), field("cy")) {
        xs.push(cx);
        ys.push(cy);
    }
    if let (Some(x), Some(y)) = (field("x"), field("y")) {
        let width = field("width").or_else(|| field("w")).unwrap_or(0.0);
        let height = field("height").or_else(|| field("h")).unwrap_or(0.0);
        xs.push(x);
        ys.push(y);
        xs.push(x + width);
        ys.push(y + height);
    }

    if xs.is_empty() || ys.is_empty() {
        return None;
    }
    let min_x = xs.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_x = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let min_y = ys.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_y = ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    Some(((min_x + max_x) / 2.0, (min_y + max_y) / 2.0))
}

/// Convert a model-space anchor into the NSScreen bottom-left-origin LOGICAL
/// frame origin `glide_mascot_to` expects: invert the screen→model transform
/// (physical pixels), divide by the monitor's device scale (logical points),
/// flip against the primary display's logical height, and nudge so the orb
/// parks beside — not on top of — the highlighted shape. The Swift host
/// clamps the final origin to visible screens.
fn model_anchor_to_mascot_origin(
    transform: &DrawOverlayMonitorTransform,
    model_x: f64,
    model_y: f64,
    primary_logical_height: f64,
) -> (f64, f64) {
    let scale = if transform.scale > 0.0 {
        transform.scale
    } else {
        1.0
    };
    let device_scale = transform.device_scale.max(1.0);
    let physical_x = transform.x + model_x / scale;
    let physical_y = transform.y + model_y / scale;
    let logical_x = physical_x / device_scale;
    let logical_y = physical_y / device_scale;
    (
        logical_x + MASCOT_DRAW_GLIDE_OFFSET_X,
        primary_logical_height - logical_y - MASCOT_DRAW_GLIDE_OFFSET_Y,
    )
}

/// Presence plan Phase 7 embodiment: the orb accompanies the copilot/tutor
/// storyboard — when a draw batch highlights a shape on screen, the mascot
/// glides beside it instead of idling at its dock. Best-effort and throttled;
/// never blocks or fails the draw path.
async fn maybe_glide_mascot_to_draw_anchor(
    app: &AppHandle,
    shapes: &[Value],
    target_monitor: Option<DrawOverlayMonitorTransform>,
) {
    let follows = {
        let state = app.state::<crate::AppState>();
        let config = state.config.lock().await.clone();
        config.host_gateway.mascot_follows_draw
    };
    if !follows {
        return;
    }
    let Some((model_x, model_y)) = shapes.iter().find_map(draw_shape_model_anchor) else {
        return;
    };
    let Some(transform) = target_monitor.or_else(|| draw_overlay_monitor_transform(app)) else {
        return;
    };
    let Some(primary_logical_height) = app
        .primary_monitor()
        .ok()
        .flatten()
        .map(|monitor| monitor.size().height as f64 / monitor.scale_factor().max(1.0))
    else {
        return;
    };

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0);
    let last = MASCOT_LAST_DRAW_GLIDE_MS.load(std::sync::atomic::Ordering::Relaxed);
    if now_ms.saturating_sub(last) < MASCOT_DRAW_GLIDE_MIN_INTERVAL_MS {
        return;
    }
    MASCOT_LAST_DRAW_GLIDE_MS.store(now_ms, std::sync::atomic::Ordering::Relaxed);
    MASCOT_DRAW_GLIDE_ENGAGED.store(true, std::sync::atomic::Ordering::SeqCst);

    let (x, y) =
        model_anchor_to_mascot_origin(&transform, model_x, model_y, primary_logical_height);
    tauri::async_runtime::spawn(async move {
        if let Err(error) = crate::host_gateway::glide_mascot_to(x, y).await {
            warn!("mascot draw-follow glide failed: {}", error);
        }
    });
}

/// When a clear-only batch tears the storyboard down, send a perched orb back
/// to its dock. No-op when the orb never glided (respects user placement).
fn maybe_dock_mascot_after_draw_clear() {
    if MASCOT_DRAW_GLIDE_ENGAGED.swap(false, std::sync::atomic::Ordering::SeqCst) {
        tauri::async_runtime::spawn(async {
            if let Err(error) = crate::host_gateway::dock_mascot().await {
                warn!("mascot draw-follow dock failed: {}", error);
            }
        });
    }
}

fn draw_overlay_monitor_transform(app: &AppHandle) -> Option<DrawOverlayMonitorTransform> {
    let monitor = app
        .get_webview_window(DRAW_OVERLAY_WINDOW_LABEL)
        .and_then(|window| window.current_monitor().ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten())
        .or_else(|| app.available_monitors().ok()?.into_iter().next())?;
    let position = monitor.position();
    let size = monitor.size();
    let mut transform =
        draw_overlay_monitor_transform_from_parts(position.x, position.y, size.width, size.height);
    transform.device_scale = monitor.scale_factor().max(1.0);
    Some(transform)
}

fn draw_overlay_monitor_transform_from_parts(
    position_x: i32,
    position_y: i32,
    width: u32,
    height: u32,
) -> DrawOverlayMonitorTransform {
    let monitor_x = position_x as f64;
    let monitor_y = position_y as f64;
    let monitor_width = width as f64;
    let monitor_height = height as f64;
    let scale = (DRAW_OVERLAY_MODEL_MAX_SIDE / monitor_width.max(monitor_height)).min(1.0);
    DrawOverlayMonitorTransform {
        x: monitor_x,
        y: monitor_y,
        width: monitor_width,
        height: monitor_height,
        scale,
        device_scale: 1.0,
    }
}

fn draw_overlay_monitor_transform_for_point(
    app: &AppHandle,
    screen_x: f64,
    screen_y: f64,
) -> Option<DrawOverlayMonitorTransform> {
    app.available_monitors()
        .ok()?
        .into_iter()
        .find(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            let mut transform = draw_overlay_monitor_transform_from_parts(
                position.x,
                position.y,
                size.width,
                size.height,
            );
            transform.device_scale = monitor.scale_factor().max(1.0);
            screen_x >= transform.x
                && screen_x <= transform.x + transform.width
                && screen_y >= transform.y
                && screen_y <= transform.y + transform.height
                || draw_overlay_screen_point_to_model_point_logical_scaled(
                    &transform, screen_x, screen_y,
                )
                .is_some()
        })
        .map(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            let mut transform = draw_overlay_monitor_transform_from_parts(
                position.x,
                position.y,
                size.width,
                size.height,
            );
            transform.device_scale = monitor.scale_factor().max(1.0);
            transform
        })
}

fn draw_overlay_screen_point_to_model_points_with_transform(
    monitor: &DrawOverlayMonitorTransform,
    screen_x: f64,
    screen_y: f64,
) -> Vec<DrawOverlayModelPoint> {
    let mut points = Vec::with_capacity(2);
    if let Some(point) =
        draw_overlay_screen_point_to_model_point_with_transform(monitor, screen_x, screen_y)
    {
        points.push(point);
    }
    if let Some(point) =
        draw_overlay_screen_point_to_model_point_logical_scaled(monitor, screen_x, screen_y)
    {
        let duplicate = points.iter().any(|existing| {
            (existing.x - point.x).abs() < 0.5 && (existing.y - point.y).abs() < 0.5
        });
        if !duplicate {
            points.push(point);
        }
    }
    points
}

fn draw_overlay_screen_point_to_model_point_with_transform(
    monitor: &DrawOverlayMonitorTransform,
    screen_x: f64,
    screen_y: f64,
) -> Option<DrawOverlayModelPoint> {
    if screen_x < monitor.x
        || screen_x > monitor.x + monitor.width
        || screen_y < monitor.y
        || screen_y > monitor.y + monitor.height
    {
        return None;
    }
    Some(DrawOverlayModelPoint {
        x: (screen_x - monitor.x) * monitor.scale,
        y: (screen_y - monitor.y) * monitor.scale,
    })
}

fn draw_overlay_screen_point_to_model_point_logical_scaled(
    monitor: &DrawOverlayMonitorTransform,
    screen_x: f64,
    screen_y: f64,
) -> Option<DrawOverlayModelPoint> {
    if monitor.device_scale <= 1.0 {
        return None;
    }
    let logical_x = monitor.x / monitor.device_scale;
    let logical_y = monitor.y / monitor.device_scale;
    let logical_width = monitor.width / monitor.device_scale;
    let logical_height = monitor.height / monitor.device_scale;
    if screen_x < logical_x
        || screen_x > logical_x + logical_width
        || screen_y < logical_y
        || screen_y > logical_y + logical_height
    {
        return None;
    }
    Some(DrawOverlayModelPoint {
        x: (screen_x - logical_x) * monitor.device_scale * monitor.scale,
        y: (screen_y - logical_y) * monitor.device_scale * monitor.scale,
    })
}

fn draw_overlay_any_replay_button_hit(
    points: &[DrawOverlayModelPoint],
    monitor: DrawOverlayMonitorTransform,
) -> bool {
    points
        .iter()
        .copied()
        .any(|point| draw_overlay_replay_button_hit(point, monitor))
}

fn draw_overlay_replay_button_hit(
    point: DrawOverlayModelPoint,
    monitor: DrawOverlayMonitorTransform,
) -> bool {
    let model_width = monitor.width * monitor.scale;
    let model_height = monitor.height * monitor.scale;
    let x1 = (model_width - DRAW_OVERLAY_REPLAY_BUTTON_WIDTH - DRAW_OVERLAY_REPLAY_BUTTON_RIGHT)
        .max(0.0);
    let y1 = (model_height - DRAW_OVERLAY_REPLAY_BUTTON_HEIGHT - DRAW_OVERLAY_REPLAY_BUTTON_BOTTOM)
        .max(0.0);
    let x2 = x1 + DRAW_OVERLAY_REPLAY_BUTTON_WIDTH;
    let y2 = y1 + DRAW_OVERLAY_REPLAY_BUTTON_HEIGHT;
    point.x >= x1 - DRAW_OVERLAY_CONTROL_HIT_PADDING
        && point.x <= x2 + DRAW_OVERLAY_CONTROL_HIT_PADDING
        && point.y >= y1 - DRAW_OVERLAY_CONTROL_HIT_PADDING
        && point.y <= y2 + DRAW_OVERLAY_CONTROL_HIT_PADDING
}

fn draw_overlay_control_rect_contains(
    point: DrawOverlayModelPoint,
    rect: &DrawOverlayControlRect,
) -> bool {
    if !rect.x.is_finite()
        || !rect.y.is_finite()
        || !rect.width.is_finite()
        || !rect.height.is_finite()
        || rect.width <= 0.0
        || rect.height <= 0.0
    {
        return false;
    }
    point.x >= rect.x
        && point.x < rect.x + rect.width
        && point.y >= rect.y
        && point.y < rect.y + rect.height
}

fn draw_overlay_control_rect_hit(
    point: DrawOverlayModelPoint,
    rect: &DrawOverlayControlRect,
) -> bool {
    if !rect.x.is_finite()
        || !rect.y.is_finite()
        || !rect.width.is_finite()
        || !rect.height.is_finite()
        || rect.width <= 0.0
        || rect.height <= 0.0
    {
        return false;
    }
    point.x >= rect.x - DRAW_OVERLAY_CONTROL_HIT_PADDING
        && point.x <= rect.x + rect.width + DRAW_OVERLAY_CONTROL_HIT_PADDING
        && point.y >= rect.y - DRAW_OVERLAY_CONTROL_HIT_PADDING
        && point.y <= rect.y + rect.height + DRAW_OVERLAY_CONTROL_HIT_PADDING
}

fn draw_overlay_any_control_rect_hit(
    points: &[DrawOverlayModelPoint],
    rect: &DrawOverlayControlRect,
) -> bool {
    points
        .iter()
        .copied()
        .any(|point| draw_overlay_control_rect_hit(point, rect))
}

fn draw_overlay_any_control_rect_contains(
    points: &[DrawOverlayModelPoint],
    rect: &DrawOverlayControlRect,
) -> bool {
    points
        .iter()
        .copied()
        .any(|point| draw_overlay_control_rect_contains(point, rect))
}

fn draw_overlay_dismiss_button_hit(
    point: DrawOverlayModelPoint,
    monitor: DrawOverlayMonitorTransform,
    replay_reserved: bool,
) -> bool {
    let model_width = monitor.width * monitor.scale;
    let model_height = monitor.height * monitor.scale;
    let x1 = if replay_reserved {
        model_width
            - DRAW_OVERLAY_REPLAY_BUTTON_WIDTH
            - DRAW_OVERLAY_DISMISS_BUTTON_WIDTH
            - DRAW_OVERLAY_CONTROL_BUTTON_GAP
            - DRAW_OVERLAY_REPLAY_BUTTON_RIGHT
    } else {
        model_width - DRAW_OVERLAY_DISMISS_BUTTON_WIDTH - DRAW_OVERLAY_REPLAY_BUTTON_RIGHT
    }
    .max(0.0);
    let y1 = (model_height - DRAW_OVERLAY_REPLAY_BUTTON_HEIGHT - DRAW_OVERLAY_REPLAY_BUTTON_BOTTOM)
        .max(0.0);
    let x2 = x1 + DRAW_OVERLAY_DISMISS_BUTTON_WIDTH;
    let y2 = y1 + DRAW_OVERLAY_REPLAY_BUTTON_HEIGHT;
    point.x >= x1 - DRAW_OVERLAY_CONTROL_HIT_PADDING
        && point.x <= x2 + DRAW_OVERLAY_CONTROL_HIT_PADDING
        && point.y >= y1 - DRAW_OVERLAY_CONTROL_HIT_PADDING
        && point.y <= y2 + DRAW_OVERLAY_CONTROL_HIT_PADDING
}

fn draw_overlay_any_dismiss_button_hit(
    points: &[DrawOverlayModelPoint],
    monitor: DrawOverlayMonitorTransform,
    replay_reserved: bool,
) -> bool {
    points
        .iter()
        .copied()
        .any(|point| draw_overlay_dismiss_button_hit(point, monitor, replay_reserved))
}

pub async fn emit_overlay_draw_shape(app: &AppHandle, shape: Value) -> Result<(), String> {
    let raw_shapes = normalize_overlay_draw_shapes(shape)?;
    let target_monitor = draw_overlay_target_monitor_transform(app, &raw_shapes);
    let shapes = raw_shapes
        .into_iter()
        .map(|shape| normalize_overlay_draw_coordinate_space(app, shape, target_monitor))
        .collect::<Result<Vec<_>, _>>()?;
    let generation = begin_overlay_draw_batch(app).await;
    let has_non_clear = shapes
        .iter()
        .any(|shape| !overlay_draw_shape_is_type(shape, "clear"));
    if !has_non_clear {
        for shape in &shapes {
            schedule_overlay_draw_shape(app.clone(), shape.clone(), generation).await?;
        }
        maybe_dock_mascot_after_draw_clear();
        hide_draw_overlay_window(app)?;
        return Ok(());
    }
    show_draw_overlay_window_on_monitor(app, target_monitor)?;
    schedule_draw_overlay_visibility_reassertion(app.clone(), generation, target_monitor);
    maybe_glide_mascot_to_draw_anchor(app, &shapes, target_monitor).await;
    for shape in shapes {
        schedule_overlay_draw_shape(app.clone(), shape, generation).await?;
    }
    Ok(())
}

fn schedule_draw_overlay_visibility_reassertion(
    app: AppHandle,
    generation: u64,
    target_monitor: Option<DrawOverlayMonitorTransform>,
) {
    for delay_ms in [90_u64, 260_u64] {
        let app_clone = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            let should_show = {
                let state = app_clone.state::<crate::AppState>();
                let mut overlay = state.overlay_session.lock().await;
                prune_expired_active_draw_shapes(&mut overlay);
                overlay.draw_generation == generation && !overlay.active_draw_shapes.is_empty()
            };
            if should_show {
                let _ = show_draw_overlay_window_on_monitor(&app_clone, target_monitor);
            }
        });
    }
}

async fn begin_overlay_draw_batch(app: &AppHandle) -> u64 {
    let state = app.state::<crate::AppState>();
    let mut overlay = state.overlay_session.lock().await;
    overlay.draw_generation = overlay.draw_generation.wrapping_add(1);
    overlay.draw_generation
}

async fn schedule_overlay_draw_shape(
    app: AppHandle,
    shape: Value,
    generation: u64,
) -> Result<(), String> {
    let delay_ms = overlay_draw_shape_delay_ms(&shape);
    if delay_ms == 0 {
        enqueue_overlay_draw_shape(app, shape, generation).await
    } else {
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            if let Err(error) = enqueue_overlay_draw_shape(app, shape, generation).await {
                warn!("Failed to emit delayed draw overlay shape: {}", error);
            }
        });
        Ok(())
    }
}

async fn enqueue_overlay_draw_shape(
    app: AppHandle,
    shape: Value,
    generation: u64,
) -> Result<(), String> {
    let queued = {
        let state = app.state::<crate::AppState>();
        let mut overlay = state.overlay_session.lock().await;
        queue_overlay_draw_shape(&mut overlay, shape.clone(), generation)
    };
    if !queued {
        return Ok(());
    }
    emit_overlay_draw_event_if_current(&app, &shape, generation).await?;
    for retry_delay_ms in [150_u64, 400_u64] {
        let app_clone = app.clone();
        let shape_clone = shape.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(retry_delay_ms)).await;
            let _ = emit_overlay_draw_event_if_current(&app_clone, &shape_clone, generation).await;
        });
    }
    Ok(())
}

fn queue_overlay_draw_shape(
    overlay: &mut OverlaySessionState,
    shape: Value,
    generation: u64,
) -> bool {
    if overlay.draw_generation != generation {
        return false;
    }
    prune_expired_active_draw_shapes(overlay);
    overlay
        .pending_draw_shapes
        .push(overlay_draw_shape_with_generation(
            shape.clone(),
            generation,
        ));
    if overlay_draw_shape_is_type(&shape, "clear")
        || shape
            .get("clear_previous")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        overlay.active_draw_shapes.clear();
        overlay.draw_overlay_controls = DrawOverlayControlsState::default();
    }
    if !overlay_draw_shape_is_type(&shape, "clear") {
        overlay
            .active_draw_shapes
            .push(ActiveOverlayDrawShape::new(shape.clone()));
    }
    true
}

fn overlay_draw_shape_with_generation(mut shape: Value, generation: u64) -> Value {
    if let Some(object) = shape.as_object_mut() {
        object.insert(
            DRAW_OVERLAY_GENERATION_FIELD.to_string(),
            Value::from(generation),
        );
    }
    shape
}

fn drain_pending_overlay_draw_shapes(overlay: &mut OverlaySessionState) -> Vec<Value> {
    let current_generation = overlay.draw_generation;
    std::mem::take(&mut overlay.pending_draw_shapes)
        .into_iter()
        .filter_map(|mut shape| {
            let generation = shape
                .get(DRAW_OVERLAY_GENERATION_FIELD)
                .and_then(Value::as_u64);
            if generation.is_some_and(|generation| generation != current_generation) {
                return None;
            }
            if let Some(object) = shape.as_object_mut() {
                object.remove(DRAW_OVERLAY_GENERATION_FIELD);
            }
            Some(shape)
        })
        .collect()
}

async fn emit_overlay_draw_event_if_current(
    app: &AppHandle,
    shape: &Value,
    generation: u64,
) -> Result<(), String> {
    {
        let state = app.state::<crate::AppState>();
        let overlay = state.overlay_session.lock().await;
        if overlay.draw_generation != generation {
            return Ok(());
        }
    }
    app.emit(OVERLAY_DRAW_EVENT, shape)
        .map_err(|error| format!("failed to emit draw shape: {error}"))?;
    Ok(())
}

fn overlay_draw_shape_delay_ms(shape: &Value) -> u64 {
    if let Some(delay_ms) = shape.get("delay_ms").and_then(Value::as_u64) {
        return scaled_draw_overlay_delay_ms(delay_ms);
    }
    let Some(reveal_order) = shape.get("reveal_order").and_then(Value::as_u64) else {
        return 0;
    };
    if reveal_order <= 1 {
        return 0;
    }
    reveal_order
        .saturating_sub(1)
        .saturating_mul(DRAW_OVERLAY_IMPLICIT_REVEAL_STEP_MS)
        .min(DRAW_OVERLAY_MAX_DELAY_MS)
}

fn scaled_draw_overlay_delay_ms(delay_ms: u64) -> u64 {
    if delay_ms == 0 {
        return 0;
    }
    let scaled = delay_ms
        .saturating_mul(DRAW_OVERLAY_DELAY_SCALE_NUMERATOR)
        .saturating_add(DRAW_OVERLAY_DELAY_SCALE_DENOMINATOR - 1)
        / DRAW_OVERLAY_DELAY_SCALE_DENOMINATOR;
    scaled.min(DRAW_OVERLAY_MAX_DELAY_MS)
}

fn normalize_overlay_draw_shapes(shape: Value) -> Result<Vec<Value>, String> {
    let inherited = Map::new();
    expand_overlay_draw_shape(shape, &inherited, 0)
}

fn normalize_overlay_draw_coordinate_space(
    app: &AppHandle,
    mut shape: Value,
    monitor_override: Option<DrawOverlayMonitorTransform>,
) -> Result<Value, String> {
    let Some(object) = shape.as_object_mut() else {
        return Ok(shape);
    };
    let Some(space) = object
        .get("coordinate_space")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
    else {
        return Ok(shape);
    };
    if matches!(space.as_str(), "model" | "overlay") {
        object.remove("coordinate_space");
        return Ok(shape);
    }

    let monitor = monitor_override
        .or_else(|| draw_overlay_monitor_transform(app))
        .ok_or_else(|| "draw overlay monitor unavailable for coordinate conversion".to_string())?;
    let mapper = match space.as_str() {
        "screen" => DrawOverlayCoordinateMapper::Screen { monitor },
        "capture" => DrawOverlayCoordinateMapper::Capture {
            monitor,
            rect: parse_overlay_capture_rect(object.get("capture_screen_rect"))?,
            image: parse_overlay_capture_image_size(object.get("capture_image_size"))?,
        },
        other => {
            return Err(format!(
                "unsupported draw coordinate_space `{other}`; use `model`, `screen`, or `capture`"
            ));
        },
    };
    transform_overlay_shape_object_coordinates(object, &mapper)?;
    object.remove("coordinate_space");
    object.remove("capture_screen_rect");
    object.remove("capture_image_size");
    Ok(shape)
}

fn draw_overlay_target_monitor_transform(
    app: &AppHandle,
    shapes: &[Value],
) -> Option<DrawOverlayMonitorTransform> {
    shapes.iter().find_map(|shape| {
        let object = shape.as_object()?;
        let space = object
            .get("coordinate_space")
            .and_then(Value::as_str)
            .map(str::trim)
            .map(|value| value.to_ascii_lowercase())?;
        match space.as_str() {
            "capture" => {
                let rect = parse_overlay_capture_rect(object.get("capture_screen_rect")).ok()?;
                draw_overlay_monitor_transform_for_point(
                    app,
                    rect.x + rect.width * 0.5,
                    rect.y + rect.height * 0.5,
                )
            },
            "screen" => draw_overlay_screen_anchor(object)
                .and_then(|(x, y)| draw_overlay_monitor_transform_for_point(app, x, y)),
            _ => None,
        }
    })
}

fn draw_overlay_screen_anchor(object: &Map<String, Value>) -> Option<(f64, f64)> {
    for (x_key, y_key) in [
        ("x", "y"),
        ("cx", "cy"),
        ("x1", "y1"),
        ("from_x", "from_y"),
        ("to_x", "to_y"),
        ("x2", "y2"),
    ] {
        if let (Some(x), Some(y)) = (
            object.get(x_key).and_then(Value::as_f64),
            object.get(y_key).and_then(Value::as_f64),
        ) {
            if x.is_finite() && y.is_finite() {
                return Some((x, y));
            }
        }
    }
    None
}

fn parse_overlay_capture_rect(value: Option<&Value>) -> Result<DrawOverlayCaptureRect, String> {
    let object = value
        .and_then(Value::as_object)
        .ok_or_else(|| "coordinate_space `capture` requires capture_screen_rect".to_string())?;
    let rect = DrawOverlayCaptureRect {
        x: overlay_required_number(object, "x", "capture_screen_rect")?,
        y: overlay_required_number(object, "y", "capture_screen_rect")?,
        width: overlay_required_number(object, "width", "capture_screen_rect")?,
        height: overlay_required_number(object, "height", "capture_screen_rect")?,
    };
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return Err("capture_screen_rect width and height must be positive".to_string());
    }
    Ok(rect)
}

fn parse_overlay_capture_image_size(value: Option<&Value>) -> Result<DrawOverlayImageSize, String> {
    let object = value
        .and_then(Value::as_object)
        .ok_or_else(|| "coordinate_space `capture` requires capture_image_size".to_string())?;
    let image = DrawOverlayImageSize {
        width: overlay_required_number(object, "width", "capture_image_size")?,
        height: overlay_required_number(object, "height", "capture_image_size")?,
    };
    if image.width <= 0.0 || image.height <= 0.0 {
        return Err("capture_image_size width and height must be positive".to_string());
    }
    Ok(image)
}

fn overlay_required_number(
    object: &Map<String, Value>,
    key: &str,
    owner: &str,
) -> Result<f64, String> {
    object
        .get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .ok_or_else(|| format!("{owner}.{key} must be a finite number"))
}

fn transform_overlay_shape_object_coordinates(
    object: &mut Map<String, Value>,
    mapper: &DrawOverlayCoordinateMapper,
) -> Result<(), String> {
    for (x_key, y_key) in [
        ("x", "y"),
        ("cx", "cy"),
        ("x1", "y1"),
        ("x2", "y2"),
        ("from_x", "from_y"),
        ("to_x", "to_y"),
    ] {
        transform_overlay_point_fields(object, x_key, y_key, mapper)?;
    }
    transform_overlay_dimension_field(object, "w", mapper, true);
    transform_overlay_dimension_field(object, "h", mapper, false);
    for key in ["r", "radius", "size"] {
        transform_overlay_scalar_field(object, key, mapper);
    }
    if let Some(points) = object.get_mut("points") {
        transform_overlay_points(points, mapper)?;
    }
    Ok(())
}

fn transform_overlay_point_fields(
    object: &mut Map<String, Value>,
    x_key: &str,
    y_key: &str,
    mapper: &DrawOverlayCoordinateMapper,
) -> Result<(), String> {
    let Some(x) = object.get(x_key).and_then(Value::as_f64) else {
        return Ok(());
    };
    let Some(y) = object.get(y_key).and_then(Value::as_f64) else {
        return Ok(());
    };
    let point = mapper.map_point(x, y).ok_or_else(|| {
        format!("draw coordinate ({x},{y}) maps outside the active draw overlay monitor")
    })?;
    object.insert(x_key.to_string(), json!(point.x));
    object.insert(y_key.to_string(), json!(point.y));
    Ok(())
}

fn transform_overlay_dimension_field(
    object: &mut Map<String, Value>,
    key: &str,
    mapper: &DrawOverlayCoordinateMapper,
    horizontal: bool,
) {
    if let Some(value) = object.get(key).and_then(Value::as_f64) {
        let mapped = if horizontal {
            mapper.map_width(value)
        } else {
            mapper.map_height(value)
        };
        object.insert(key.to_string(), json!(mapped));
    }
}

fn transform_overlay_scalar_field(
    object: &mut Map<String, Value>,
    key: &str,
    mapper: &DrawOverlayCoordinateMapper,
) {
    if let Some(value) = object.get(key).and_then(Value::as_f64) {
        object.insert(key.to_string(), json!(mapper.map_scalar(value)));
    }
}

fn transform_overlay_points(
    points: &mut Value,
    mapper: &DrawOverlayCoordinateMapper,
) -> Result<(), String> {
    let Some(points) = points.as_array_mut() else {
        return Ok(());
    };
    for point in points {
        if let Some(values) = point.as_array_mut() {
            if values.len() < 2 {
                continue;
            }
            let Some(x) = values.first().and_then(Value::as_f64) else {
                continue;
            };
            let Some(y) = values.get(1).and_then(Value::as_f64) else {
                continue;
            };
            let mapped = mapper.map_point(x, y).ok_or_else(|| {
                format!("draw point ({x},{y}) maps outside the active draw overlay monitor")
            })?;
            values[0] = json!(mapped.x);
            values[1] = json!(mapped.y);
        } else if let Some(object) = point.as_object_mut() {
            transform_overlay_point_fields(object, "x", "y", mapper)?;
        }
    }
    Ok(())
}

impl DrawOverlayCoordinateMapper {
    fn map_point(&self, x: f64, y: f64) -> Option<DrawOverlayModelPoint> {
        match self {
            DrawOverlayCoordinateMapper::Screen { monitor } => {
                draw_overlay_screen_point_to_model_point_with_transform(monitor, x, y)
            },
            DrawOverlayCoordinateMapper::Capture {
                monitor,
                rect,
                image,
            } => {
                let screen_x = rect.x + x * (rect.width / image.width);
                let screen_y = rect.y + y * (rect.height / image.height);
                draw_overlay_screen_point_to_model_point_with_transform(monitor, screen_x, screen_y)
            },
        }
    }

    fn map_width(&self, value: f64) -> f64 {
        match self {
            DrawOverlayCoordinateMapper::Screen { monitor } => value * monitor.scale,
            DrawOverlayCoordinateMapper::Capture {
                monitor,
                rect,
                image,
            } => value * (rect.width / image.width) * monitor.scale,
        }
    }

    fn map_height(&self, value: f64) -> f64 {
        match self {
            DrawOverlayCoordinateMapper::Screen { monitor } => value * monitor.scale,
            DrawOverlayCoordinateMapper::Capture {
                monitor,
                rect,
                image,
            } => value * (rect.height / image.height) * monitor.scale,
        }
    }

    fn map_scalar(&self, value: f64) -> f64 {
        (self.map_width(value).abs() + self.map_height(value).abs()) * 0.5
    }
}

fn expand_overlay_draw_shape(
    mut shape: Value,
    inherited: &Map<String, Value>,
    depth: usize,
) -> Result<Vec<Value>, String> {
    if depth > DRAW_OVERLAY_GROUP_MAX_DEPTH {
        return Err(format!(
            "draw group nesting exceeds maximum depth of {DRAW_OVERLAY_GROUP_MAX_DEPTH}"
        ));
    }
    let object = shape
        .as_object_mut()
        .ok_or_else(|| "draw payload must be a JSON object".to_string())?;
    let shape_type = object
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "draw payload requires non-empty `type`".to_string())?
        .to_string();
    if !is_supported_overlay_draw_shape_type(&shape_type) {
        return Err(format!("unsupported draw shape type `{shape_type}`"));
    }
    validate_overlay_draw_shape_geometry(object, &shape_type)?;
    if shape_type == "group" {
        let mut next_inherited = inherited.clone();
        for key in DRAW_OVERLAY_INHERITED_GROUP_KEYS {
            if let Some(value) = object.get(*key).cloned() {
                next_inherited.insert((*key).to_string(), value);
            }
        }
        if !next_inherited.contains_key("group_id") {
            let group_id = object
                .get("id")
                .or_else(|| object.get("group_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| {
                    let sequence = OVERLAY_DRAW_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
                    format!("draw-group-{sequence}")
                });
            next_inherited.insert("group_id".to_string(), Value::String(group_id));
        }
        let children = object
            .remove("shapes")
            .or_else(|| object.remove("children"))
            .ok_or_else(|| "draw group requires `shapes` or `children`".to_string())?;
        let children = children
            .as_array()
            .ok_or_else(|| "draw group `shapes` must be an array".to_string())?;
        if children.is_empty() {
            return Err("draw group must contain at least one child shape".to_string());
        }
        let mut expanded = Vec::new();
        for child in children.iter().cloned() {
            expanded.extend(expand_overlay_draw_shape(
                child,
                &next_inherited,
                depth + 1,
            )?);
        }
        return Ok(expanded);
    }
    for (key, value) in inherited {
        if !object.contains_key(key) {
            object.insert(key.clone(), value.clone());
        }
    }
    if object
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none()
    {
        let sequence = OVERLAY_DRAW_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        object.insert("id".to_string(), Value::String(format!("draw-{sequence}")));
    }
    Ok(vec![shape])
}

fn validate_overlay_draw_shape_geometry(
    object: &Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    validate_overlay_numeric_geometry_fields(object, shape_type)?;
    match shape_type {
        "clear" | "group" => Ok(()),
        "line" | "arrow" | "axis" | "trajectory" | "field_line" | "measurement_tick"
        | "vector_arrow" | "force_arrow" | "component_vector" | "pointer_arrow" | "flow_edge"
        | "side_label" | "square_on_segment" => {
            require_overlay_segment_geometry(object, shape_type)
        },
        "rect" | "highlight" | "mask" | "spotlight" | "free_body_body" | "code_highlight"
        | "stack_frame" | "heap_object" | "state_box" | "flow_node" | "memory_cell" => {
            require_overlay_rect_geometry(object, shape_type)
        },
        "polygon" => require_overlay_points_geometry(object, shape_type),
        "path" => require_overlay_path_geometry(object, shape_type),
        "curve" => require_overlay_curve_geometry(object, shape_type),
        "freehand" => require_overlay_points_geometry(object, shape_type),
        "area_fill" => {
            if object.get("points").is_some() {
                require_overlay_points_geometry(object, shape_type)
            } else {
                require_overlay_rect_geometry(object, shape_type)
            }
        },
        "circle" => require_overlay_circle_geometry(object, shape_type),
        "arc"
        | "angle_marker"
        | "right_angle_marker"
        | "perpendicular_marker"
        | "parallel_marker" => require_overlay_anchor_geometry(object, shape_type),
        "label" | "callout" | "formula" | "unit_label" | "timeline_tick" => {
            require_overlay_anchor_geometry(object, shape_type)
        },
        "handwriting" | "cursive_text" => {
            require_overlay_anchor_geometry(object, shape_type)?;
            require_overlay_text_geometry(object, shape_type)
        },
        _ => Ok(()),
    }
}

fn validate_overlay_numeric_geometry_fields(
    object: &Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    for key in [
        "x",
        "y",
        "w",
        "h",
        "x1",
        "y1",
        "x2",
        "y2",
        "cx",
        "cy",
        "r",
        "radius",
        "size",
        "start_angle",
        "end_angle",
        "from_x",
        "from_y",
        "to_x",
        "to_y",
        "control_x",
        "control_y",
        "control1_x",
        "control1_y",
        "control2_x",
        "control2_y",
        "c1x",
        "c1y",
        "c2x",
        "c2y",
        "font_size",
    ] {
        if let Some(value) = object.get(key) {
            let Some(number) = value.as_f64() else {
                return Err(format!(
                    "draw shape `{shape_type}` field `{key}` must be numeric"
                ));
            };
            if !number.is_finite() {
                return Err(format!(
                    "draw shape `{shape_type}` field `{key}` must be finite"
                ));
            }
            if matches!(key, "w" | "h" | "r" | "radius" | "size" | "font_size") && number < 0.0 {
                return Err(format!(
                    "draw shape `{shape_type}` field `{key}` cannot be negative"
                ));
            }
        }
    }
    if let Some(points) = object.get("points") {
        validate_overlay_points(points, shape_type)?;
    }
    Ok(())
}

fn require_overlay_rect_geometry(
    object: &Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    require_overlay_numeric_fields(object, shape_type, &["x", "y", "w", "h"])
}

fn require_overlay_segment_geometry(
    object: &Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    let has_x1y1 = has_overlay_numeric_fields(object, &["x1", "y1"]);
    let has_from = has_overlay_numeric_fields(object, &["from_x", "from_y"]);
    let has_x2y2 = has_overlay_numeric_fields(object, &["x2", "y2"]);
    let has_to = has_overlay_numeric_fields(object, &["to_x", "to_y"]);
    if (has_x1y1 || has_from) && (has_x2y2 || has_to) {
        return Ok(());
    }
    Err(format!(
        "draw shape `{shape_type}` requires segment coordinates (`x1`,`y1`,`x2`,`y2`) or (`from_x`,`from_y`,`to_x`,`to_y`)"
    ))
}

fn require_overlay_points_geometry(
    object: &Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    if object.get("points").is_some() {
        return Ok(());
    }
    Err(format!(
        "draw shape `{shape_type}` requires non-empty `points`"
    ))
}

fn require_overlay_text_geometry(
    object: &Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    if object
        .get("text")
        .or_else(|| object.get("label"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
    {
        return Ok(());
    }
    Err(format!(
        "draw shape `{shape_type}` requires non-empty `text` or `label`"
    ))
}

fn require_overlay_path_geometry(
    object: &Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    let Some(path) = object
        .get("d")
        .or_else(|| object.get("path"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(format!(
            "draw shape `{shape_type}` requires SVG path data in `d` or `path`"
        ));
    };
    if path.len() > DRAW_OVERLAY_MAX_PATH_CHARS {
        return Err(format!(
            "draw shape `{shape_type}` path data must be {DRAW_OVERLAY_MAX_PATH_CHARS} characters or fewer"
        ));
    }
    if !path.chars().all(is_supported_overlay_svg_path_char) {
        return Err(format!(
            "draw shape `{shape_type}` path data contains unsupported characters"
        ));
    }
    Ok(())
}

fn is_supported_overlay_svg_path_char(ch: char) -> bool {
    ch.is_ascii_digit()
        || matches!(
            ch,
            'M' | 'm'
                | 'Z'
                | 'z'
                | 'L'
                | 'l'
                | 'H'
                | 'h'
                | 'V'
                | 'v'
                | 'C'
                | 'c'
                | 'S'
                | 's'
                | 'Q'
                | 'q'
                | 'T'
                | 't'
                | 'A'
                | 'a'
                | 'E'
                | 'e'
                | ','
                | '.'
                | '-'
                | '+'
                | ' '
                | '\n'
                | '\r'
                | '\t'
        )
}

fn require_overlay_curve_geometry(
    object: &Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    require_overlay_segment_geometry(object, shape_type)?;
    let has_control = has_overlay_numeric_fields(object, &["control_x", "control_y"])
        || has_overlay_numeric_fields(object, &["c1x", "c1y"])
        || has_overlay_numeric_fields(object, &["control1_x", "control1_y"]);
    if has_control {
        return Ok(());
    }
    Err(format!(
        "draw shape `{shape_type}` requires a Bezier control point (`control_x`,`control_y`) or cubic controls (`control1_x`,`control1_y`,`control2_x`,`control2_y`)"
    ))
}

fn require_overlay_circle_geometry(
    object: &Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    if (has_overlay_numeric_fields(object, &["cx", "cy"])
        || has_overlay_numeric_fields(object, &["x", "y"]))
        && (has_overlay_numeric_field(object, "r") || has_overlay_numeric_field(object, "radius"))
    {
        return Ok(());
    }
    Err(format!(
        "draw shape `{shape_type}` requires center (`cx`,`cy`) or (`x`,`y`) plus `r` or `radius`"
    ))
}

fn require_overlay_anchor_geometry(
    object: &Map<String, Value>,
    shape_type: &str,
) -> Result<(), String> {
    if has_overlay_numeric_fields(object, &["x", "y"])
        || has_overlay_numeric_fields(object, &["cx", "cy"])
        || has_overlay_numeric_fields(object, &["x1", "y1"])
        || has_overlay_numeric_fields(object, &["from_x", "from_y"])
        || has_overlay_numeric_fields(object, &["x2", "y2"])
        || has_overlay_numeric_fields(object, &["to_x", "to_y"])
    {
        return Ok(());
    }
    Err(format!(
        "draw shape `{shape_type}` requires an anchor point such as (`x`,`y`), (`cx`,`cy`), or segment coordinates"
    ))
}

fn require_overlay_numeric_fields(
    object: &Map<String, Value>,
    shape_type: &str,
    fields: &[&str],
) -> Result<(), String> {
    if has_overlay_numeric_fields(object, fields) {
        return Ok(());
    }
    Err(format!(
        "draw shape `{shape_type}` requires numeric fields: {}",
        fields.join(", ")
    ))
}

fn has_overlay_numeric_fields(object: &Map<String, Value>, fields: &[&str]) -> bool {
    fields
        .iter()
        .all(|field| has_overlay_numeric_field(object, field))
}

fn has_overlay_numeric_field(object: &Map<String, Value>, field: &str) -> bool {
    object
        .get(field)
        .and_then(Value::as_f64)
        .map(f64::is_finite)
        .unwrap_or(false)
}

fn validate_overlay_points(points: &Value, shape_type: &str) -> Result<(), String> {
    let Some(points) = points.as_array() else {
        return Err(format!("draw shape `{shape_type}` points must be an array"));
    };
    if points.is_empty() {
        return Err(format!("draw shape `{shape_type}` points cannot be empty"));
    }
    for point in points {
        if let Some(values) = point.as_array() {
            if values.len() < 2 {
                return Err(format!(
                    "draw shape `{shape_type}` point arrays must contain x and y"
                ));
            }
            for value in values.iter().take(2) {
                let Some(number) = value.as_f64() else {
                    return Err(format!(
                        "draw shape `{shape_type}` point values must be numeric"
                    ));
                };
                if !number.is_finite() {
                    return Err(format!(
                        "draw shape `{shape_type}` point values must be finite"
                    ));
                }
            }
        } else if let Some(object) = point.as_object() {
            for key in ["x", "y"] {
                let Some(number) = object.get(key).and_then(Value::as_f64) else {
                    return Err(format!(
                        "draw shape `{shape_type}` point objects require numeric `{key}`"
                    ));
                };
                if !number.is_finite() {
                    return Err(format!(
                        "draw shape `{shape_type}` point object `{key}` must be finite"
                    ));
                }
            }
        } else {
            return Err(format!(
                "draw shape `{shape_type}` points must be arrays or objects"
            ));
        }
    }
    Ok(())
}

fn overlay_draw_shape_is_type(shape: &Value, expected_type: &str) -> bool {
    shape
        .get("type")
        .and_then(Value::as_str)
        .map(|value| value == expected_type)
        .unwrap_or(false)
}

fn is_supported_overlay_draw_shape_type(shape_type: &str) -> bool {
    matches!(
        shape_type,
        "clear"
            | "group"
            | "label"
            | "callout"
            | "line"
            | "arrow"
            | "rect"
            | "highlight"
            | "path"
            | "curve"
            | "freehand"
            | "handwriting"
            | "cursive_text"
            | "polygon"
            | "circle"
            | "arc"
            | "mask"
            | "spotlight"
            | "angle_marker"
            | "right_angle_marker"
            | "side_label"
            | "perpendicular_marker"
            | "parallel_marker"
            | "square_on_segment"
            | "area_fill"
            | "measurement_tick"
            | "formula"
            | "vector_arrow"
            | "force_arrow"
            | "component_vector"
            | "axis"
            | "trajectory"
            | "field_line"
            | "free_body_body"
            | "unit_label"
            | "code_highlight"
            | "stack_frame"
            | "heap_object"
            | "pointer_arrow"
            | "state_box"
            | "flow_node"
            | "flow_edge"
            | "timeline_tick"
            | "memory_cell"
    )
}

fn prune_expired_active_draw_shapes(overlay: &mut OverlaySessionState) {
    if overlay.active_draw_shapes.is_empty() {
        return;
    }
    let now = Instant::now();
    if overlay
        .active_draw_shapes
        .iter()
        .any(|active| draw_shape_ttl(&active.shape).is_none())
    {
        return;
    }
    if overlay
        .active_draw_shapes
        .iter()
        .all(|active| active_draw_shape_expired(active, now))
    {
        overlay.active_draw_shapes.clear();
    }
}

fn active_draw_shape_expired(active: &ActiveOverlayDrawShape, now: Instant) -> bool {
    let Some(ttl) = draw_shape_ttl(&active.shape) else {
        return false;
    };
    now.saturating_duration_since(active.created_at) >= ttl
}

fn draw_shape_ttl(shape: &Value) -> Option<Duration> {
    if shape
        .get("persist")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let ttl_ms = shape
        .get("ttl_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DRAW_OVERLAY_DEFAULT_TTL_MS);
    if ttl_ms == 0 {
        return None;
    }
    Some(Duration::from_millis(
        ttl_ms.max(DRAW_OVERLAY_MIN_TTL_MS) + draw_shape_duration_ms(shape),
    ))
}

fn draw_shape_duration_ms(shape: &Value) -> u64 {
    shape
        .get("duration_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DRAW_OVERLAY_DEFAULT_DRAW_DURATION_MS)
        .clamp(
            DRAW_OVERLAY_MIN_DRAW_DURATION_MS,
            DRAW_OVERLAY_MAX_DRAW_DURATION_MS,
        )
}

/// Draw-overlay pull path: draw commands can be emitted before the freshly
/// created draw webview mounts its event listeners, so it drains the queue on
/// mount and deduplicates by shape id.
#[tauri::command]
pub async fn take_overlay_draw_shapes(app: AppHandle) -> Result<Vec<Value>, String> {
    let state = app.state::<crate::AppState>();
    let mut overlay = state.overlay_session.lock().await;
    Ok(drain_pending_overlay_draw_shapes(&mut overlay))
}

#[tauri::command]
pub async fn take_tutor_overlay_status(
    app: AppHandle,
) -> Result<Option<TutorOverlayStatusPayload>, String> {
    let state = app.state::<crate::AppState>();
    let overlay = state.overlay_session.lock().await;
    Ok(overlay.tutor_overlay_status.clone())
}

#[tauri::command]
pub async fn set_draw_overlay_control_regions(
    app: AppHandle,
    dismiss_rect: Option<DrawOverlayControlRect>,
    keep_showing_rect: Option<DrawOverlayControlRect>,
    replay_rect: Option<DrawOverlayControlRect>,
    deeper_rect: Option<DrawOverlayControlRect>,
    action_rects: Option<Vec<DrawOverlayControlRect>>,
) -> Result<(), String> {
    validate_draw_overlay_control_rect(dismiss_rect.as_ref(), "dismiss_rect")?;
    validate_draw_overlay_control_rect(keep_showing_rect.as_ref(), "keep_showing_rect")?;
    validate_draw_overlay_control_rect(replay_rect.as_ref(), "replay_rect")?;
    validate_draw_overlay_control_rect(deeper_rect.as_ref(), "deeper_rect")?;
    let action_rects = action_rects.unwrap_or_default();
    for (index, rect) in action_rects.iter().enumerate() {
        validate_draw_overlay_control_rect(Some(rect), &format!("action_rects[{index}]"))?;
    }
    let state = app.state::<crate::AppState>();
    let mut overlay = state.overlay_session.lock().await;
    overlay.draw_overlay_controls = DrawOverlayControlsState {
        dismiss_rect,
        keep_showing_rect,
        replay_rect,
        deeper_rect,
        action_rects,
    };
    Ok(())
}

fn validate_draw_overlay_control_rect(
    rect: Option<&DrawOverlayControlRect>,
    label: &str,
) -> Result<(), String> {
    let Some(rect) = rect else {
        return Ok(());
    };
    if !rect.x.is_finite()
        || !rect.y.is_finite()
        || !rect.width.is_finite()
        || !rect.height.is_finite()
    {
        return Err(format!(
            "{label} must contain finite x, y, width, and height"
        ));
    }
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return Err(format!("{label} width and height must be positive"));
    }
    Ok(())
}

#[tauri::command]
pub async fn show_tutor_overlay_status(
    app: AppHandle,
    status: String,
    session_id: Option<String>,
    canvas_mode: Option<String>,
    rail: Option<String>,
) -> Result<(), String> {
    let normalized = match status.trim().to_ascii_lowercase().as_str() {
        "working" => "working",
        "idle" | "" => "idle",
        other => {
            return Err(format!(
                "unsupported tutor overlay status `{other}`; expected `working` or `idle`"
            ))
        },
    };
    let canvas_mode = normalize_tutor_overlay_canvas_mode(canvas_mode)?;
    let rail = normalize_tutor_overlay_rail(rail)?;
    if normalized == "working" {
        show_draw_overlay_window_on_monitor(&app, None)?;
    }
    let payload = TutorOverlayStatusPayload {
        status: normalized.to_string(),
        session_id: session_id.and_then(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }),
        canvas_mode,
        rail,
    };
    {
        let state = app.state::<crate::AppState>();
        let mut overlay = state.overlay_session.lock().await;
        overlay.tutor_overlay_status = Some(payload.clone());
    }
    app.emit(TUTOR_OVERLAY_STATUS_EVENT, payload)
        .map_err(|error| format!("failed to emit tutor overlay status: {error}"))?;
    Ok(())
}

fn normalize_tutor_overlay_canvas_mode(
    canvas_mode: Option<String>,
) -> Result<Option<String>, String> {
    let Some(value) = canvas_mode else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match trimmed {
        "screen_overlay" | "blackboard" => Ok(Some(trimmed.to_string())),
        other => Err(format!(
            "unsupported tutor overlay canvas_mode `{other}`; expected `screen_overlay` or `blackboard`"
        )),
    }
}

fn normalize_tutor_overlay_rail(rail: Option<String>) -> Result<Option<String>, String> {
    let Some(value) = rail else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match trimmed {
        "tutor" | "copilot" => Ok(Some(trimmed.to_string())),
        other => Err(format!(
            "unsupported tutor overlay rail `{other}`; expected `tutor` or `copilot`"
        )),
    }
}

#[tauri::command]
pub async fn get_overlay_state(app: AppHandle) -> Result<OverlaySnapshot, String> {
    let client = build_http_client()?;
    let snapshot = build_overlay_snapshot(&app, &client).await;
    emit_overlay_state(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
pub async fn show_overlay(app: AppHandle) -> Result<(), String> {
    show_overlay_window(&app)
}

#[tauri::command]
pub async fn hide_overlay(app: AppHandle) -> Result<(), String> {
    hide_overlay_window(&app)
}

#[tauri::command]
pub async fn show_contextual_assist(app: AppHandle) -> Result<(), String> {
    show_contextual_assist_window(&app)
}

#[tauri::command]
pub async fn set_contextual_assist_expanded(app: AppHandle, expanded: bool) -> Result<(), String> {
    set_contextual_assist_expanded_window(&app, expanded)
}

#[tauri::command]
pub async fn get_contextual_assist_target(
) -> Result<Option<crate::contextual_assist::NativeAssistTarget>, String> {
    Ok(current_contextual_assist_target())
}

#[tauri::command]
pub async fn get_contextual_assist_display(app: AppHandle) -> Result<Value, String> {
    Ok(json!({ "display": current_contextual_assist_display(&app) }))
}

#[tauri::command]
pub async fn hide_contextual_assist(app: AppHandle) -> Result<(), String> {
    hide_contextual_assist_window(&app)
}

/// Sync path to ensure + show the notification overlay window. Both the
/// `show_notify_overlay` async command and the synchronous tray-menu handler
/// ("Show Notifications") call this so the show behavior stays in one place.
pub fn show_notify_overlay_window(app: &AppHandle) -> Result<(), String> {
    initialize_notify_overlay(app)?;
    if let Some(window) = app.get_webview_window(NOTIFY_OVERLAY_WINDOW_LABEL) {
        window.show().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn show_notify_overlay(app: AppHandle) -> Result<(), String> {
    show_notify_overlay_window(&app)
}

#[tauri::command]
pub async fn hide_notify_overlay(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(NOTIFY_OVERLAY_WINDOW_LABEL) {
        window.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Toggle whether the notification overlay can take keyboard focus. Passive
/// (`false`) by default so it never steals focus from the active app; the
/// `/notify-overlay` route flips this to `true` only while a card needs input.
#[tauri::command]
pub async fn set_notify_overlay_focusable(app: AppHandle, focusable: bool) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(NOTIFY_OVERLAY_WINDOW_LABEL) {
        window.set_focusable(focusable).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Resize the overlay to hug the card stack (the route reports its content
/// height) and re-pin it to the bottom-right corner of the primary monitor.
///
/// The bottom edge is fixed (`monitor_height - h - BOTTOM_MARGIN`) so the
/// window grows UPWARD as the stack fills — the route re-reports its content
/// height on every change, so each re-pin keeps the bottom anchored while the
/// top rises. `y` is clamped to `>= SCREEN_MARGIN` so a very tall stack never
/// runs off the top of the screen.
#[tauri::command]
pub async fn resize_notify_overlay(app: AppHandle, height: f64) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(NOTIFY_OVERLAY_WINDOW_LABEL) {
        let h = height.max(1.0);
        window
            .set_size(tauri::LogicalSize::new(NOTIFY_OVERLAY_WIDTH, h))
            .map_err(|e| e.to_string())?;
        if let Ok(Some(monitor)) = window.primary_monitor() {
            let size = monitor.size().to_logical::<f64>(monitor.scale_factor());
            let x = size.width - NOTIFY_OVERLAY_WIDTH - NOTIFY_OVERLAY_SCREEN_MARGIN;
            // Bottom-anchored: grow upward with a fixed bottom edge, clamped so a
            // tall stack never goes off the top.
            let y =
                (size.height - h - NOTIFY_OVERLAY_BOTTOM_MARGIN).max(NOTIFY_OVERLAY_SCREEN_MARGIN);
            window
                .set_position(tauri::LogicalPosition::new(x, y))
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn select_overlay_execution(app: AppHandle, execution_id: String) -> Result<(), String> {
    let execution_id = execution_id.trim().to_string();
    if execution_id.is_empty() {
        return Err("execution_id is required".to_string());
    }

    let state = app.state::<crate::AppState>();
    {
        let mut overlay = state.overlay_session.lock().await;
        if overlay
            .tracked_executions
            .iter()
            .any(|execution| execution.execution_id == execution_id)
        {
            overlay.selected_execution_id = Some(execution_id);
        } else {
            return Err("execution_id is not tracked by the overlay".to_string());
        }
    }

    let client = build_http_client()?;
    let next_snapshot = build_overlay_snapshot(&app, &client).await;
    emit_overlay_state(&app, &next_snapshot);
    Ok(())
}

#[tauri::command]
pub async fn open_dashboard(app: AppHandle) -> Result<(), String> {
    crate::tray::open_app(&app);
    Ok(())
}

#[tauri::command]
pub async fn start_overlay_automation(
    app: AppHandle,
    prompt: String,
) -> Result<OverlayStartResponse, String> {
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err("Prompt cannot be empty".to_string());
    }

    let client = build_http_client()?;
    let snapshot = build_overlay_snapshot(&app, &client).await;
    if snapshot.needs_setup {
        return Err("Magican is not set up yet. Open Setup first.".to_string());
    }
    if !snapshot.service_available {
        return Err(
            "The backend is not reachable yet. Start the container and wait for health to turn green."
                .to_string(),
        );
    }

    let state = app.state::<crate::AppState>();
    let config = state.config.lock().await.clone();
    let url = executions_api_url(&config);
    let body = json!({
        "title": truncate_for_title(&prompt),
        "initial_message": prompt.clone(),
        "skip_planning": true
    });

    let response = crate::magician_auth::authorize(client.post(url))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Failed to start automation: {}", e))?;
    let payload: Value = ensure_success(response, "Failed to start automation")
        .await?
        .json()
        .await
        .map_err(|e| format!("Failed to parse automation start response: {}", e))?;

    let execution_id = payload
        .get("execution_id")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("execution")
                .and_then(|execution| execution.get("id"))
                .and_then(Value::as_str)
        })
        .or_else(|| payload.get("id").and_then(Value::as_str))
        .ok_or_else(|| "Backend response did not include execution id".to_string())?
        .to_string();

    {
        let mut overlay = state.overlay_session.lock().await;
        overlay
            .tracked_executions
            .retain(|execution| execution.execution_id != execution_id);
        overlay.tracked_executions.insert(
            0,
            TrackedOverlayExecution {
                execution_id: execution_id.clone(),
                prompt,
            },
        );
        overlay.selected_execution_id = Some(execution_id.clone());
    }

    let next_snapshot = build_overlay_snapshot(&app, &client).await;
    emit_overlay_state(&app, &next_snapshot);

    Ok(OverlayStartResponse { execution_id })
}

#[tauri::command]
pub async fn submit_overlay_resume(
    app: AppHandle,
    request: OverlayResumeRequest,
) -> Result<(), String> {
    if request.execution_id.trim().is_empty() {
        return Err("execution_id is required".to_string());
    }
    if request.input_type.trim().is_empty() {
        return Err("input_type is required".to_string());
    }

    let client = build_http_client()?;
    let state = app.state::<crate::AppState>();
    let config = state.config.lock().await.clone();
    let endpoint = if should_cancel_overlay_pause_action(&request) {
        "agentic-cancel"
    } else if uses_agentic_continue(request.pause_kind.as_deref()) {
        "agentic-continue"
    } else {
        "agentic-resume"
    };
    let url = format!(
        "{}/execution/{}",
        execution_api_base_url(&config, &request.execution_id),
        endpoint
    );

    let request_body = if endpoint == "agentic-cancel" {
        json!({
            "pause_state_id": request.pause_state_id,
            "plan_id": request.plan_id,
            "step_id": request.step_id,
            "agent_id": request.agent_id,
            "goal_id": request.goal_id,
            "cycle_id": request.cycle_id,
        })
    } else {
        json!({
            "pause_state_id": request.pause_state_id,
            "plan_id": request.plan_id,
            "step_id": request.step_id,
            "input_type": request.input_type,
            "value": request.value,
            "agent_id": request.agent_id,
            "goal_id": request.goal_id,
            "cycle_id": request.cycle_id,
        })
    };

    let response = crate::magician_auth::authorize(client.post(url))
        .json(&request_body)
        .send()
        .await
        .map_err(|e| format!("Failed to submit prompt response: {}", e))?;
    ensure_success(response, "Failed to submit prompt response").await?;

    let next_snapshot = build_overlay_snapshot(&app, &client).await;
    emit_overlay_state(&app, &next_snapshot);
    Ok(())
}

#[tauri::command]
pub async fn submit_overlay_clarification(
    app: AppHandle,
    request: OverlayClarificationSubmitRequest,
) -> Result<(), String> {
    if request.execution_id.trim().is_empty() {
        return Err("execution_id is required".to_string());
    }
    if request.question_id.trim().is_empty() {
        return Err("question_id is required".to_string());
    }
    if request.response_text.trim().is_empty() {
        return Err("response_text cannot be empty".to_string());
    }

    let client = build_http_client()?;
    let state = app.state::<crate::AppState>();
    let config = state.config.lock().await.clone();
    let url = format!(
        "{}/clarify/{}/respond",
        execution_api_base_url(&config, &request.execution_id),
        request.question_id
    );

    let response = crate::magician_auth::authorize(client.post(url))
        .json(&json!({
            "response_text": request.response_text,
        }))
        .send()
        .await
        .map_err(|e| format!("Failed to submit clarification: {}", e))?;
    ensure_success(response, "Failed to submit clarification").await?;

    let next_snapshot = build_overlay_snapshot(&app, &client).await;
    emit_overlay_state(&app, &next_snapshot);
    Ok(())
}

#[tauri::command]
pub async fn resolve_overlay_approval(
    app: AppHandle,
    request: OverlayApprovalResolveRequest,
) -> Result<(), String> {
    let decision = request.decision.trim().to_ascii_lowercase();
    if request.approval_id.trim().is_empty() {
        return Err("approval_id is required".to_string());
    }
    if decision != "approve" && decision != "reject" {
        return Err("decision must be 'approve' or 'reject'".to_string());
    }

    let client = build_http_client()?;
    let state = app.state::<crate::AppState>();
    let config = state.config.lock().await.clone();
    let url = format!(
        "{}/approvals/{}/resolve",
        api_base_url(&config),
        request.approval_id
    );

    let response = crate::magician_auth::authorize(client.post(url))
        .json(&json!({
            "decision": decision,
            "channel": "desktop_overlay",
        }))
        .send()
        .await
        .map_err(|e| format!("Failed to resolve approval: {}", e))?;
    ensure_success(response, "Failed to resolve approval").await?;

    let next_snapshot = build_overlay_snapshot(&app, &client).await;
    emit_overlay_state(&app, &next_snapshot);
    Ok(())
}

pub async fn overlay_monitor(app: AppHandle) {
    let client = match build_http_client() {
        Ok(client) => client,
        Err(e) => {
            warn!("Overlay monitor disabled: {}", e);
            return;
        },
    };

    let mut ticker = interval(Duration::from_secs(OVERLAY_MONITOR_INTERVAL_SECS));
    let mut last_snapshot: Option<OverlaySnapshot> = None;
    let mut last_attention_key: Option<String> = None;

    loop {
        ticker.tick().await;
        let snapshot = build_overlay_snapshot(&app, &client).await;

        if last_snapshot.as_ref() != Some(&snapshot) {
            emit_overlay_state(&app, &snapshot);
            last_snapshot = Some(snapshot.clone());
        }

        let attention_key = snapshot_attention_key(&snapshot);
        if attention_key.is_some() && attention_key != last_attention_key {
            if let Err(e) = show_overlay_window(&app) {
                warn!("Failed to auto-open overlay: {}", e);
            }
        }
        last_attention_key = attention_key;
    }
}

fn snapshot_attention_key(snapshot: &OverlaySnapshot) -> Option<String> {
    if let Some(attention_key) = snapshot.attention_key.as_ref() {
        return Some(attention_key.clone());
    }
    if let Some(prompt) = &snapshot.pending_prompt {
        return Some(format!(
            "prompt:{}:{}",
            prompt.execution_id,
            prompt.pause_state_id.as_deref().unwrap_or_default()
        ));
    }
    if let Some(clarification) = &snapshot.pending_clarification {
        return Some(format!(
            "clarify:{}:{}",
            clarification.execution_id, clarification.question_id
        ));
    }
    snapshot
        .pending_approval
        .as_ref()
        .map(|approval| format!("approval:{}", approval.approval_id))
}

fn emit_overlay_state(app: &AppHandle, snapshot: &OverlaySnapshot) {
    let _ = app.emit(OVERLAY_STATE_EVENT, snapshot);
}

fn build_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(4))
        .build()
        .map_err(|e| format!("Failed to create overlay HTTP client: {}", e))
}

async fn build_overlay_snapshot(app: &AppHandle, client: &reqwest::Client) -> OverlaySnapshot {
    let status =
        crate::commands::get_status(app.clone())
            .await
            .unwrap_or(crate::commands::StatusResponse {
                container_running: false,
                container_status: "Unknown".to_string(),
                runtime_name: "none".to_string(),
                image: String::new(),
                health: "unknown".to_string(),
                needs_setup: true,
                runtime_managed: true,
            });

    let state = app.state::<crate::AppState>();
    let (config, tracked_executions, registered_shortcut, selected_execution_id) = {
        let config = state.config.lock().await.clone();
        let overlay = state.overlay_session.lock().await;
        (
            config,
            overlay.tracked_executions.clone(),
            overlay.registered_shortcut.clone(),
            overlay.selected_execution_id.clone(),
        )
    };

    // When the desktop is *managing* the container runtime, we gate on
    // both `container_running` and the HTTP healthcheck. In local-dev
    // mode (`MAGICIAN_DESKTOP_MANAGE_RUNTIME=0`, set by `make run-all`)
    // there's no container — the magician/magicutor/supervisor binaries
    // are launched directly. In that mode the container check is
    // meaningless and would force the overlay to permanently read
    // "Waiting for Magician" even though the API is responsive, so we
    // trust the HTTP probe alone.
    let manage_runtime = crate::manage_runtime_stack_enabled(&config);
    let service_available = if manage_runtime {
        if status.container_running && !status.needs_setup {
            is_service_available(client, &config).await
        } else {
            false
        }
    } else {
        is_service_available(client, &config).await
    };

    // In local-dev mode the runtime/container fields are meaningless —
    // there's no container to start. Pretend the runtime is set up and
    // running so the overlay doesn't surface "Setup required" /
    // "Container is stopped" affordances that would dead-end the user.
    let (effective_needs_setup, effective_container_running, effective_container_status) =
        if manage_runtime {
            (
                status.needs_setup,
                status.container_running,
                status.container_status,
            )
        } else {
            (false, true, "ExternallyManaged".to_string())
        };

    let mut snapshot = OverlaySnapshot {
        shortcut: registered_shortcut.unwrap_or_else(|| {
            crate::config::normalize_overlay_shortcut(&config.general.quick_overlay_shortcut)
        }),
        needs_setup: effective_needs_setup,
        container_running: effective_container_running,
        container_status: effective_container_status,
        service_available,
        attention_key: None,
        active_execution_id: None,
        active_prompt: None,
        active_execution_status: None,
        can_start: service_available,
        tracked_executions: Vec::new(),
        pending_prompt: None,
        pending_clarification: None,
        pending_approval: None,
        pending_approval_count: 0,
    };

    if service_available {
        let mut execution_states = Vec::with_capacity(tracked_executions.len());
        for tracked in &tracked_executions {
            execution_states.push(fetch_overlay_execution_state(client, &config, tracked).await);
        }

        let active_execution_states = execution_states
            .into_iter()
            .filter(overlay_execution_is_active)
            .collect::<Vec<_>>();
        let next_tracked_executions = active_execution_states
            .iter()
            .map(|state| state.tracked.clone())
            .collect::<Vec<_>>();

        if tracked_executions != next_tracked_executions {
            let mut overlay = state.overlay_session.lock().await;
            overlay.tracked_executions = next_tracked_executions;
        }

        let selected_present = selected_execution_id.as_deref().and_then(|execution_id| {
            active_execution_states
                .iter()
                .find(|state| state.tracked.execution_id == execution_id)
        });
        let display_execution =
            select_display_execution(selected_execution_id.as_deref(), &active_execution_states)
                .or_else(|| select_primary_execution(&active_execution_states));
        let attention_execution = select_attention_execution(&active_execution_states);

        snapshot.tracked_executions = summarize_tracked_executions(&active_execution_states);
        if let Some(execution) = display_execution {
            snapshot.active_execution_id = Some(execution.tracked.execution_id.clone());
            snapshot.active_prompt = Some(execution.tracked.prompt.clone());
            snapshot.active_execution_status = execution.status.clone();
            snapshot.pending_prompt = execution.pending_prompt.clone();
            snapshot.pending_clarification = if snapshot.pending_prompt.is_none() {
                execution.pending_clarification.clone()
            } else {
                None
            };
        }
        snapshot.attention_key = attention_execution
            .and_then(execution_attention_key)
            .or_else(|| {
                snapshot
                    .pending_approval
                    .as_ref()
                    .map(|approval| format!("approval:{}", approval.approval_id))
            });

        if let Ok((approval, count)) = fetch_pending_approvals(client, &config).await {
            snapshot.pending_approval =
                if snapshot.pending_prompt.is_none() && snapshot.pending_clarification.is_none() {
                    approval
                } else {
                    None
                };
            snapshot.pending_approval_count = count;
            if snapshot.attention_key.is_none() {
                snapshot.attention_key = snapshot
                    .pending_approval
                    .as_ref()
                    .map(|approval| format!("approval:{}", approval.approval_id));
            }
        }

        let selected_is_actionable = selected_present
            .map(|execution| {
                execution.pending_prompt.is_some() || execution.pending_clarification.is_some()
            })
            .unwrap_or(false);
        let should_update_selected = match selected_execution_id.as_deref() {
            Some(selected_execution_id) if !selected_is_actionable => {
                selected_present.is_none()
                    || attention_execution
                        .map(|execution| execution.tracked.execution_id.as_str())
                        .is_some_and(|execution_id| execution_id != selected_execution_id)
            },
            _ => false,
        };
        if should_update_selected {
            let mut overlay = state.overlay_session.lock().await;
            overlay.selected_execution_id =
                attention_execution.map(|execution| execution.tracked.execution_id.clone());
        }
    }

    snapshot
}

async fn fetch_overlay_execution_state(
    client: &reqwest::Client,
    config: &crate::config::MagicianDesktopConfig,
    tracked: &TrackedOverlayExecution,
) -> OverlayExecutionRuntimeState {
    let (status, pending_prompt, pending_clarification) = tokio::join!(
        fetch_execution_status(client, config, &tracked.execution_id),
        fetch_pause_prompt(client, config, &tracked.execution_id),
        fetch_pending_clarification(client, config, &tracked.execution_id),
    );

    OverlayExecutionRuntimeState {
        tracked: tracked.clone(),
        status: status.ok().flatten(),
        pending_prompt: pending_prompt.ok().flatten(),
        pending_clarification: pending_clarification.ok().flatten(),
    }
}

fn overlay_execution_is_active(state: &OverlayExecutionRuntimeState) -> bool {
    match state.status.as_deref() {
        Some(status) => !is_terminal_status(status),
        None => true,
    }
}

fn select_primary_execution(
    states: &[OverlayExecutionRuntimeState],
) -> Option<&OverlayExecutionRuntimeState> {
    states
        .iter()
        .find(|state| state.pending_prompt.is_some())
        .or_else(|| {
            states
                .iter()
                .find(|state| state.pending_clarification.is_some())
        })
        .or_else(|| states.first())
}

fn select_attention_execution(
    states: &[OverlayExecutionRuntimeState],
) -> Option<&OverlayExecutionRuntimeState> {
    states
        .iter()
        .find(|state| state.pending_prompt.is_some())
        .or_else(|| {
            states
                .iter()
                .find(|state| state.pending_clarification.is_some())
        })
}

fn select_display_execution<'a>(
    selected_execution_id: Option<&str>,
    states: &'a [OverlayExecutionRuntimeState],
) -> Option<&'a OverlayExecutionRuntimeState> {
    let selected = selected_execution_id.and_then(|execution_id| {
        states
            .iter()
            .find(|state| state.tracked.execution_id == execution_id)
    });

    if let Some(state) = selected {
        if state.pending_prompt.is_some() || state.pending_clarification.is_some() {
            return Some(state);
        }
    }

    select_attention_execution(states)
        .or(selected)
        .or_else(|| states.first())
}

fn execution_attention_key(state: &OverlayExecutionRuntimeState) -> Option<String> {
    if let Some(prompt) = state.pending_prompt.as_ref() {
        return Some(format!(
            "prompt:{}:{}",
            prompt.execution_id,
            prompt.pause_state_id.as_deref().unwrap_or_default()
        ));
    }
    state.pending_clarification.as_ref().map(|clarification| {
        format!(
            "clarify:{}:{}",
            clarification.execution_id, clarification.question_id
        )
    })
}

fn summarize_tracked_executions(
    states: &[OverlayExecutionRuntimeState],
) -> Vec<OverlayTrackedExecutionSummary> {
    states
        .iter()
        .map(|state| {
            let (waiting_on, waiting_question) = if let Some(prompt) = state.pending_prompt.as_ref()
            {
                (Some("input".to_string()), Some(prompt.question.clone()))
            } else if let Some(clarification) = state.pending_clarification.as_ref() {
                (
                    Some("clarification".to_string()),
                    Some(clarification.question_text.clone()),
                )
            } else {
                (None, None)
            };

            OverlayTrackedExecutionSummary {
                execution_id: state.tracked.execution_id.clone(),
                prompt: state.tracked.prompt.clone(),
                status: state.status.clone(),
                waiting_on,
                waiting_question,
            }
        })
        .collect()
}

fn api_base_url(config: &crate::config::MagicianDesktopConfig) -> String {
    config.engine_url("/api/magician/v2")
}

fn executions_api_url(config: &crate::config::MagicianDesktopConfig) -> String {
    format!("{}/executions", api_base_url(config))
}

fn execution_api_base_url(
    config: &crate::config::MagicianDesktopConfig,
    execution_id: &str,
) -> String {
    format!("{}/executions/{}", api_base_url(config), execution_id)
}

fn health_url(config: &crate::config::MagicianDesktopConfig) -> String {
    config.engine_url("/health")
}

async fn is_service_available(
    client: &reqwest::Client,
    config: &crate::config::MagicianDesktopConfig,
) -> bool {
    match client.get(health_url(config)).send().await {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    }
}

async fn fetch_execution_status(
    client: &reqwest::Client,
    config: &crate::config::MagicianDesktopConfig,
    execution_id: &str,
) -> Result<Option<String>, String> {
    let url = format!("{}/status", execution_api_base_url(config, execution_id));
    let response = crate::magician_auth::authorize(client.get(url))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch execution status: {}", e))?;
    if !response.status().is_success() {
        return Ok(None);
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse execution status: {}", e))?;
    Ok(value_as_string(
        payload
            .get("waiting_state")
            .or_else(|| payload.get("status")),
    ))
}

async fn fetch_pause_prompt(
    client: &reqwest::Client,
    config: &crate::config::MagicianDesktopConfig,
    execution_id: &str,
) -> Result<Option<OverlayPrompt>, String> {
    let url = format!(
        "{}/pause-state",
        execution_api_base_url(config, execution_id)
    );
    let response = crate::magician_auth::authorize(client.get(url))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch pause state: {}", e))?;
    if !response.status().is_success() {
        return Ok(None);
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse pause state: {}", e))?;
    if !payload
        .get("has_pause_state")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(None);
    }

    let Some(active) = payload.get("active_pause_state") else {
        return Ok(None);
    };
    let input_type = active
        .get("input_type")
        .and_then(parse_input_type_name)
        .unwrap_or_else(|| "text".to_string());
    let (confirm_label, deny_label) = parse_confirmation_labels(active.get("input_type"));

    Ok(Some(OverlayPrompt {
        execution_id: execution_id.to_string(),
        pause_state_id: value_as_string(active.get("pause_state_id")),
        plan_id: value_as_string(active.get("pause_state").and_then(|v| v.get("plan_id"))),
        step_id: value_as_string(active.get("pause_state").and_then(|v| v.get("step_id"))),
        input_type,
        question: value_as_string(active.get("question")).unwrap_or_default(),
        hint: value_as_string(active.get("hint")),
        options: parse_options(active.get("options")),
        is_retry: active
            .get("is_retry")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        retry_count: active
            .get("retry_count")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize,
        previous_answer: value_as_string(active.get("previous_answer")),
        retry_reason: value_as_string(active.get("retry_reason")),
        confirm_label,
        deny_label,
        pause_kind: value_as_string(active.get("pause_kind")),
        agent_id: value_as_string(active.get("pause_state").and_then(|v| v.get("agent_id"))),
        goal_id: value_as_string(active.get("pause_state").and_then(|v| v.get("goal_id"))),
        cycle_id: value_as_string(active.get("pause_state").and_then(|v| v.get("cycle_id"))),
    }))
}

fn uses_agentic_continue(pause_kind: Option<&str>) -> bool {
    pause_kind
        .map(|value| value.trim().to_ascii_lowercase())
        .map(|value| {
            matches!(
                value.as_str(),
                "maxiterations" | "max_iterations" | "manual"
            )
        })
        .unwrap_or(false)
}

fn should_cancel_overlay_pause_action(request: &OverlayResumeRequest) -> bool {
    if !uses_agentic_continue(request.pause_kind.as_deref()) {
        return false;
    }

    let Some(value_type) = request
        .value
        .get("type")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
    else {
        return false;
    };

    match value_type.as_str() {
        "aborted" => true,
        "confirmation" => request.value.get("confirmed").and_then(Value::as_bool) == Some(false),
        _ => false,
    }
}

async fn fetch_pending_clarification(
    client: &reqwest::Client,
    config: &crate::config::MagicianDesktopConfig,
    execution_id: &str,
) -> Result<Option<OverlayClarification>, String> {
    let url = format!(
        "{}/clarify/pending",
        execution_api_base_url(config, execution_id)
    );
    let response = crate::magician_auth::authorize(client.get(url))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch clarifications: {}", e))?;
    if !response.status().is_success() {
        return Ok(None);
    }

    let payload: Vec<Value> = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse clarifications: {}", e))?;
    let Some(first) = payload.first() else {
        return Ok(None);
    };

    Ok(Some(OverlayClarification {
        execution_id: execution_id.to_string(),
        question_id: value_as_string(first.get("id")).unwrap_or_default(),
        question_text: value_as_string(first.get("question_text")).unwrap_or_default(),
        context_snippets: first
            .get("context_snippets")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        options: parse_options(first.get("options")),
        urgency: first.get("urgency").and_then(Value::as_f64).unwrap_or(0.0),
        source_slot_id: value_as_string(first.get("source_slot_id")),
        batch_id: value_as_string(first.get("batch_id")),
        batch_total: first
            .get("batch_total")
            .and_then(Value::as_u64)
            .map(|value| value as usize),
    }))
}

async fn fetch_pending_approvals(
    client: &reqwest::Client,
    config: &crate::config::MagicianDesktopConfig,
) -> Result<(Option<OverlayApproval>, usize), String> {
    let url = format!("{}/approvals?status=pending", api_base_url(config));
    let response = crate::magician_auth::authorize(client.get(url))
        .send()
        .await
        .map_err(|e| format!("Failed to fetch approvals: {}", e))?;
    if !response.status().is_success() {
        return Ok((None, 0));
    }

    let payload: Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse approvals: {}", e))?;
    let approvals = payload
        .get("approvals")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let Some(first) = approvals.first() else {
        return Ok((None, 0));
    };

    let approval = OverlayApproval {
        approval_id: value_as_string(first.get("approval_id")).unwrap_or_default(),
        agent_id: value_as_string(first.get("agent_id")).unwrap_or_default(),
        goal_id: value_as_string(first.get("goal_id")).unwrap_or_default(),
        cycle_id: value_as_string(first.get("cycle_id")).unwrap_or_default(),
        expires_at: value_as_string(first.get("expires_at")),
        pending_actions: first
            .get("pending_actions")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|item| OverlayPendingAction {
                        step_id: value_as_string(item.get("step_id")).unwrap_or_default(),
                        action_description: value_as_string(item.get("action_description"))
                            .unwrap_or_default(),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    };

    Ok((Some(approval), approvals.len()))
}

fn parse_input_type_name(value: &Value) -> Option<String> {
    match value {
        Value::String(raw) => Some(normalize_input_type_name(raw)),
        Value::Object(map) => map.keys().next().map(|raw| normalize_input_type_name(raw)),
        _ => None,
    }
}

fn parse_confirmation_labels(value: Option<&Value>) -> (Option<String>, Option<String>) {
    let Some(Value::Object(map)) = value else {
        return (None, None);
    };
    let Some((variant, payload)) = map.iter().next() else {
        return (None, None);
    };
    if normalize_input_type_name(variant) != "confirmation" {
        return (None, None);
    }
    let Value::Object(payload) = payload else {
        return (None, None);
    };

    (
        value_as_string(payload.get("confirm_label")),
        value_as_string(payload.get("deny_label")),
    )
}

fn normalize_input_type_name(raw: &str) -> String {
    let lowered = raw.to_ascii_lowercase().replace('-', "_");
    match lowered.as_str() {
        "multichoice" => "multi_choice".to_string(),
        "externalaction" => "external_action".to_string(),
        "filepath" => "file_path".to_string(),
        other => other.to_string(),
    }
}

fn parse_options(value: Option<&Value>) -> Vec<OverlayOption> {
    match value {
        Some(Value::String(raw)) => serde_json::from_str::<Vec<Value>>(raw)
            .map(|items| items.iter().filter_map(parse_option_value).collect())
            .unwrap_or_default(),
        Some(Value::Array(items)) => items.iter().filter_map(parse_option_value).collect(),
        _ => Vec::new(),
    }
}

fn parse_option_value(value: &Value) -> Option<OverlayOption> {
    let id = value_as_string(value.get("id").or_else(|| value.get("value")))?;
    Some(OverlayOption {
        label: value_as_string(value.get("label")).unwrap_or_else(|| id.clone()),
        description: value_as_string(value.get("description")),
        id,
    })
}

fn value_as_string(value: Option<&Value>) -> Option<String> {
    value.and_then(|value| match value {
        Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    })
}

fn truncate_for_title(prompt: &str) -> String {
    let trimmed = prompt.trim();
    if trimmed.chars().count() <= 72 {
        return trimmed.to_string();
    }

    let mut result = String::new();
    for ch in trimmed.chars().take(69) {
        result.push(ch);
    }
    result.push_str("...");
    result
}

fn is_terminal_status(status: &str) -> bool {
    let normalized = status.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    matches!(
        normalized.as_str(),
        "completed" | "complete" | "failed" | "cancelled" | "canceled" | "aborted" | "done"
    )
}

async fn ensure_success(
    response: reqwest::Response,
    default_message: &str,
) -> Result<reqwest::Response, String> {
    if response.status().is_success() {
        return Ok(response);
    }

    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if text.trim().is_empty() {
        Err(format!("{} ({})", default_message, status))
    } else {
        Err(format!("{} ({}): {}", default_message, status, text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 0.0001,
            "expected {actual} to be close to {expected}"
        );
    }

    // === Mascot follows the draw-overlay storyboard (copilot embodiment) ===
    //
    // Shapes reaching these helpers are already normalized into the overlay's
    // model space by `emit_overlay_draw_shape`. The anchor is the geometric
    // bounding-box center across every coordinate convention the tutor shape
    // vocabulary uses; the origin conversion inverts the screen→model
    // transform, converts to logical points, and flips into the NSScreen
    // bottom-left space `glide_mascot_to` expects.

    #[test]
    fn draw_shape_model_anchor_uses_rect_bbox_center() {
        let shape = serde_json::json!({
            "type": "rect", "x": 100.0, "y": 50.0, "width": 200.0, "height": 100.0
        });
        let (x, y) = draw_shape_model_anchor(&shape).expect("rect must anchor");
        assert_close(x, 200.0);
        assert_close(y, 100.0);
    }

    #[test]
    fn draw_shape_model_anchor_handles_circle_line_and_points() {
        let circle = serde_json::json!({ "type": "circle", "cx": 300.0, "cy": 400.0, "r": 20.0 });
        let (x, y) = draw_shape_model_anchor(&circle).expect("circle must anchor");
        assert_close(x, 300.0);
        assert_close(y, 400.0);

        let line = serde_json::json!({ "type": "arrow", "x1": 10.0, "y1": 20.0, "x2": 110.0, "y2": 220.0 });
        let (x, y) = draw_shape_model_anchor(&line).expect("line must anchor");
        assert_close(x, 60.0);
        assert_close(y, 120.0);

        let path = serde_json::json!({
            "type": "path",
            "points": [[0.0, 0.0], {"x": 100.0, "y": 60.0}, [50.0, 120.0]]
        });
        let (x, y) = draw_shape_model_anchor(&path).expect("points must anchor");
        assert_close(x, 50.0);
        assert_close(y, 60.0);
    }

    #[test]
    fn draw_shape_model_anchor_skips_non_geometric_shapes() {
        assert!(draw_shape_model_anchor(&serde_json::json!({ "type": "clear" })).is_none());
        assert!(draw_shape_model_anchor(
            &serde_json::json!({ "type": "narration", "text": "watch this" })
        )
        .is_none());
    }

    #[test]
    fn model_anchor_converts_to_flipped_logical_mascot_origin() {
        // 4K monitor at physical origin, retina (device_scale 2), model space
        // 2048-wide: scale = 2048/3840. Model (1024, 512) → physical
        // (1920, 960) → logical (960, 480) → ns-flip against 1080 with the
        // beside-the-shape offsets applied.
        let transform = DrawOverlayMonitorTransform {
            x: 0.0,
            y: 0.0,
            width: 3840.0,
            height: 2160.0,
            scale: DRAW_OVERLAY_MODEL_MAX_SIDE / 3840.0,
            device_scale: 2.0,
        };
        let (x, y) = model_anchor_to_mascot_origin(&transform, 1024.0, 512.0, 1080.0);
        assert_close(x, 960.0 + MASCOT_DRAW_GLIDE_OFFSET_X);
        assert_close(y, 1080.0 - 480.0 - MASCOT_DRAW_GLIDE_OFFSET_Y);
    }

    #[test]
    fn model_anchor_origin_accounts_for_secondary_monitor_offset() {
        // Secondary monitor to the right of a 3840-physical-wide primary.
        let transform = DrawOverlayMonitorTransform {
            x: 3840.0,
            y: 0.0,
            width: 3840.0,
            height: 2160.0,
            scale: DRAW_OVERLAY_MODEL_MAX_SIDE / 3840.0,
            device_scale: 2.0,
        };
        let (x, _) = model_anchor_to_mascot_origin(&transform, 0.0, 0.0, 1080.0);
        assert_close(x, 3840.0 / 2.0 + MASCOT_DRAW_GLIDE_OFFSET_X);
    }

    fn assert_contextual_placement_inside_bounds(
        placement: ContextualAssistWindowPlacement,
        bounds: ContextualAssistScreenBounds,
    ) {
        assert!(
            placement.x >= bounds.x,
            "x {} should be >= monitor left {}",
            placement.x,
            bounds.x
        );
        assert!(
            placement.y >= bounds.y,
            "y {} should be >= monitor top {}",
            placement.y,
            bounds.y
        );
        assert!(
            placement.x + placement.physical_width <= bounds.x + bounds.width,
            "right edge {} should be <= monitor right {}",
            placement.x + placement.physical_width,
            bounds.x + bounds.width
        );
        assert!(
            placement.y + placement.physical_height <= bounds.y + bounds.height,
            "bottom edge {} should be <= monitor bottom {}",
            placement.y + placement.physical_height,
            bounds.y + bounds.height
        );
    }

    #[test]
    fn contextual_assist_placement_keeps_expanded_menu_inside_bottom_right_bounds() {
        let bounds = ContextualAssistScreenBounds {
            x: 0.0,
            y: 0.0,
            width: 1512.0,
            height: 982.0,
            scale: 2.0,
        };
        let placement = contextual_assist_window_placement(
            CONTEXTUAL_ASSIST_MENU_WIDTH,
            CONTEXTUAL_ASSIST_MENU_HEIGHT,
            true,
            Some((1505.0, 975.0)),
            Some(bounds),
        );

        assert_contextual_placement_inside_bounds(placement, bounds);
        assert_close(placement.x, 588.0);
        assert_close(placement.y, 12.0);
        assert_close(placement.physical_width, 912.0);
        assert_close(placement.physical_height, 958.0);
    }

    #[test]
    fn contextual_assist_centered_menu_uses_anchor_as_center() {
        let bounds = ContextualAssistScreenBounds {
            x: 0.0,
            y: 0.0,
            width: 1512.0,
            height: 982.0,
            scale: 2.0,
        };
        let placement = contextual_assist_centered_window_placement(
            CONTEXTUAL_ASSIST_MENU_WIDTH,
            CONTEXTUAL_ASSIST_MENU_HEIGHT,
            (756.0, 491.0),
            Some(bounds),
        );

        assert_contextual_placement_inside_bounds(placement, bounds);
        assert_close(placement.x, 300.0);
        assert_close(placement.y, 12.0);
        assert_close(placement.physical_width, 912.0);
        assert_close(placement.physical_height, 958.0);
    }

    #[test]
    fn contextual_assist_placement_keeps_chip_inside_negative_origin_monitor() {
        let bounds = ContextualAssistScreenBounds {
            x: -1728.0,
            y: 100.0,
            width: 1728.0,
            height: 1117.0,
            scale: 2.0,
        };
        let placement = contextual_assist_window_placement(
            CONTEXTUAL_ASSIST_CHIP_WIDTH,
            CONTEXTUAL_ASSIST_CHIP_HEIGHT,
            false,
            Some((-8.0, 1210.0)),
            Some(bounds),
        );

        assert_contextual_placement_inside_bounds(placement, bounds);
        assert_close(placement.x, -88.0);
        assert_close(placement.y, 1129.0);
        assert_close(placement.physical_width, 76.0);
        assert_close(placement.physical_height, 76.0);
    }

    #[test]
    fn contextual_assist_placement_shrinks_to_tiny_bounds() {
        let bounds = ContextualAssistScreenBounds {
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 20.0,
            scale: 2.0,
        };
        let placement = contextual_assist_window_placement(
            CONTEXTUAL_ASSIST_MENU_WIDTH,
            CONTEXTUAL_ASSIST_MENU_HEIGHT,
            true,
            Some((18.0, 18.0)),
            Some(bounds),
        );

        assert_contextual_placement_inside_bounds(placement, bounds);
        assert_close(placement.x, 0.0);
        assert_close(placement.y, 0.0);
        assert_close(placement.physical_width, 20.0);
        assert_close(placement.physical_height, 20.0);
    }

    #[test]
    fn overlay_routes_resolve_for_current_build_mode() {
        // Hermetic against a developer shell that exports the bundled-assets
        // override: the default assertions below require it unset.
        std::env::remove_var("MAGICIAN_DESKTOP_BUNDLED_OVERLAYS");
        let hud = overlay_route_webview_url("HUD", OVERLAY_HUD_DEV_URL, OVERLAY_HUD_APP_PATH)
            .expect("HUD URL should resolve");
        let draw =
            overlay_route_webview_url("draw overlay", DRAW_OVERLAY_DEV_URL, DRAW_OVERLAY_APP_PATH)
                .expect("draw overlay URL should resolve");

        #[cfg(debug_assertions)]
        {
            match hud {
                tauri::WebviewUrl::External(url) => {
                    assert_eq!(url.as_str(), OVERLAY_HUD_DEV_URL);
                },
                other => panic!("expected debug HUD external URL, got {other:?}"),
            }
            match draw {
                tauri::WebviewUrl::External(url) => {
                    assert_eq!(url.as_str(), DRAW_OVERLAY_DEV_URL);
                },
                other => panic!("expected debug draw external URL, got {other:?}"),
            }

            // The bundled-assets override flips debug resolution to the same
            // custom-protocol URLs release uses. Checked in this same test so
            // the env mutation cannot race a sibling test.
            std::env::set_var("MAGICIAN_DESKTOP_BUNDLED_OVERLAYS", "1");
            let bundled_hud =
                overlay_route_webview_url("HUD", OVERLAY_HUD_DEV_URL, OVERLAY_HUD_APP_PATH)
                    .expect("HUD URL should resolve under bundled override");
            std::env::remove_var("MAGICIAN_DESKTOP_BUNDLED_OVERLAYS");
            match bundled_hud {
                tauri::WebviewUrl::CustomProtocol(url) => {
                    assert_eq!(
                        url.as_str(),
                        crate::tray::desktop_app_url(OVERLAY_HUD_APP_PATH).as_str()
                    );
                },
                other => panic!("expected bundled HUD app URL, got {other:?}"),
            }
        }

        #[cfg(not(debug_assertions))]
        {
            match hud {
                tauri::WebviewUrl::CustomProtocol(url) => {
                    assert_eq!(
                        url.as_str(),
                        crate::tray::desktop_app_url(OVERLAY_HUD_APP_PATH).as_str()
                    );
                },
                other => panic!("expected release HUD app URL, got {other:?}"),
            }
            match draw {
                tauri::WebviewUrl::CustomProtocol(url) => {
                    assert_eq!(
                        url.as_str(),
                        crate::tray::desktop_app_url(DRAW_OVERLAY_APP_PATH).as_str()
                    );
                },
                other => panic!("expected release draw app URL, got {other:?}"),
            }
        }
    }

    #[test]
    fn capture_coordinate_mapper_translates_crop_pixels_to_model_space() {
        let mapper = DrawOverlayCoordinateMapper::Capture {
            monitor: DrawOverlayMonitorTransform {
                x: 0.0,
                y: 0.0,
                width: 2000.0,
                height: 1000.0,
                scale: 1.024,
                device_scale: 1.0,
            },
            rect: DrawOverlayCaptureRect {
                x: 100.0,
                y: 50.0,
                width: 400.0,
                height: 200.0,
            },
            image: DrawOverlayImageSize {
                width: 800.0,
                height: 400.0,
            },
        };
        let mut shape = json!({
            "type": "highlight",
            "x": 400.0,
            "y": 200.0,
            "w": 160.0,
            "h": 80.0,
            "points": [[0.0, 0.0], {"x": 800.0, "y": 400.0}]
        });
        {
            let object = shape.as_object_mut().expect("shape object");
            transform_overlay_shape_object_coordinates(object, &mapper).expect("transform");
        }

        assert_close(shape["x"].as_f64().unwrap(), 307.2);
        assert_close(shape["y"].as_f64().unwrap(), 153.6);
        assert_close(shape["w"].as_f64().unwrap(), 81.92);
        assert_close(shape["h"].as_f64().unwrap(), 40.96);
        assert_close(shape["points"][0][0].as_f64().unwrap(), 102.4);
        assert_close(shape["points"][0][1].as_f64().unwrap(), 51.2);
        assert_close(shape["points"][1]["x"].as_f64().unwrap(), 512.0);
        assert_close(shape["points"][1]["y"].as_f64().unwrap(), 256.0);
    }

    #[test]
    fn draw_overlay_click_points_include_logical_scaled_retina_coordinates() {
        let mut monitor = draw_overlay_monitor_transform_from_parts(0, 0, 2880, 1800);
        monitor.device_scale = 2.0;

        let points =
            draw_overlay_screen_point_to_model_points_with_transform(&monitor, 1400.0, 880.0);

        assert_eq!(points.len(), 2);
        assert_close(points[1].x, 1991.111111111111);
        assert_close(points[1].y, 1251.5555555555557);
    }

    #[test]
    fn draw_overlay_control_rect_hit_has_padding() {
        let rect = DrawOverlayControlRect {
            x: 100.0,
            y: 200.0,
            width: 120.0,
            height: 34.0,
        };

        assert!(draw_overlay_control_rect_hit(
            DrawOverlayModelPoint { x: 90.0, y: 210.0 },
            &rect
        ));
        assert!(!draw_overlay_control_rect_hit(
            DrawOverlayModelPoint { x: 70.0, y: 210.0 },
            &rect
        ));
    }

    fn explain_deeper_controls() -> DrawOverlayControlsState {
        DrawOverlayControlsState {
            deeper_rect: Some(DrawOverlayControlRect {
                x: 100.0,
                y: 200.0,
                width: 150.0,
                height: 34.0,
            }),
            keep_showing_rect: Some(DrawOverlayControlRect {
                x: 260.0,
                y: 200.0,
                width: 154.0,
                height: 34.0,
            }),
            dismiss_rect: Some(DrawOverlayControlRect {
                x: 424.0,
                y: 200.0,
                width: 128.0,
                height: 34.0,
            }),
            replay_rect: Some(DrawOverlayControlRect {
                x: 562.0,
                y: 200.0,
                width: 118.0,
                height: 34.0,
            }),
            action_rects: Vec::new(),
        }
    }

    fn test_draw_overlay_monitor() -> DrawOverlayMonitorTransform {
        DrawOverlayMonitorTransform {
            x: 0.0,
            y: 0.0,
            width: 1_000.0,
            height: 1_000.0,
            scale: 1.0,
            device_scale: 1.0,
        }
    }

    #[test]
    fn reported_explain_deeper_rect_resolves_to_its_action() {
        let mut overlay = OverlaySessionState {
            draw_overlay_controls: explain_deeper_controls(),
            ..OverlaySessionState::default()
        };
        let action = resolve_draw_overlay_click_action(
            &mut overlay,
            &[DrawOverlayModelPoint { x: 130.0, y: 217.0 }],
            test_draw_overlay_monitor(),
            130.0,
            217.0,
        );

        assert_eq!(action, DrawOverlayClickAction::ExplainDeeper);
    }

    #[test]
    fn reported_dismiss_rect_keeps_its_action_after_deeper_is_added() {
        let mut overlay = OverlaySessionState {
            draw_overlay_controls: explain_deeper_controls(),
            ..OverlaySessionState::default()
        };
        let action = resolve_draw_overlay_click_action(
            &mut overlay,
            &[DrawOverlayModelPoint { x: 440.0, y: 217.0 }],
            test_draw_overlay_monitor(),
            440.0,
            217.0,
        );

        assert_eq!(action, DrawOverlayClickAction::Dismiss(None));
    }

    #[test]
    fn exact_deeper_ownership_beats_neighbor_padding() {
        let controls = explain_deeper_controls();
        // x=248 is visibly inside Deeper, but also inside Keep Showing's 16px
        // padded target across the 10px visual gap.
        let action = reported_draw_overlay_click_action(
            &[DrawOverlayModelPoint { x: 248.0, y: 217.0 }],
            &controls,
        );

        assert_eq!(action, Some(DrawOverlayClickAction::ExplainDeeper));
    }

    #[test]
    fn shared_control_boundary_has_one_half_open_owner() {
        let mut controls = explain_deeper_controls();
        controls.dismiss_rect = Some(DrawOverlayControlRect {
            x: 250.0,
            y: 200.0,
            width: 128.0,
            height: 34.0,
        });
        controls.keep_showing_rect = None;
        let point = DrawOverlayModelPoint { x: 250.0, y: 217.0 };

        assert!(!draw_overlay_control_rect_contains(
            point,
            controls.deeper_rect.as_ref().unwrap()
        ));
        assert!(draw_overlay_control_rect_contains(
            point,
            controls.dismiss_rect.as_ref().unwrap()
        ));
        assert_eq!(
            reported_draw_overlay_click_action(&[point], &controls),
            Some(DrawOverlayClickAction::Dismiss(None))
        );
    }

    #[test]
    fn absent_deeper_rect_never_uses_guessed_geometry() {
        let mut overlay = OverlaySessionState::default();
        overlay
            .active_draw_shapes
            .push(ActiveOverlayDrawShape::new(json!({ "type": "rect" })));
        let action = resolve_draw_overlay_click_action(
            &mut overlay,
            &[DrawOverlayModelPoint { x: 450.0, y: 950.0 }],
            test_draw_overlay_monitor(),
            450.0,
            950.0,
        );

        assert_eq!(action, DrawOverlayClickAction::None);
    }

    #[test]
    fn draw_queue_skips_shapes_from_stale_generation() {
        let mut state = OverlaySessionState {
            draw_generation: 2,
            ..OverlaySessionState::default()
        };
        let queued = queue_overlay_draw_shape(
            &mut state,
            json!({
                "type": "highlight",
                "x": 10.0,
                "y": 20.0,
                "w": 30.0,
                "h": 40.0
            }),
            1,
        );

        assert!(!queued);
        assert!(state.pending_draw_shapes.is_empty());
        assert!(state.active_draw_shapes.is_empty());
    }

    #[test]
    fn draw_queue_accepts_current_generation_and_honors_clear_previous() {
        let mut state = OverlaySessionState {
            draw_generation: 3,
            ..OverlaySessionState::default()
        };
        state
            .active_draw_shapes
            .push(ActiveOverlayDrawShape::new(json!({
                "type": "highlight",
                "x": 10.0,
                "y": 20.0,
                "w": 30.0,
                "h": 40.0
            })));

        let queued = queue_overlay_draw_shape(
            &mut state,
            json!({
                "type": "arrow",
                "from_x": 10.0,
                "from_y": 20.0,
                "to_x": 80.0,
                "to_y": 100.0,
                "clear_previous": true
            }),
            3,
        );

        assert!(queued);
        assert_eq!(state.pending_draw_shapes.len(), 1);
        assert_eq!(state.active_draw_shapes.len(), 1);
        assert_eq!(
            state.active_draw_shapes[0]
                .shape
                .get("type")
                .and_then(Value::as_str),
            Some("arrow")
        );
    }

    #[test]
    fn draw_queue_clear_resets_control_regions() {
        let mut state = OverlaySessionState {
            draw_generation: 5,
            draw_overlay_controls: DrawOverlayControlsState {
                dismiss_rect: Some(DrawOverlayControlRect {
                    x: 10.0,
                    y: 20.0,
                    width: 120.0,
                    height: 34.0,
                }),
                keep_showing_rect: Some(DrawOverlayControlRect {
                    x: 140.0,
                    y: 20.0,
                    width: 154.0,
                    height: 34.0,
                }),
                replay_rect: Some(DrawOverlayControlRect {
                    x: 304.0,
                    y: 20.0,
                    width: 118.0,
                    height: 34.0,
                }),
                deeper_rect: Some(DrawOverlayControlRect {
                    x: 432.0,
                    y: 20.0,
                    width: 58.0,
                    height: 34.0,
                }),
                action_rects: vec![DrawOverlayControlRect {
                    x: 500.0,
                    y: 20.0,
                    width: 90.0,
                    height: 34.0,
                }],
            },
            ..OverlaySessionState::default()
        };

        let queued = queue_overlay_draw_shape(
            &mut state,
            json!({
                "type": "clear"
            }),
            5,
        );

        assert!(queued);
        assert_eq!(
            state.draw_overlay_controls,
            DrawOverlayControlsState::default()
        );
    }

    #[test]
    fn draw_queue_drain_filters_stale_generation_replay_shapes() {
        let mut state = OverlaySessionState {
            draw_generation: 7,
            ..OverlaySessionState::default()
        };
        state
            .pending_draw_shapes
            .push(overlay_draw_shape_with_generation(
                json!({
                    "type": "highlight",
                    "id": "old",
                    "x": 10.0,
                    "y": 20.0,
                    "w": 30.0,
                    "h": 40.0
                }),
                6,
            ));
        state
            .pending_draw_shapes
            .push(overlay_draw_shape_with_generation(
                json!({
                    "type": "highlight",
                    "id": "current",
                    "x": 11.0,
                    "y": 21.0,
                    "w": 31.0,
                    "h": 41.0
                }),
                7,
            ));

        let drained = drain_pending_overlay_draw_shapes(&mut state);

        assert_eq!(drained.len(), 1);
        assert_eq!(
            drained[0].get("id").and_then(Value::as_str),
            Some("current")
        );
        assert!(drained[0].get(DRAW_OVERLAY_GENERATION_FIELD).is_none());
        assert!(state.pending_draw_shapes.is_empty());
    }

    fn execution_state(
        execution_id: &str,
        prompt: &str,
        status: Option<&str>,
        pending_prompt: Option<OverlayPrompt>,
        pending_clarification: Option<OverlayClarification>,
    ) -> OverlayExecutionRuntimeState {
        OverlayExecutionRuntimeState {
            tracked: TrackedOverlayExecution {
                execution_id: execution_id.to_string(),
                prompt: prompt.to_string(),
            },
            status: status.map(ToOwned::to_owned),
            pending_prompt,
            pending_clarification,
        }
    }

    #[test]
    fn draw_v2_group_normalization_flattens_and_inherits_metadata() {
        let shapes = normalize_overlay_draw_shapes(json!({
            "type": "group",
            "id": "slope-step",
            "color": "orange",
            "ttl_ms": 12000,
            "reveal_id": "show-slope",
            "reveal_order": 2,
            "delay_ms": 900,
            "duration_ms": 1100,
            "tutor_step_label": "Show slope",
            "narration": "Now connect rise over run to the plotted line.",
            "wait_for_voice": true,
            "shapes": [
                {"type": "axis", "x1": 100.0, "y1": 500.0, "x2": 700.0, "y2": 500.0},
                {"type": "formula", "x": 420.0, "y": 260.0, "text": "slope = rise / run", "color": "blue"}
            ]
        }))
        .expect("group should normalize");

        assert_eq!(shapes.len(), 2);
        assert_eq!(
            shapes[0].get("group_id").and_then(Value::as_str),
            Some("slope-step")
        );
        assert_eq!(
            shapes[0].get("color").and_then(Value::as_str),
            Some("orange")
        );
        assert_eq!(shapes[0].get("ttl_ms").and_then(Value::as_u64), Some(12000));
        assert_eq!(
            shapes[0].get("reveal_id").and_then(Value::as_str),
            Some("show-slope")
        );
        assert_eq!(
            shapes[0].get("reveal_order").and_then(Value::as_u64),
            Some(2)
        );
        assert_eq!(shapes[0].get("delay_ms").and_then(Value::as_u64), Some(900));
        assert_eq!(
            shapes[0].get("duration_ms").and_then(Value::as_u64),
            Some(1100)
        );
        assert_eq!(
            shapes[0].get("tutor_step_label").and_then(Value::as_str),
            Some("Show slope")
        );
        assert_eq!(
            shapes[0].get("narration").and_then(Value::as_str),
            Some("Now connect rise over run to the plotted line.")
        );
        assert_eq!(
            shapes[0].get("wait_for_voice").and_then(Value::as_bool),
            Some(true)
        );
        assert!(shapes[0].get("id").and_then(Value::as_str).is_some());
        assert_eq!(shapes[1].get("color").and_then(Value::as_str), Some("blue"));
    }

    #[test]
    fn draw_overlay_delay_scales_explicit_delay_and_derives_reveal_order_delay() {
        assert_eq!(
            overlay_draw_shape_delay_ms(&json!({
                "type": "axis",
                "delay_ms": 900
            })),
            1500
        );
        assert_eq!(
            overlay_draw_shape_delay_ms(&json!({
                "type": "formula",
                "reveal_order": 3
            })),
            4800
        );
        assert_eq!(
            overlay_draw_shape_delay_ms(&json!({
                "type": "formula",
                "delay_ms": 0,
                "reveal_order": 3
            })),
            0
        );
    }

    #[test]
    fn draw_v2_normalization_rejects_missing_required_geometry() {
        for payload in [
            json!({"type": "line", "x1": 100.0, "y1": 100.0}),
            json!({"type": "formula", "text": "x = 1"}),
            json!({"type": "highlight", "x": 100.0, "y": 200.0, "w": 300.0}),
            json!({"type": "circle", "cx": 200.0, "cy": 200.0}),
            json!({"type": "polygon"}),
        ] {
            let error =
                normalize_overlay_draw_shapes(payload).expect_err("missing geometry should fail");
            assert!(
                error.contains("requires"),
                "expected required-geometry error, got {error}"
            );
        }
    }

    #[test]
    fn draw_v2_normalization_accepts_curved_primitives() {
        let shapes = normalize_overlay_draw_shapes(json!({
            "type": "group",
            "shapes": [
                {
                    "type": "path",
                    "d": "M 40 140 C 90 40 150 240 220 120",
                    "color": "#67e8f9"
                },
                {
                    "type": "curve",
                    "from_x": 260.0,
                    "from_y": 180.0,
                    "control_x": 320.0,
                    "control_y": 40.0,
                    "to_x": 390.0,
                    "to_y": 180.0,
                    "color": "#f0abfc"
                },
                {
                    "type": "freehand",
                    "points": [[430.0, 170.0], [470.0, 110.0], [510.0, 180.0]],
                    "color": "#fde047"
                }
            ]
        }))
        .expect("curved primitives should normalize");

        assert_eq!(shapes.len(), 3);
        assert_eq!(shapes[0].get("type").and_then(Value::as_str), Some("path"));
        assert_eq!(shapes[1].get("type").and_then(Value::as_str), Some("curve"));
        assert_eq!(
            shapes[2].get("type").and_then(Value::as_str),
            Some("freehand")
        );
    }

    #[test]
    fn draw_v2_normalization_rejects_bad_curved_primitives() {
        let unsafe_path = normalize_overlay_draw_shapes(json!({
            "type": "path",
            "d": "M 0 0 <script>"
        }))
        .expect_err("unsafe path data should fail");
        assert!(unsafe_path.contains("unsupported characters"));

        let missing_control = normalize_overlay_draw_shapes(json!({
            "type": "curve",
            "from_x": 10.0,
            "from_y": 20.0,
            "to_x": 110.0,
            "to_y": 120.0
        }))
        .expect_err("curve without controls should fail");
        assert!(missing_control.contains("Bezier control point"));
    }

    #[test]
    fn draw_v2_normalization_accepts_text_backed_handwriting() {
        let shapes = normalize_overlay_draw_shapes(json!({
            "type": "cursive_text",
            "x": 180.0,
            "y": 430.0,
            "text": "we",
            "font_size": 96.0,
            "color": "cyan"
        }))
        .expect("cursive text should normalize");

        assert_eq!(shapes.len(), 1);
        assert_eq!(
            shapes[0].get("type").and_then(Value::as_str),
            Some("cursive_text")
        );
    }

    #[test]
    fn draw_v2_normalization_rejects_handwriting_without_text() {
        let error = normalize_overlay_draw_shapes(json!({
            "type": "handwriting",
            "x": 180.0,
            "y": 430.0,
            "font_size": 96.0
        }))
        .expect_err("handwriting without text should fail");

        assert!(error.contains("text") || error.contains("label"));
    }

    #[test]
    fn draw_v2_normalization_accepts_rect_like_area_fill() {
        let shapes = normalize_overlay_draw_shapes(json!({
            "type": "area_fill",
            "x": 120.0,
            "y": 240.0,
            "w": 300.0,
            "h": 80.0,
            "label": "region"
        }))
        .expect("area fill should normalize");

        assert_eq!(shapes.len(), 1);
        assert_eq!(
            shapes[0].get("type").and_then(Value::as_str),
            Some("area_fill")
        );
    }

    #[test]
    fn active_draw_shapes_prune_as_group_by_ttl_unless_persistent() {
        let mut state = OverlaySessionState::default();
        let expired_age = DRAW_OVERLAY_MIN_TTL_MS + DRAW_OVERLAY_DEFAULT_DRAW_DURATION_MS + 1_000;
        state.active_draw_shapes.push(ActiveOverlayDrawShape {
            shape: json!({"type": "highlight", "ttl_ms": 1000}),
            created_at: Instant::now() - Duration::from_millis(1_500),
        });
        state.active_draw_shapes.push(ActiveOverlayDrawShape {
            shape: json!({"type": "highlight", "ttl_ms": 1000}),
            created_at: Instant::now() - Duration::from_millis(expired_age),
        });

        prune_expired_active_draw_shapes(&mut state);

        assert_eq!(state.active_draw_shapes.len(), 2);

        for active in &mut state.active_draw_shapes {
            active.created_at = Instant::now() - Duration::from_millis(expired_age);
        }
        prune_expired_active_draw_shapes(&mut state);
        assert!(state.active_draw_shapes.is_empty());

        state.active_draw_shapes.push(ActiveOverlayDrawShape {
            shape: json!({"type": "highlight", "ttl_ms": 1000}),
            created_at: Instant::now() - Duration::from_millis(expired_age),
        });
        state.active_draw_shapes.push(ActiveOverlayDrawShape {
            shape: json!({"type": "highlight", "persist": true}),
            created_at: Instant::now() - Duration::from_secs(60),
        });

        prune_expired_active_draw_shapes(&mut state);

        assert_eq!(state.active_draw_shapes.len(), 2);
        assert!(state.active_draw_shapes[1]
            .shape
            .get("persist")
            .and_then(Value::as_bool)
            .unwrap_or(false));
    }

    #[test]
    fn normalize_agentic_input_type_names() {
        assert_eq!(normalize_input_type_name("Text"), "text");
        assert_eq!(normalize_input_type_name("MultiChoice"), "multi_choice");
        assert_eq!(
            normalize_input_type_name("external_action"),
            "external_action"
        );
        assert_eq!(normalize_input_type_name("FilePath"), "file_path");
    }

    #[test]
    fn parse_options_supports_agentic_and_clarify_shapes() {
        let agentic = Value::String(
            r#"[{"id":"browser","label":"Browser UI"},{"id":"api","label":"API","description":"Faster"}]"#
                .to_string(),
        );
        let clarify = Value::Array(vec![json!({
            "value": "imap",
            "label": "IMAP",
            "description": "Needs credentials"
        })]);

        assert_eq!(
            parse_options(Some(&agentic)),
            vec![
                OverlayOption {
                    id: "browser".to_string(),
                    label: "Browser UI".to_string(),
                    description: None,
                },
                OverlayOption {
                    id: "api".to_string(),
                    label: "API".to_string(),
                    description: Some("Faster".to_string()),
                }
            ]
        );
        assert_eq!(
            parse_options(Some(&clarify)),
            vec![OverlayOption {
                id: "imap".to_string(),
                label: "IMAP".to_string(),
                description: Some("Needs credentials".to_string()),
            }]
        );
    }

    #[test]
    fn terminal_status_detection_is_lenient() {
        assert!(is_terminal_status("Completed"));
        assert!(is_terminal_status("failed"));
        assert!(is_terminal_status("cancelled"));
        assert!(!is_terminal_status("running"));
        assert!(!is_terminal_status("planning_complete"));
    }

    #[test]
    fn overlay_shortcut_validation_normalizes_empty_values() {
        assert_eq!(
            validate_overlay_shortcut("   ").unwrap(),
            crate::config::DEFAULT_OVERLAY_SHORTCUT
        );
        assert!(validate_overlay_shortcut("bad shortcut").is_err());
    }

    #[test]
    fn execution_api_base_url_uses_execution_routes() {
        let mut config = crate::config::MagicianDesktopConfig::default();
        config.network.magician_port = 4123;

        assert_eq!(
            executions_api_url(&config),
            "http://127.0.0.1:4123/api/magician/v2/executions"
        );
        assert_eq!(
            execution_api_base_url(&config, "exec-42"),
            "http://127.0.0.1:4123/api/magician/v2/executions/exec-42"
        );
    }

    #[test]
    fn uses_agentic_continue_for_manual_and_max_iteration_pauses() {
        assert!(uses_agentic_continue(Some("Manual")));
        assert!(uses_agentic_continue(Some("max_iterations")));
        assert!(!uses_agentic_continue(Some("Confirmation")));
        assert!(!uses_agentic_continue(None));
    }

    #[test]
    fn rejecting_continue_pause_uses_cancel_endpoint() {
        let request = OverlayResumeRequest {
            execution_id: "exec-1".to_string(),
            input_type: "confirmation".to_string(),
            value: json!({
                "type": "confirmation",
                "confirmed": false
            }),
            pause_kind: Some("max_iterations".to_string()),
            ..Default::default()
        };

        assert!(should_cancel_overlay_pause_action(&request));
    }

    #[test]
    fn aborted_continue_pause_uses_cancel_endpoint() {
        let request = OverlayResumeRequest {
            execution_id: "exec-1".to_string(),
            input_type: "confirmation".to_string(),
            value: json!({
                "type": "aborted",
                "reason": "Cancelled from desktop overlay"
            }),
            pause_kind: Some("manual".to_string()),
            ..Default::default()
        };

        assert!(should_cancel_overlay_pause_action(&request));
    }

    #[test]
    fn parse_confirmation_labels_extracts_resume_cancel_labels() {
        let value = json!({
            "Confirmation": {
                "confirm_label": "Resume",
                "deny_label": "Cancel",
                "destructive": false
            }
        });

        assert_eq!(
            parse_confirmation_labels(Some(&value)),
            (Some("Resume".to_string()), Some("Cancel".to_string()))
        );
    }

    #[test]
    fn primary_execution_selection_prefers_prompt_then_clarification_then_latest_active() {
        let clarification = OverlayClarification {
            execution_id: "older".to_string(),
            question_id: "clarify-1".to_string(),
            question_text: "Need more detail".to_string(),
            ..OverlayClarification::default()
        };
        let prompt = OverlayPrompt {
            execution_id: "newer".to_string(),
            question: "Approve deletion?".to_string(),
            input_type: "confirmation".to_string(),
            ..OverlayPrompt::default()
        };

        let states = vec![
            execution_state(
                "newer",
                "New execution",
                Some("running"),
                Some(prompt),
                None,
            ),
            execution_state(
                "older",
                "Older execution",
                Some("running"),
                None,
                Some(clarification),
            ),
        ];
        assert_eq!(
            select_primary_execution(&states)
                .unwrap()
                .tracked
                .execution_id,
            "newer"
        );

        let clarification_only = vec![
            execution_state("first", "First", Some("running"), None, None),
            execution_state(
                "second",
                "Second",
                Some("running"),
                None,
                Some(OverlayClarification {
                    execution_id: "second".to_string(),
                    question_id: "clarify-2".to_string(),
                    question_text: "Which account?".to_string(),
                    ..OverlayClarification::default()
                }),
            ),
        ];
        assert_eq!(
            select_primary_execution(&clarification_only)
                .unwrap()
                .tracked
                .execution_id,
            "second"
        );

        let running_only = vec![
            execution_state("latest", "Latest", Some("running"), None, None),
            execution_state("older", "Older", Some("running"), None, None),
        ];
        assert_eq!(
            select_primary_execution(&running_only)
                .unwrap()
                .tracked
                .execution_id,
            "latest"
        );
    }

    #[test]
    fn tracked_execution_summaries_reflect_waiting_state() {
        let summaries = summarize_tracked_executions(&[
            execution_state(
                "prompt-execution",
                "Prompt execution",
                Some("running"),
                Some(OverlayPrompt {
                    execution_id: "prompt-execution".to_string(),
                    question: "Continue?".to_string(),
                    input_type: "confirmation".to_string(),
                    ..OverlayPrompt::default()
                }),
                None,
            ),
            execution_state(
                "clarify-execution",
                "Clarify execution",
                Some("running"),
                None,
                Some(OverlayClarification {
                    execution_id: "clarify-execution".to_string(),
                    question_id: "clarify-3".to_string(),
                    question_text: "Pick a mailbox".to_string(),
                    ..OverlayClarification::default()
                }),
            ),
        ]);

        assert_eq!(summaries[0].waiting_on.as_deref(), Some("input"));
        assert_eq!(summaries[0].waiting_question.as_deref(), Some("Continue?"));
        assert_eq!(summaries[1].waiting_on.as_deref(), Some("clarification"));
        assert_eq!(
            summaries[1].waiting_question.as_deref(),
            Some("Pick a mailbox")
        );
    }

    #[test]
    fn display_execution_selection_honors_selected_actionable_execution() {
        let states = vec![
            execution_state(
                "newer",
                "Newer",
                Some("running"),
                Some(OverlayPrompt {
                    execution_id: "newer".to_string(),
                    question: "Newest prompt".to_string(),
                    input_type: "text".to_string(),
                    ..OverlayPrompt::default()
                }),
                None,
            ),
            execution_state(
                "older",
                "Older",
                Some("running"),
                None,
                Some(OverlayClarification {
                    execution_id: "older".to_string(),
                    question_id: "clarify-older".to_string(),
                    question_text: "Older clarification".to_string(),
                    ..OverlayClarification::default()
                }),
            ),
        ];

        assert_eq!(
            select_display_execution(Some("older"), &states)
                .unwrap()
                .tracked
                .execution_id,
            "older"
        );
        assert_eq!(
            select_display_execution(Some("missing"), &states)
                .unwrap()
                .tracked
                .execution_id,
            "newer"
        );
    }

    #[test]
    fn display_execution_selection_falls_back_when_selected_execution_has_no_pending_work() {
        let states = vec![
            execution_state("selected", "Selected", Some("running"), None, None),
            execution_state(
                "waiting",
                "Waiting",
                Some("running"),
                Some(OverlayPrompt {
                    execution_id: "waiting".to_string(),
                    question: "Needs input".to_string(),
                    input_type: "text".to_string(),
                    ..OverlayPrompt::default()
                }),
                None,
            ),
        ];

        assert_eq!(
            select_display_execution(Some("selected"), &states)
                .unwrap()
                .tracked
                .execution_id,
            "waiting"
        );
    }
}
