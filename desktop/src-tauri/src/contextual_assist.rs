use crate::{config::ContextualAssistConfig, overlay, AppState};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Manager};
use tokio::process::Command;
use tokio::time::{sleep, Duration, Instant};
use tracing::warn;

const POLL_INTERVAL: Duration = Duration::from_millis(300);
const STABLE_TARGET_DELAY: Duration = Duration::from_millis(450);
const LEFT_OPTION_SINGLE_TAP_DELAY_MS: u64 = 475;
const BROWSER_PROBE_TIMEOUT_MS: u64 = 2_500;
const ACTION_EXECUTION_TIMEOUT_SECS: u64 = 180;
const SCREENCAPTURE_BIN: &str = "/usr/sbin/screencapture";
const MAX_CONTEXT_TEXT_CHARS: usize = 50_000;
const TAURI_WEBVIEW_CONTEXT_MAX_AGE: Duration = Duration::from_secs(30 * 60);
const ACTION_STATUS_PENDING_BINDING: &str = "pending_binding";
const ACTION_STATUS_DRAFT_READY: &str = "draft_ready";
const ACTION_BINDING_MAGICIAN_EXECUTE: &str = "magician_contextual_assist_execute";
const WRITING_ASSISTANT_AGENT_ID: &str = "writing-assistant";
const ACTION_KIND_PRIMARY: &str = "primary";
const ACTION_KIND_SECONDARY: &str = "secondary";
const SCREENSHOT_POLICY_REQUIRED: &str = "required";
const SCREENSHOT_POLICY_SKIPPED: &str = "skipped";
const SCREENSHOT_SOURCE_CHROME_VISIBLE_TAB: &str = "chrome_visible_tab";
const SCREENSHOT_SOURCE_ACTIVE_WINDOW_REGION: &str = "active_window_region";
const SCREENSHOT_SOURCE_FULL_SCREEN_FALLBACK: &str = "full_screen_fallback";
const SCREENSHOT_SOURCE_NONE: &str = "none";

const STATE_SELECTION: &str = "selection";
const STATE_SELECTION_FIELD: &str = "selection-field";
const STATE_EMPTY_CONTEXT: &str = "empty-context";
const STATE_EMPTY_NO_CONTEXT: &str = "empty-no-context";
const STATE_PAGE_CONTEXT: &str = "page-context";
const STATE_FILES: &str = "files";
const STATE_DRAFT: &str = "draft";
const STATE_SECURE: &str = "secure";
const STATE_EXCLUDED: &str = "excluded";
const STATE_UNSUPPORTED: &str = "unsupported";

const ACTION_REWRITE: &str = "rewrite";
const ACTION_SUMMARIZE: &str = "summarize";
const ACTION_SUMMARIZE_PAGE: &str = "summarize_page";
const ACTION_DRAFT_REPLY: &str = "draft_reply";
const ACTION_WRITE_FROM_CONTEXT: &str = "write_from_context";
const ACTION_OBSERVE_THEN_DRAFT: &str = "observe_then_draft";
const ACTION_CONTINUE_DRAFT: &str = "continue_draft";
const ACTION_IMPROVE_DRAFT: &str = "improve_draft";
const ACTION_SHORTEN: &str = "shorten";
const ACTION_CLARIFY: &str = "clarify";
const ACTION_CREATE_TASK: &str = "create_task";
const ACTION_SCHEDULE_FOLLOWUP: &str = "schedule_followup";
const ACTION_OPEN_HUD: &str = "open_hud";
/// Files the selection into Notes. Unlike every other action here it produces
/// no draft to review, so the overlay runs it directly instead of through the
/// select-then-generate flow.
/// Files the selection into Notes rather than asking a model to write about it.
///
/// The overlay matches this id to take the local capture path, so it is duplicated
/// in Svelte and guarded by `make check-notes-capture-surface`.
const ACTION_SAVE_TO_NOTES: &str = "save_to_notes";

/// Focus-settle before verifying the insertion target after the menu
/// resigns keyboard focus.
const INSERT_FOCUS_SETTLE_MS: u64 = 90;
/// How long the target app gets to consume the staged paste before the
/// user's previous clipboard is restored.
const INSERT_PASTE_SETTLE_MS: u64 = 160;
const INSERT_MAIN_THREAD_TIMEOUT_MS: u64 = 750;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextualAssistCaptureRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NativeAssistTarget {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_tab_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_window_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_rect: Option<ContextualAssistCaptureRect>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextualAssistActionOption {
    pub id: String,
    pub label: String,
    pub description: String,
    pub kind: String,
    pub intent: String,
    pub requires_context: bool,
    pub requires_observation: bool,
    pub requires_screenshot: bool,
    pub requires_writable_target: bool,
    pub mutates_text: bool,
    pub creates_task: bool,
    pub opens_hud: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextualAssistActionState {
    pub id: String,
    pub label: String,
    pub actions: Vec<ContextualAssistActionOption>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualAssistActionRequest {
    pub action_id: String,
    #[serde(default)]
    pub user_prompt: Option<String>,
    #[serde(default)]
    pub reuse_screenshot_attachment_id: Option<String>,
    #[serde(default)]
    pub personality: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub target: Option<NativeAssistTarget>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextualAssistActionContext {
    pub state: String,
    pub personality: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_text: Option<String>,
    pub has_context_text: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextualAssistActionRouting {
    pub agent_id: String,
    pub source_kind: String,
    pub source_key: String,
    pub session_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_url: Option<String>,
    pub target_text_kind: String,
    pub action_intent: String,
    pub personality: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextualAssistScreenshotRequest {
    pub policy: String,
    pub required: bool,
    pub source: String,
    pub reason: String,
    pub degraded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_rect: Option<ContextualAssistCaptureRect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_tab_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_window_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextualAssistVisualContext {
    pub screenshot: ContextualAssistScreenshotRequest,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualAssistActionResponse {
    pub status: String,
    pub action: ContextualAssistActionOption,
    pub context: ContextualAssistActionContext,
    pub routing: ContextualAssistActionRouting,
    pub visual_context: ContextualAssistVisualContext,
    pub message: String,
    pub next_binding: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attachment_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_attachment_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq)]
struct DetectedAssistTarget {
    native: NativeAssistTarget,
    anchor_x: f64,
    anchor_y: f64,
}

#[derive(Debug, Clone, PartialEq)]
struct BrowserProbeContext {
    app: Option<String>,
    window_title: Option<String>,
    window_rect: Option<ContextualAssistCaptureRect>,
    anchor_x: f64,
    anchor_y: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TauriWebviewContextPayload {
    #[serde(default)]
    pub route: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub selected_text: Option<String>,
    #[serde(default)]
    pub field_text: Option<String>,
    #[serde(default)]
    pub editable: bool,
    #[serde(default)]
    pub has_selection: bool,
    #[serde(default)]
    pub secure: bool,
}

#[derive(Debug, Clone)]
struct StoredTauriWebviewContext {
    payload: TauriWebviewContextPayload,
    updated_at: Instant,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrowserProbeResponse {
    #[serde(default)]
    eligible: bool,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    tab_title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    tab_url: Option<String>,
    #[serde(default)]
    frame_url: Option<String>,
    #[serde(default)]
    tab_id: Option<i64>,
    #[serde(default)]
    window_id: Option<i64>,
    #[serde(default)]
    is_writable: bool,
    #[serde(default)]
    has_selection: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ContextualAssistScreenshotPayload {
    image_b64: String,
    mime_type: String,
    capture_mode: String,
    source: String,
    degraded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_rect: Option<ContextualAssistCaptureRect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    browser_tab_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    browser_window_id: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrowserTabCaptureResponse {
    #[serde(default)]
    captured: bool,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    image_b64: Option<String>,
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default)]
    capture_mode: Option<String>,
    #[serde(default)]
    tab_id: Option<i64>,
    #[serde(default)]
    window_id: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContextualWritingExecutionResponse {
    status: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    draft_text: Option<String>,
    #[serde(default)]
    attachment_ids: Vec<String>,
    #[serde(default)]
    screenshot_attachment_id: Option<String>,
    #[serde(default)]
    provenance: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
struct ContextualAssistActionRun {
    id: u64,
    session_key: String,
    cancelled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatSessionListEnvelope {
    #[serde(default)]
    sessions: Vec<ChatSessionSummary>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatSessionSummary {
    id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CancelChatRunEnvelope {
    #[serde(default)]
    cancelled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualAssistCancelResponse {
    pub cancelled: bool,
    pub backend_cancelled: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum BrowserProbeOutcome {
    NotExtensionBrowser,
    Eligible(DetectedAssistTarget),
    /// The probe ran but found no text target. `secure_field` marks the one
    /// ineligibility that must suppress the menu entirely (a screenshot-backed
    /// fallback would visualize a password); every other reason — probe
    /// timeout, content-script unavailable, no selection — falls through to
    /// the screen-context fallback in the hotkey path.
    Ineligible {
        secure_field: bool,
    },
}

static LAST_DETECTED_TARGET: OnceLock<Mutex<Option<DetectedAssistTarget>>> = OnceLock::new();
static ACTIVE_CONTEXTUAL_ASSIST_ACTION: OnceLock<Mutex<Option<ContextualAssistActionRun>>> =
    OnceLock::new();
static ACTION_RUN_COUNTER: AtomicU64 = AtomicU64::new(1);
static HOTKEY_TAP_GENERATION: AtomicU64 = AtomicU64::new(0);
static HOTKEY_TAP_SUPPRESSED_UNTIL_MS: AtomicU64 = AtomicU64::new(0);
static HOTKEY_TAP_CLOCK_START: OnceLock<Instant> = OnceLock::new();
static TAURI_WEBVIEW_CONTEXT: OnceLock<Mutex<Option<StoredTauriWebviewContext>>> = OnceLock::new();
static CONTEXTUAL_ASSIST_HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

pub async fn contextual_assist_monitor(app: AppHandle) {
    start_input_tracking(app.clone());

    let mut pending: Option<DetectedAssistTarget> = None;
    let mut pending_since = Instant::now();
    let mut cached: Option<DetectedAssistTarget> = None;

    loop {
        sleep(POLL_INTERVAL).await;

        if overlay::contextual_assist_is_expanded() {
            continue;
        }

        let config = app
            .state::<AppState>()
            .config
            .lock()
            .await
            .contextual_assist
            .clone();

        // Detection and overlay updates hit AppKit (NSScreen via
        // `available_monitors()`); drain their autoreleased objects every
        // tick instead of leaking them on this tokio worker.
        crate::with_autorelease_pool(|| {
            poll_contextual_target(&app, &config, &mut pending, &mut pending_since, &mut cached)
        });
    }
}

fn poll_contextual_target(
    app: &AppHandle,
    config: &ContextualAssistConfig,
    pending: &mut Option<DetectedAssistTarget>,
    pending_since: &mut Instant,
    cached: &mut Option<DetectedAssistTarget>,
) {
    let detected =
        if config.explicit_hotkey_only || frontmost_browser_or_webview_active(app, config) {
            None
        } else {
            detect_contextual_target(app, config)
        };
    if detected != *pending {
        *pending = detected;
        *pending_since = Instant::now();
        return;
    }
    if pending_since.elapsed() < STABLE_TARGET_DELAY {
        return;
    }

    match pending.clone() {
        Some(target) if cached.as_ref() != Some(&target) => {
            overlay::update_contextual_assist_target(app, Some(target.native.clone()));
            store_latest_detected_target(Some(target.clone()));
            *cached = Some(target);
        },
        Some(_) => {},
        None => {
            if cached.is_some() {
                if overlay::contextual_assist_manual_grace_active() {
                    return;
                }
                let _ = overlay::hide_contextual_assist_window(app);
                store_latest_detected_target(None);
                *cached = None;
            }
        },
    }
}

fn detect_contextual_target(
    app: &AppHandle,
    config: &ContextualAssistConfig,
) -> Option<DetectedAssistTarget> {
    if !config.enabled {
        return None;
    }

    #[cfg(target_os = "macos")]
    {
        macos::detect_contextual_target(app, config)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, config);
        None
    }
}

fn screen_context_fallback_target(
    app: &AppHandle,
    config: &ContextualAssistConfig,
) -> Option<DetectedAssistTarget> {
    if !config.enabled {
        return None;
    }

    #[cfg(target_os = "macos")]
    {
        macos::screen_context_fallback_target(app, config)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, config);
        None
    }
}

/// Pure focus-verification for insertion: the frontmost app must be the
/// action's target app, and when both sides know a window title they must
/// agree. Missing titles degrade to the app check (titles are volatile);
/// a missing target app can never be verified.
fn insertion_target_matches(
    front_app: &str,
    front_window_title: Option<&str>,
    target_app: Option<&str>,
    target_window_title: Option<&str>,
) -> bool {
    let Some(target_app) = target_app.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    if !front_app.trim().eq_ignore_ascii_case(target_app) {
        return false;
    }
    match (
        target_window_title
            .map(str::trim)
            .filter(|value| !value.is_empty()),
        front_window_title
            .map(str::trim)
            .filter(|value| !value.is_empty()),
    ) {
        (Some(target_title), Some(front_title)) => front_title.eq_ignore_ascii_case(target_title),
        _ => true,
    }
}

/// Opaque handle to the user's previous clipboard, moved across the insert
/// await so the restore happens after the target app consumed the paste.
#[derive(Debug, Clone, Default)]
pub(crate) struct PasteboardRestoreToken(Option<String>);

/// True while an insertion is deliberately releasing the menu window's
/// keyboard focus. The window's `Focused(false)` handler must NOT treat
/// that as a user-initiated focus loss — emitting the dismiss request would
/// clear the draft the insertion is placing (and on abort, the draft the
/// user still needs on screen).
static CONTEXTUAL_INSERT_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

pub fn contextual_insert_in_progress() -> bool {
    CONTEXTUAL_INSERT_IN_PROGRESS.load(Ordering::SeqCst)
}

/// Sets the insert flag on creation and clears it on drop, so every return
/// path of the insert command (including panics unwinding) releases it.
struct ContextualInsertFlagGuard;

impl ContextualInsertFlagGuard {
    fn acquire() -> Self {
        CONTEXTUAL_INSERT_IN_PROGRESS.store(true, Ordering::SeqCst);
        Self
    }
}

impl Drop for ContextualInsertFlagGuard {
    fn drop(&mut self) {
        CONTEXTUAL_INSERT_IN_PROGRESS.store(false, Ordering::SeqCst);
    }
}

/// Screen-region rect of the frontmost (non-self) window. Used by the HUD's
/// attach-screen flow to capture "what the user was looking at" without any
/// text-target requirements. None when the frontmost window is this
/// process, untrusted, or has no readable frame.
pub fn frontmost_window_capture_rect(app: &AppHandle) -> Option<ContextualAssistCaptureRect> {
    #[cfg(target_os = "macos")]
    {
        macos::frontmost_window_capture_rect(app)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        None
    }
}

fn stage_paste_for_focused_app(app: &AppHandle, text: &str) -> Option<PasteboardRestoreToken> {
    #[cfg(target_os = "macos")]
    {
        macos::stage_paste_for_focused_app(app, text)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, text);
        None
    }
}

fn restore_pasteboard_after_insert(app: &AppHandle, token: &PasteboardRestoreToken) {
    #[cfg(target_os = "macos")]
    {
        macos::restore_pasteboard_after_insert(app, token);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, token);
    }
}

/// Result of a preview-card insertion attempt. `ok: false` always means
/// nothing was pasted anywhere — the draft stays on screen.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualInsertResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ContextualInsertResult {
    fn aborted(reason: impl Into<String>) -> Self {
        Self {
            ok: false,
            reason: Some(reason.into()),
        }
    }
}

/// @@-style result placement: paste the produced draft back into the target
/// the action ran against. The menu resigns keyboard focus (it stays
/// visible, so an aborted insert leaves the draft on screen), focus is
/// re-verified against the action's target — abort rather than paste into
/// the wrong app/window — then the text is staged on the pasteboard, Cmd+V
/// is synthesized for the focused app, and the user's previous clipboard is
/// restored. Over a live selection, paste replaces the selection; in a
/// field, it lands at the caret.
#[tauri::command]
pub async fn insert_contextual_text(app: AppHandle, text: String) -> ContextualInsertResult {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, text);
        return ContextualInsertResult::aborted("insertion requires macOS");
    }

    #[cfg(target_os = "macos")]
    {
        let text = text.trim().to_string();
        if text.is_empty() {
            return ContextualInsertResult::aborted("nothing to insert");
        }
        let Some(target) = overlay::current_contextual_assist_target() else {
            return ContextualInsertResult::aborted("no insertion target is bound");
        };
        // Acquired BEFORE the focus resign: the window's Focused(false)
        // handler checks this flag and must not dismiss the menu while the
        // insertion owns the focus handoff.
        let _insert_flag = ContextualInsertFlagGuard::acquire();

        // The expanded menu owns keyboard focus; resign it so the paste can
        // land in the user's app. Display state is untouched on purpose.
        if let Err(error) = overlay::set_contextual_assist_menu_keyboard_focus(&app, false) {
            return ContextualInsertResult::aborted(error);
        }
        tokio::time::sleep(Duration::from_millis(INSERT_FOCUS_SETTLE_MS)).await;

        let focus_verified = frontmost_app_and_window_title()
            .map(|(front_app, front_title)| {
                insertion_target_matches(
                    &front_app,
                    front_title.as_deref(),
                    target.app.as_deref(),
                    target.window_title.as_deref(),
                )
            })
            .unwrap_or(false);
        if !focus_verified {
            let _ = overlay::set_contextual_assist_menu_keyboard_focus(&app, true);
            return ContextualInsertResult::aborted("focus moved — nothing was inserted");
        }

        let Some(token) = stage_paste_for_focused_app(&app, &text) else {
            let _ = overlay::set_contextual_assist_menu_keyboard_focus(&app, true);
            return ContextualInsertResult::aborted("could not stage the paste");
        };
        tokio::time::sleep(Duration::from_millis(INSERT_PASTE_SETTLE_MS)).await;
        restore_pasteboard_after_insert(&app, &token);

        // Success: the frontend dismisses the menu; keyboard focus stays
        // with the user's app, where the pasted text now is.
        ContextualInsertResult {
            ok: true,
            reason: None,
        }
    }
}

fn frontmost_extension_browser_context(
    app: &AppHandle,
    config: &ContextualAssistConfig,
) -> Option<BrowserProbeContext> {
    #[cfg(target_os = "macos")]
    {
        macos::frontmost_extension_browser_context(app, config)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, config);
        None
    }
}

/// Frontmost app name and focused-window title, independent of any text
/// target. Used by the screen-ask chords to stamp captures with the context
/// they were taken in. Returns None without Accessibility trust or off-macOS.
pub fn frontmost_app_and_window_title() -> Option<(String, Option<String>)> {
    #[cfg(target_os = "macos")]
    {
        macos::frontmost_app_and_window_title()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

async fn finder_selection_target(
    app: &AppHandle,
    config: &ContextualAssistConfig,
) -> Option<DetectedAssistTarget> {
    if !config.enabled {
        return None;
    }

    #[cfg(target_os = "macos")]
    {
        macos::finder_selection_target(app, config).await
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, config);
        None
    }
}

fn frontmost_browser_or_webview_active(app: &AppHandle, config: &ContextualAssistConfig) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::frontmost_browser_or_webview_active(app, config)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, config);
        false
    }
}

fn frontmost_webview_hotkey_target(
    app: &AppHandle,
    config: &ContextualAssistConfig,
) -> Option<DetectedAssistTarget> {
    #[cfg(target_os = "macos")]
    {
        macos::frontmost_webview_hotkey_target(app, config)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, config);
        None
    }
}

fn frontmost_webview_empty_hotkey_target(
    app: &AppHandle,
    config: &ContextualAssistConfig,
) -> Option<DetectedAssistTarget> {
    #[cfg(target_os = "macos")]
    {
        macos::frontmost_webview_empty_hotkey_target(app, config)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, config);
        None
    }
}

fn frontmost_tauri_webview_context(
    app: &AppHandle,
    config: &ContextualAssistConfig,
) -> Option<BrowserProbeContext> {
    #[cfg(target_os = "macos")]
    {
        macos::frontmost_tauri_webview_context(app, config)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, config);
        None
    }
}

fn latest_tauri_webview_hotkey_target(
    app: &AppHandle,
    config: &ContextualAssistConfig,
) -> Option<DetectedAssistTarget> {
    let context = frontmost_tauri_webview_context(app, config)?;
    let stored = tauri_webview_context_store()
        .lock()
        .ok()
        .and_then(|stored| stored.clone())?;
    if stored.updated_at.elapsed() > TAURI_WEBVIEW_CONTEXT_MAX_AGE {
        return None;
    }
    tauri_webview_target_from_payload(stored.payload, context, config)
}

async fn probe_browser_contextual_target(
    app: &AppHandle,
    config: &ContextualAssistConfig,
    magicutor_port: u16,
) -> BrowserProbeOutcome {
    let Some(context) = frontmost_extension_browser_context(app, config) else {
        return BrowserProbeOutcome::NotExtensionBrowser;
    };

    let url = format!("http://127.0.0.1:{magicutor_port}/contextual-assist/probe");
    let Ok(response) = contextual_assist_http_client()
        .post(url)
        .timeout(Duration::from_millis(BROWSER_PROBE_TIMEOUT_MS))
        .send()
        .await
    else {
        return BrowserProbeOutcome::Ineligible {
            secure_field: false,
        };
    };
    if !response.status().is_success() {
        return BrowserProbeOutcome::Ineligible {
            secure_field: false,
        };
    }

    let Ok(probe) = response.json::<BrowserProbeResponse>().await else {
        return BrowserProbeOutcome::Ineligible {
            secure_field: false,
        };
    };
    if !probe.eligible {
        return BrowserProbeOutcome::Ineligible {
            secure_field: probe
                .reason
                .as_deref()
                .is_some_and(|reason| reason.eq_ignore_ascii_case("secure_field")),
        };
    }
    if !browser_probe_matches_window_title(
        &probe,
        context.window_title.as_deref(),
        context.app.as_deref(),
    ) {
        return BrowserProbeOutcome::Ineligible {
            secure_field: false,
        };
    }

    BrowserProbeOutcome::Eligible(DetectedAssistTarget {
        native: NativeAssistTarget {
            state: state_from_browser_probe(&probe),
            app: context.app,
            window_title: context.window_title,
            url: probe.url.or(probe.tab_url),
            context_text: None,
            source: Some("chrome_extension_probe".to_string()),
            frame_url: probe.frame_url,
            browser_tab_id: probe.tab_id,
            browser_window_id: probe.window_id,
            window_rect: context.window_rect,
        },
        anchor_x: context.anchor_x,
        anchor_y: context.anchor_y,
    })
}

fn state_from_browser_probe(probe: &BrowserProbeResponse) -> String {
    match probe.state.as_deref() {
        Some("selection" | "selection-field" | "draft" | "empty-context") => {
            return probe.state.clone().unwrap_or_default();
        },
        _ => {},
    }

    let reason = probe
        .reason
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if probe.has_selection && probe.is_writable {
        "selection-field".to_string()
    } else if probe.has_selection || reason.contains("selection") {
        "selection".to_string()
    } else if probe.is_writable {
        "empty-context".to_string()
    } else {
        "selection".to_string()
    }
}

fn browser_probe_matches_window_title(
    probe: &BrowserProbeResponse,
    window_title: Option<&str>,
    app_name: Option<&str>,
) -> bool {
    let Some(window_title) = window_title.and_then(normalized_nonempty) else {
        return true;
    };
    if app_name
        .and_then(normalized_nonempty)
        .map(|app_name| app_name == window_title)
        .unwrap_or(false)
    {
        return true;
    }
    let Some(tab_title) = probe
        .title
        .as_deref()
        .or(probe.tab_title.as_deref())
        .and_then(normalized_nonempty)
    else {
        return true;
    };

    window_title.contains(&tab_title) || tab_title.contains(&window_title)
}

fn normalized_nonempty(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    (!normalized.is_empty()).then_some(normalized)
}

fn context_text_from_str(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(MAX_CONTEXT_TEXT_CHARS).collect())
}

/// Backend catalog cache (see `get_contextual_assist_action_catalog`).
/// The slot holds `(fetched_at, states)`; an EMPTY vec is the failure
/// marker (backend unreachable, booting, or stale) so a down backend is
/// remembered briefly instead of stalling every menu open on the fetch
/// timeout — the desktop commonly starts before the backend is serving.
static BACKEND_ACTION_CATALOG: OnceLock<
    Mutex<Option<(Instant, Vec<ContextualAssistActionState>)>>,
> = OnceLock::new();
const ACTION_CATALOG_TTL: Duration = Duration::from_secs(10 * 60);
const ACTION_CATALOG_FAILURE_TTL: Duration = Duration::from_secs(30);
const ACTION_CATALOG_FETCH_TIMEOUT_MS: u64 = 1_500;

/// Fetch the backend's canonical action catalog (`GET
/// /contextual-writing/catalog`). The backend copy is used ONLY when it is
/// a superset of the local catalog's state ids — an older backend must
/// never hide desktop-native states. Any failure keeps the local catalog,
/// which is byte-for-byte what this crate served before the endpoint
/// existed, and is remembered for `ACTION_CATALOG_FAILURE_TTL` so repeated
/// menu opens fall back instantly.
async fn fetch_backend_action_catalog(
    desktop: &crate::config::MagicianDesktopConfig,
) -> Option<Vec<ContextualAssistActionState>> {
    let cache = BACKEND_ACTION_CATALOG.get_or_init(|| Mutex::new(None));
    if let Ok(slot) = cache.lock() {
        if let Some((fetched_at, catalog)) = slot.as_ref() {
            let ttl = if catalog.is_empty() {
                ACTION_CATALOG_FAILURE_TTL
            } else {
                ACTION_CATALOG_TTL
            };
            if fetched_at.elapsed() < ttl {
                return (!catalog.is_empty()).then(|| catalog.clone());
            }
        }
    }

    let fetched = fetch_backend_action_catalog_uncached(desktop).await;
    let catalog = fetched.unwrap_or_default();
    if let Ok(mut slot) = cache.lock() {
        *slot = Some((Instant::now(), catalog.clone()));
    }
    (!catalog.is_empty()).then_some(catalog)
}

async fn fetch_backend_action_catalog_uncached(
    desktop: &crate::config::MagicianDesktopConfig,
) -> Option<Vec<ContextualAssistActionState>> {
    let url = desktop.engine_url("/api/magician/v2/contextual-writing/catalog");
    let response = crate::magician_auth::authorize(contextual_assist_http_client().get(&url))
        .timeout(Duration::from_millis(ACTION_CATALOG_FETCH_TIMEOUT_MS))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let payload: serde_json::Value = response.json().await.ok()?;
    let catalog: Vec<ContextualAssistActionState> =
        serde_json::from_value(payload.get("states")?.clone()).ok()?;
    if catalog.is_empty() {
        return None;
    }

    // Superset guard: a stale backend must not remove local states.
    let local_catalog = contextual_assist_action_catalog();
    let local_state_ids: std::collections::HashSet<&str> = local_catalog
        .iter()
        .map(|state| state.id.as_str())
        .collect();
    let backend_state_ids: std::collections::HashSet<&str> =
        catalog.iter().map(|state| state.id.as_str()).collect();
    if !local_state_ids.is_subset(&backend_state_ids) {
        warn!("backend contextual-writing catalog is missing local states; using the local copy");
        return None;
    }

    Some(catalog)
}

/// Whether macOS Accessibility trust is granted. Without it the watcher can
/// detect nothing and every invoke silently falls back — the menu surfaces
/// this so the first run explains itself instead of doing nothing.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualAssistPermissionStatus {
    pub ax_trusted: bool,
}

pub fn ax_trusted() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::ax_trusted()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

#[tauri::command]
pub fn get_contextual_assist_permission_status() -> ContextualAssistPermissionStatus {
    ContextualAssistPermissionStatus {
        ax_trusted: ax_trusted(),
    }
}

#[tauri::command]
pub async fn get_contextual_assist_action_catalog(
    app: AppHandle,
) -> Result<Vec<ContextualAssistActionState>, String> {
    let desktop = app.state::<AppState>().config.lock().await.clone();
    if let Some(catalog) = fetch_backend_action_catalog(&desktop).await {
        return Ok(catalog);
    }
    Ok(contextual_assist_action_catalog())
}

#[tauri::command]
pub async fn invoke_contextual_assist_action(
    app: AppHandle,
    request: ContextualAssistActionRequest,
) -> Result<ContextualAssistActionResponse, String> {
    let (config, desktop, magicutor_port) = {
        let state = app.state::<AppState>();
        let desktop = state.config.lock().await.clone();
        let assist = desktop.contextual_assist.clone();
        let magicutor_port = desktop.network.magicutor_port;
        (assist, desktop, magicutor_port)
    };
    if !config.enabled {
        return Err("Contextual Assist is disabled".to_string());
    }

    let action_id = request.action_id.trim();
    if action_id.is_empty() {
        return Err("action_id is required".to_string());
    }
    let user_prompt = request
        .user_prompt
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let reuse_screenshot_attachment_id = request
        .reuse_screenshot_attachment_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let target = overlay::current_contextual_assist_target().or(request.target.clone());
    let state =
        contextual_assist_state_from_target_or_request(target.as_ref(), request.state.as_deref());
    let action = contextual_assist_action_for_state(&state, action_id).ok_or_else(|| {
        format!("action `{action_id}` is not available for contextual state `{state}`")
    })?;
    let personality = normalize_action_personality(request.personality.as_deref(), &config);
    let context = contextual_assist_action_context(state, personality, target.as_ref());
    let routing = contextual_assist_action_routing(&action, &context);
    let visual_context = contextual_assist_visual_context(&action, target.as_ref());

    if action.opens_hud {
        return Ok(ContextualAssistActionResponse {
            status: ACTION_STATUS_PENDING_BINDING.to_string(),
            action,
            context,
            routing,
            visual_context,
            message: "HUD handoff is pending; choose a writing action for draft generation."
                .to_string(),
            next_binding: ACTION_BINDING_MAGICIAN_EXECUTE.to_string(),
            draft_text: None,
            session_id: None,
            thread_id: None,
            attachment_ids: Vec::new(),
            screenshot_attachment_id: None,
            provenance: None,
        });
    }

    let action_run_id = begin_contextual_assist_action_run(&routing.session_key);
    let result = async {
        if contextual_assist_action_cancelled(action_run_id) {
            return Err("Contextual Assist action cancelled.".to_string());
        }
        let screenshot = if reuse_screenshot_attachment_id.is_some()
            && visual_context.screenshot.required
        {
            None
        } else {
            capture_contextual_assist_screenshot(&app, magicutor_port, &visual_context.screenshot)
                .await?
        };
        if contextual_assist_action_cancelled(action_run_id) {
            return Err("Contextual Assist action cancelled.".to_string());
        }
        let execution = post_contextual_assist_action(
            &desktop,
            &action,
            &context,
            &routing,
            &visual_context,
            screenshot.as_ref(),
            reuse_screenshot_attachment_id.as_deref(),
            user_prompt.as_deref(),
        )
        .await?;
        if contextual_assist_action_cancelled(action_run_id) {
            return Err("Contextual Assist action cancelled.".to_string());
        }
        let message = match execution.status.as_str() {
            ACTION_STATUS_DRAFT_READY => "Draft ready.".to_string(),
            "queued" => "Writing request queued.".to_string(),
            "accepted" => "Writing request accepted.".to_string(),
            other => format!("Writing request {other}."),
        };

        Ok(ContextualAssistActionResponse {
            status: execution.status,
            action,
            context,
            routing,
            visual_context,
            message,
            next_binding: ACTION_BINDING_MAGICIAN_EXECUTE.to_string(),
            draft_text: execution.draft_text,
            session_id: execution.session_id,
            thread_id: execution.thread_id,
            attachment_ids: execution.attachment_ids,
            screenshot_attachment_id: execution.screenshot_attachment_id,
            provenance: execution.provenance,
        })
    }
    .await;
    finish_contextual_assist_action_run(action_run_id);
    result
}

#[tauri::command]
pub async fn cancel_contextual_assist_action(
    app: AppHandle,
) -> Result<ContextualAssistCancelResponse, String> {
    let Some(session_key) = mark_contextual_assist_action_cancelled() else {
        return Ok(ContextualAssistCancelResponse {
            cancelled: false,
            backend_cancelled: false,
        });
    };
    let backend_cancelled = cancel_contextual_assist_backend_run(&app, &session_key)
        .await
        .unwrap_or(false);
    Ok(ContextualAssistCancelResponse {
        cancelled: true,
        backend_cancelled,
    })
}

async fn post_contextual_assist_action(
    desktop: &crate::config::MagicianDesktopConfig,
    action: &ContextualAssistActionOption,
    context: &ContextualAssistActionContext,
    routing: &ContextualAssistActionRouting,
    visual_context: &ContextualAssistVisualContext,
    screenshot: Option<&ContextualAssistScreenshotPayload>,
    reuse_screenshot_attachment_id: Option<&str>,
    user_prompt: Option<&str>,
) -> Result<ContextualWritingExecutionResponse, String> {
    let url = desktop.engine_url("/api/magician/v2/contextual-writing/actions");
    let body = serde_json::json!({
        "action": action,
        "context": context,
        "routing": routing,
        "visualContext": visual_context,
        "screenshot": screenshot,
        "reuseScreenshotAttachmentId": reuse_screenshot_attachment_id,
        "userPrompt": user_prompt,
    });
    let response = crate::magician_auth::authorize(contextual_assist_http_client().post(&url))
        .timeout(Duration::from_secs(ACTION_EXECUTION_TIMEOUT_SECS))
        .json(&body)
        .send()
        .await
        .map_err(|error| format!("POST {url}: {error}"))?;
    let status = response.status();
    let body_text = response
        .text()
        .await
        .map_err(|error| format!("read contextual writing response: {error}"))?;
    if !status.is_success() {
        let detail = contextual_writing_error_detail(&body_text);
        return Err(format!("contextual writing returned {status}: {detail}"));
    }
    serde_json::from_str::<ContextualWritingExecutionResponse>(&body_text)
        .map_err(|error| format!("parse contextual writing response: {error}"))
}

fn contextual_writing_error_detail(body_text: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body_text)
        .ok()
        .and_then(|body| {
            body.get("details")
                .and_then(|details| {
                    details
                        .get("reason")
                        .and_then(|value| value.as_str())
                        .or_else(|| details.as_str())
                })
                .or_else(|| body.get("error").and_then(|value| value.as_str()))
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| body_text.trim().to_string())
}

async fn cancel_contextual_assist_backend_run(
    app: &AppHandle,
    session_key: &str,
) -> Result<bool, String> {
    let desktop = app.state::<AppState>().config.lock().await.clone();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| format!("http client: {error}"))?;
    let list_url = desktop.engine_url("/api/magician/v2/chat/sessions");
    let response = crate::magician_auth::authorize(client.get(&list_url))
        .query(&[("ui_thread_id", session_key)])
        .send()
        .await
        .map_err(|error| format!("GET {list_url}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "list contextual writing sessions returned {}",
            response.status()
        ));
    }
    let sessions = response
        .json::<ChatSessionListEnvelope>()
        .await
        .map_err(|error| format!("parse chat sessions response: {error}"))?;
    let mut backend_cancelled = false;
    for session in sessions.sessions {
        let cancel_url = desktop.engine_url(&format!(
            "/api/magician/v2/chat/sessions/{}/run",
            session.id
        ));
        let response = crate::magician_auth::authorize(client.delete(&cancel_url))
            .send()
            .await
            .map_err(|error| format!("DELETE {cancel_url}: {error}"))?;
        if !response.status().is_success() {
            continue;
        }
        let cancelled = response
            .json::<CancelChatRunEnvelope>()
            .await
            .map(|body| body.cancelled)
            .unwrap_or(false);
        backend_cancelled |= cancelled;
    }
    Ok(backend_cancelled)
}

async fn capture_contextual_assist_screenshot(
    app: &AppHandle,
    magicutor_port: u16,
    request: &ContextualAssistScreenshotRequest,
) -> Result<Option<ContextualAssistScreenshotPayload>, String> {
    if !request.required {
        return Ok(None);
    }
    if request.source == SCREENSHOT_SOURCE_CHROME_VISIBLE_TAB {
        match capture_contextual_assist_browser_tab(magicutor_port, request).await {
            Ok(Some(screenshot)) => return Ok(Some(screenshot)),
            Ok(None) => {
                warn!("browser visible-tab screenshot returned no payload; falling back to native screenshot");
            },
            Err(error) => {
                warn!("browser visible-tab screenshot failed; falling back to native screenshot: {error}");
            },
        }
        let fallback_request = contextual_assist_browser_capture_fallback_request(request);
        return capture_contextual_assist_native_screenshot(app, &fallback_request)
            .await
            .map(Some);
    }
    capture_contextual_assist_native_screenshot(app, request)
        .await
        .map(Some)
}

fn contextual_assist_browser_capture_fallback_request(
    request: &ContextualAssistScreenshotRequest,
) -> ContextualAssistScreenshotRequest {
    let has_window_rect = request.window_rect.is_some();
    ContextualAssistScreenshotRequest {
        policy: request.policy.clone(),
        required: request.required,
        source: if has_window_rect {
            SCREENSHOT_SOURCE_ACTIVE_WINDOW_REGION
        } else {
            SCREENSHOT_SOURCE_FULL_SCREEN_FALLBACK
        }
        .to_string(),
        reason: "browser_visible_tab_capture_unavailable".to_string(),
        degraded: true,
        window_rect: request.window_rect.clone(),
        browser_tab_id: request.browser_tab_id,
        browser_window_id: request.browser_window_id,
    }
}

async fn capture_contextual_assist_browser_tab(
    magicutor_port: u16,
    request: &ContextualAssistScreenshotRequest,
) -> Result<Option<ContextualAssistScreenshotPayload>, String> {
    let url = format!("http://127.0.0.1:{magicutor_port}/contextual-assist/capture-tab");
    let response = contextual_assist_http_client()
        .post(&url)
        .timeout(Duration::from_secs(20))
        .json(&serde_json::json!({ "tabId": request.browser_tab_id }))
        .send()
        .await
        .map_err(|error| format!("POST {url}: {error}"))?;
    let status = response.status();
    let capture = response
        .json::<BrowserTabCaptureResponse>()
        .await
        .map_err(|error| format!("parse browser capture response: {error}"))?;
    if !status.is_success() || !capture.captured {
        let reason = capture.reason.unwrap_or_else(|| {
            capture
                .error
                .unwrap_or_else(|| format!("capture endpoint returned {status}"))
        });
        return Err(format!("browser tab screenshot unavailable: {reason}"));
    }
    let Some(image_b64) = capture.image_b64.filter(|value| !value.trim().is_empty()) else {
        return Err("browser tab screenshot response did not include image data".to_string());
    };
    Ok(Some(ContextualAssistScreenshotPayload {
        image_b64,
        mime_type: capture.mime_type.unwrap_or_else(|| "image/png".to_string()),
        capture_mode: capture
            .capture_mode
            .unwrap_or_else(|| SCREENSHOT_SOURCE_CHROME_VISIBLE_TAB.to_string()),
        source: SCREENSHOT_SOURCE_CHROME_VISIBLE_TAB.to_string(),
        degraded: request.degraded,
        window_rect: None,
        browser_tab_id: capture.tab_id.or(request.browser_tab_id),
        browser_window_id: capture.window_id.or(request.browser_window_id),
    }))
}

fn contextual_assist_http_client() -> &'static reqwest::Client {
    CONTEXTUAL_ASSIST_HTTP_CLIENT.get_or_init(reqwest::Client::new)
}

async fn capture_contextual_assist_native_screenshot(
    app: &AppHandle,
    request: &ContextualAssistScreenshotRequest,
) -> Result<ContextualAssistScreenshotPayload, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        let _ = request;
        return Err(
            "contextual screenshot capture is currently available on macOS only".to_string(),
        );
    }

    #[cfg(target_os = "macos")]
    {
        let mut output_path = std::env::temp_dir();
        output_path.push(format!(
            "magician-contextual-assist-{}.png",
            contextual_assist_epoch_millis()
        ));
        let mut command = Command::new(SCREENCAPTURE_BIN);
        command.arg("-x").arg("-t").arg("png");
        if let Some(rect) = request.window_rect.as_ref() {
            command.arg("-R").arg(format!(
                "{},{},{},{}",
                rect.x, rect.y, rect.width, rect.height
            ));
        }
        command.arg(&output_path);
        let output = command
            .output()
            .await
            .map_err(|error| format!("run screencapture: {error}"))?;
        if !output.status.success() {
            let _ = tokio::fs::remove_file(&output_path).await;
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(if stderr.is_empty() {
                format!("screencapture exited with {}", output.status)
            } else {
                format!("screencapture failed: {stderr}")
            });
        }
        let bytes = tokio::fs::read(&output_path)
            .await
            .map_err(|error| format!("read screenshot: {error}"))?;
        let _ = tokio::fs::remove_file(&output_path).await;
        if bytes.is_empty() {
            return Err("screenshot capture produced an empty file".to_string());
        }
        let _ = app;
        Ok(ContextualAssistScreenshotPayload {
            image_b64: BASE64.encode(bytes),
            mime_type: "image/png".to_string(),
            capture_mode: if request.window_rect.is_some() {
                "active_window_region".to_string()
            } else {
                "full_screen_fallback".to_string()
            },
            source: request.source.clone(),
            degraded: request.degraded,
            window_rect: request.window_rect.clone(),
            browser_tab_id: request.browser_tab_id,
            browser_window_id: request.browser_window_id,
        })
    }
}

fn contextual_assist_epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn contextual_assist_action_run_store() -> &'static Mutex<Option<ContextualAssistActionRun>> {
    ACTIVE_CONTEXTUAL_ASSIST_ACTION.get_or_init(|| Mutex::new(None))
}

fn begin_contextual_assist_action_run(session_key: &str) -> u64 {
    let id = ACTION_RUN_COUNTER.fetch_add(1, Ordering::SeqCst);
    if let Ok(mut slot) = contextual_assist_action_run_store().lock() {
        *slot = Some(ContextualAssistActionRun {
            id,
            session_key: session_key.to_string(),
            cancelled: false,
        });
    }
    id
}

fn contextual_assist_action_cancelled(id: u64) -> bool {
    contextual_assist_action_run_store()
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().map(|run| run.id == id && run.cancelled))
        .unwrap_or(false)
}

fn mark_contextual_assist_action_cancelled() -> Option<String> {
    let mut slot = contextual_assist_action_run_store().lock().ok()?;
    let run = slot.as_mut()?;
    run.cancelled = true;
    Some(run.session_key.clone())
}

fn finish_contextual_assist_action_run(id: u64) {
    if let Ok(mut slot) = contextual_assist_action_run_store().lock() {
        if slot.as_ref().map(|run| run.id == id).unwrap_or(false) {
            *slot = None;
        }
    }
}

fn contextual_assist_action_catalog() -> Vec<ContextualAssistActionState> {
    vec![
        action_state(
            STATE_SELECTION,
            "Selected text",
            vec![
                action_option(
                    ACTION_REWRITE,
                    "Rewrite",
                    "Rewrite the selected text.",
                    ACTION_KIND_PRIMARY,
                    "rewrite_selection",
                    ActionFlags {
                        requires_context: true,
                        mutates_text: true,
                        ..ActionFlags::default()
                    },
                ),
                action_option(
                    ACTION_SUMMARIZE,
                    "Summarize",
                    "Summarize the selected text.",
                    ACTION_KIND_PRIMARY,
                    "summarize_selection",
                    ActionFlags {
                        requires_context: true,
                        ..ActionFlags::default()
                    },
                ),
                action_option(
                    ACTION_SAVE_TO_NOTES,
                    "Save to Notes",
                    "Save the selected text to Notes with where it came from.",
                    ACTION_KIND_SECONDARY,
                    "save_selection_to_notes",
                    ActionFlags {
                        requires_context: true,
                        ..ActionFlags::default()
                    },
                ),
                action_option(
                    ACTION_DRAFT_REPLY,
                    "Reply",
                    "Draft a reply using selected and visible context.",
                    ACTION_KIND_PRIMARY,
                    "draft_reply_to_selection",
                    ActionFlags {
                        requires_context: true,
                        mutates_text: true,
                        ..ActionFlags::default()
                    },
                ),
                create_task_action("Create an unscheduled task proposal from this text.", false),
                schedule_followup_action("Create a dated follow-up from this text.", false),
                open_hud_action("Open the full HUD with this context."),
            ],
        ),
        action_state(
            STATE_SELECTION_FIELD,
            "Selection in field",
            vec![
                action_option(
                    ACTION_REWRITE,
                    "Rewrite",
                    "Replace only the selected text.",
                    ACTION_KIND_PRIMARY,
                    "rewrite_field_selection",
                    ActionFlags {
                        requires_context: true,
                        requires_writable_target: true,
                        mutates_text: true,
                        ..ActionFlags::default()
                    },
                ),
                action_option(
                    ACTION_SHORTEN,
                    "Shorten",
                    "Make the selected text tighter.",
                    ACTION_KIND_PRIMARY,
                    "shorten_field_selection",
                    ActionFlags {
                        requires_context: true,
                        requires_writable_target: true,
                        mutates_text: true,
                        ..ActionFlags::default()
                    },
                ),
                action_option(
                    ACTION_CLARIFY,
                    "Clarify",
                    "Clarify the selected text.",
                    ACTION_KIND_PRIMARY,
                    "clarify_field_selection",
                    ActionFlags {
                        requires_context: true,
                        requires_writable_target: true,
                        mutates_text: true,
                        ..ActionFlags::default()
                    },
                ),
                action_option(
                    ACTION_CONTINUE_DRAFT,
                    "Continue",
                    "Continue after the selected text.",
                    ACTION_KIND_PRIMARY,
                    "continue_after_selection",
                    ActionFlags {
                        requires_context: true,
                        requires_writable_target: true,
                        mutates_text: true,
                        ..ActionFlags::default()
                    },
                ),
                create_task_action("Turn the selection into a task proposal.", false),
                open_hud_action("Open the full HUD with this selection."),
            ],
        ),
        action_state(
            STATE_EMPTY_CONTEXT,
            "Empty field + context",
            vec![
                action_option(
                    ACTION_DRAFT_REPLY,
                    "Draft reply",
                    "Use current context to draft a reply.",
                    ACTION_KIND_PRIMARY,
                    "draft_reply_from_context",
                    ActionFlags {
                        requires_context: true,
                        requires_writable_target: true,
                        mutates_text: true,
                        ..ActionFlags::default()
                    },
                ),
                action_option(
                    ACTION_WRITE_FROM_CONTEXT,
                    "Write",
                    "Start writing from the current context.",
                    ACTION_KIND_PRIMARY,
                    "write_from_context",
                    ActionFlags {
                        requires_context: true,
                        requires_writable_target: true,
                        mutates_text: true,
                        ..ActionFlags::default()
                    },
                ),
                create_task_action("Create a task proposal from the current context.", false),
                schedule_followup_action(
                    "Create a scheduled follow-up from the current context.",
                    false,
                ),
                open_hud_action("Open the full HUD with this context."),
            ],
        ),
        action_state(
            STATE_EMPTY_NO_CONTEXT,
            "Empty field",
            vec![
                action_option(
                    ACTION_OBSERVE_THEN_DRAFT,
                    "Observe + draft",
                    "Read the current screen, then draft.",
                    ACTION_KIND_PRIMARY,
                    "observe_then_draft",
                    ActionFlags {
                        requires_observation: true,
                        requires_writable_target: true,
                        mutates_text: true,
                        ..ActionFlags::default()
                    },
                ),
                action_option(
                    ACTION_WRITE_FROM_CONTEXT,
                    "Start writing",
                    "Start a blank draft with the selected personality.",
                    ACTION_KIND_PRIMARY,
                    "start_blank_draft",
                    ActionFlags {
                        requires_writable_target: true,
                        mutates_text: true,
                        ..ActionFlags::default()
                    },
                ),
                create_task_action("Read the screen and create a task proposal.", true),
                schedule_followup_action("Read the screen and create a scheduled follow-up.", true),
                open_hud_action("Use the larger HUD for a broad request."),
            ],
        ),
        action_state(
            STATE_PAGE_CONTEXT,
            "Web page",
            vec![
                action_option(
                    ACTION_SUMMARIZE_PAGE,
                    "Summarize page",
                    "Summarize the current page from its URL and visible content.",
                    ACTION_KIND_PRIMARY,
                    "summarize_page",
                    ActionFlags {
                        requires_context: true,
                        ..ActionFlags::default()
                    },
                ),
                create_task_action("Create a task proposal from this page.", false),
                open_hud_action("Ask about this page in the full HUD."),
            ],
        ),
        action_state(
            STATE_FILES,
            "Finder selection",
            vec![
                action_option(
                    ACTION_SUMMARIZE,
                    "Summarize",
                    "Summarize the selected files from their names, paths, and the visible window.",
                    ACTION_KIND_PRIMARY,
                    "summarize_file_selection",
                    ActionFlags {
                        requires_observation: true,
                        ..ActionFlags::default()
                    },
                ),
                create_task_action("Create a task proposal from the selected files.", true),
                open_hud_action("Ask about these files in the full HUD."),
            ],
        ),
        action_state(
            STATE_DRAFT,
            "Non-empty field",
            vec![
                action_option(
                    ACTION_CONTINUE_DRAFT,
                    "Continue",
                    "Continue the existing draft.",
                    ACTION_KIND_PRIMARY,
                    "continue_draft",
                    ActionFlags {
                        requires_context: true,
                        requires_writable_target: true,
                        mutates_text: true,
                        ..ActionFlags::default()
                    },
                ),
                action_option(
                    ACTION_IMPROVE_DRAFT,
                    "Improve",
                    "Improve the draft without changing intent.",
                    ACTION_KIND_PRIMARY,
                    "improve_draft",
                    ActionFlags {
                        requires_context: true,
                        requires_writable_target: true,
                        mutates_text: true,
                        ..ActionFlags::default()
                    },
                ),
                action_option(
                    ACTION_SHORTEN,
                    "Shorten",
                    "Make the draft more concise.",
                    ACTION_KIND_PRIMARY,
                    "shorten_draft",
                    ActionFlags {
                        requires_context: true,
                        requires_writable_target: true,
                        mutates_text: true,
                        ..ActionFlags::default()
                    },
                ),
                action_option(
                    ACTION_CLARIFY,
                    "Clarify",
                    "Clarify the draft.",
                    ACTION_KIND_PRIMARY,
                    "clarify_draft",
                    ActionFlags {
                        requires_context: true,
                        requires_writable_target: true,
                        mutates_text: true,
                        ..ActionFlags::default()
                    },
                ),
                create_task_action("Create a task proposal from the draft.", false),
                schedule_followup_action("Create a dated follow-up from the draft.", false),
                open_hud_action("Open the full HUD with this draft."),
            ],
        ),
        action_state(STATE_SECURE, "Secure field", vec![]),
        action_state(
            STATE_EXCLUDED,
            "Excluded app",
            vec![open_hud_action("Open the HUD without contextual text.")],
        ),
        action_state(
            STATE_UNSUPPORTED,
            "Unsupported target",
            vec![open_hud_action("Use the full HUD and copy text manually.")],
        ),
    ]
}

#[derive(Debug, Clone, Copy, Default)]
struct ActionFlags {
    requires_context: bool,
    requires_observation: bool,
    requires_screenshot: bool,
    requires_writable_target: bool,
    mutates_text: bool,
    creates_task: bool,
    opens_hud: bool,
}

fn action_state(
    id: &str,
    label: &str,
    actions: Vec<ContextualAssistActionOption>,
) -> ContextualAssistActionState {
    ContextualAssistActionState {
        id: id.to_string(),
        label: label.to_string(),
        actions,
    }
}

fn action_option(
    id: &str,
    label: &str,
    description: &str,
    kind: &str,
    intent: &str,
    flags: ActionFlags,
) -> ContextualAssistActionOption {
    let requires_screenshot =
        flags.requires_screenshot || default_action_requires_screenshot(id, flags);
    ContextualAssistActionOption {
        id: id.to_string(),
        label: label.to_string(),
        description: description.to_string(),
        kind: kind.to_string(),
        intent: intent.to_string(),
        requires_context: flags.requires_context,
        requires_observation: flags.requires_observation,
        requires_screenshot,
        requires_writable_target: flags.requires_writable_target,
        mutates_text: flags.mutates_text,
        creates_task: flags.creates_task,
        opens_hud: flags.opens_hud,
    }
}

fn default_action_requires_screenshot(action_id: &str, flags: ActionFlags) -> bool {
    if action_id == ACTION_SHORTEN || flags.opens_hud {
        return false;
    }
    flags.requires_context || flags.requires_observation || flags.mutates_text || flags.creates_task
}

fn create_task_action(
    description: &str,
    requires_observation: bool,
) -> ContextualAssistActionOption {
    action_option(
        ACTION_CREATE_TASK,
        "Task",
        description,
        ACTION_KIND_SECONDARY,
        "create_task",
        ActionFlags {
            requires_context: !requires_observation,
            requires_observation,
            creates_task: true,
            ..ActionFlags::default()
        },
    )
}

fn schedule_followup_action(
    description: &str,
    requires_observation: bool,
) -> ContextualAssistActionOption {
    action_option(
        ACTION_SCHEDULE_FOLLOWUP,
        "Follow-up",
        description,
        ACTION_KIND_SECONDARY,
        "schedule_followup",
        ActionFlags {
            requires_context: !requires_observation,
            requires_observation,
            creates_task: true,
            ..ActionFlags::default()
        },
    )
}

fn open_hud_action(description: &str) -> ContextualAssistActionOption {
    action_option(
        ACTION_OPEN_HUD,
        "HUD",
        description,
        ACTION_KIND_SECONDARY,
        "open_hud",
        ActionFlags {
            opens_hud: true,
            ..ActionFlags::default()
        },
    )
}

fn contextual_assist_action_for_state(
    state: &str,
    action_id: &str,
) -> Option<ContextualAssistActionOption> {
    contextual_assist_action_catalog()
        .into_iter()
        .find(|entry| entry.id == state)
        .and_then(|state| {
            state
                .actions
                .into_iter()
                .find(|action| action.id == action_id)
        })
}

fn contextual_assist_state_from_target_or_request(
    target: Option<&NativeAssistTarget>,
    request_state: Option<&str>,
) -> String {
    target
        .and_then(|target| nonempty_string(&target.state))
        .or_else(|| request_state.and_then(nonempty_string))
        .unwrap_or_else(|| STATE_UNSUPPORTED.to_string())
}

fn normalize_action_personality(
    requested: Option<&str>,
    config: &ContextualAssistConfig,
) -> String {
    requested
        .and_then(nonempty_string)
        .or_else(|| nonempty_string(&config.default_personality))
        .unwrap_or_else(crate::config::default_contextual_assist_personality)
}

fn contextual_assist_action_context(
    state: String,
    personality: String,
    target: Option<&NativeAssistTarget>,
) -> ContextualAssistActionContext {
    let context_text = target
        .and_then(|target| target.context_text.as_deref())
        .and_then(context_text_from_str);
    let has_context_text = context_text.is_some();

    ContextualAssistActionContext {
        state,
        personality,
        app: target
            .and_then(|target| target.app.as_deref())
            .and_then(nonempty_string),
        window_title: target
            .and_then(|target| target.window_title.as_deref())
            .and_then(nonempty_string),
        url: target
            .and_then(|target| target.url.as_deref())
            .and_then(nonempty_string),
        frame_url: target
            .and_then(|target| target.frame_url.as_deref())
            .and_then(nonempty_string),
        context_text,
        has_context_text,
    }
}

fn contextual_assist_action_routing(
    action: &ContextualAssistActionOption,
    context: &ContextualAssistActionContext,
) -> ContextualAssistActionRouting {
    let root_url = context.url.as_deref().and_then(root_url_from_target_url);
    let (source_kind, source_key) = source_key_for_context(context, root_url.as_deref());
    let session_key = format!("contextual-writing:{}", stable_key_component(&source_key));

    ContextualAssistActionRouting {
        agent_id: WRITING_ASSISTANT_AGENT_ID.to_string(),
        source_kind,
        source_key,
        session_key,
        root_url,
        target_text_kind: target_text_kind_for_state(&context.state).to_string(),
        action_intent: action.intent.clone(),
        personality: context.personality.clone(),
    }
}

fn contextual_assist_visual_context(
    action: &ContextualAssistActionOption,
    target: Option<&NativeAssistTarget>,
) -> ContextualAssistVisualContext {
    ContextualAssistVisualContext {
        screenshot: contextual_assist_screenshot_request(action, target),
    }
}

fn contextual_assist_screenshot_request(
    action: &ContextualAssistActionOption,
    target: Option<&NativeAssistTarget>,
) -> ContextualAssistScreenshotRequest {
    if !action.requires_screenshot {
        return ContextualAssistScreenshotRequest {
            policy: SCREENSHOT_POLICY_SKIPPED.to_string(),
            required: false,
            source: SCREENSHOT_SOURCE_NONE.to_string(),
            reason: if action.id == ACTION_SHORTEN {
                "make_concise_text_only".to_string()
            } else {
                "no_visual_context_needed".to_string()
            },
            degraded: false,
            window_rect: None,
            browser_tab_id: None,
            browser_window_id: None,
        };
    }

    let browser_tab_id = target.and_then(|target| target.browser_tab_id);
    let browser_window_id = target.and_then(|target| target.browser_window_id);
    let window_rect = target.and_then(|target| target.window_rect.clone());
    if browser_tab_id.is_some() {
        return ContextualAssistScreenshotRequest {
            policy: SCREENSHOT_POLICY_REQUIRED.to_string(),
            required: true,
            source: SCREENSHOT_SOURCE_CHROME_VISIBLE_TAB.to_string(),
            reason: "writing_action_needs_visible_tab_context".to_string(),
            degraded: false,
            window_rect,
            browser_tab_id,
            browser_window_id,
        };
    }

    if window_rect.is_some() {
        return ContextualAssistScreenshotRequest {
            policy: SCREENSHOT_POLICY_REQUIRED.to_string(),
            required: true,
            source: SCREENSHOT_SOURCE_ACTIVE_WINDOW_REGION.to_string(),
            reason: "writing_action_needs_focused_window_context".to_string(),
            degraded: false,
            window_rect,
            browser_tab_id: None,
            browser_window_id: None,
        };
    }

    ContextualAssistScreenshotRequest {
        policy: SCREENSHOT_POLICY_REQUIRED.to_string(),
        required: true,
        source: SCREENSHOT_SOURCE_FULL_SCREEN_FALLBACK.to_string(),
        reason: "window_bounds_unavailable".to_string(),
        degraded: true,
        window_rect: None,
        browser_tab_id: None,
        browser_window_id: None,
    }
}

fn source_key_for_context(
    context: &ContextualAssistActionContext,
    root_url: Option<&str>,
) -> (String, String) {
    if let Some(root_url) = root_url.and_then(nonempty_string) {
        return ("site".to_string(), format!("site:{root_url}"));
    }
    if let Some(app) = context.app.as_deref().and_then(nonempty_string) {
        return ("app".to_string(), format!("app:{app}"));
    }
    ("unknown".to_string(), "unknown".to_string())
}

fn root_url_from_target_url(value: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(value.trim()).ok()?;
    let host = parsed.host_str()?;
    let mut root = format!("{}://{}", parsed.scheme(), host);
    if let Some(port) = parsed.port() {
        root.push(':');
        root.push_str(&port.to_string());
    }
    Some(root)
}

fn target_text_kind_for_state(state: &str) -> &'static str {
    match state {
        STATE_SELECTION | STATE_SELECTION_FIELD => "selected_text",
        STATE_DRAFT => "field_text",
        STATE_EMPTY_CONTEXT | STATE_EMPTY_NO_CONTEXT => "screen_context",
        STATE_PAGE_CONTEXT => "page_url",
        STATE_FILES => "file_paths",
        _ => "none",
    }
}

fn stable_key_component(value: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed
    }
}

#[tauri::command]
pub async fn update_contextual_assist_webview_context(
    context: TauriWebviewContextPayload,
) -> Result<(), String> {
    let normalized = normalize_tauri_webview_context(context);
    let mut stored = tauri_webview_context_store()
        .lock()
        .map_err(|_| "contextual assist webview context lock poisoned".to_string())?;
    *stored = normalized.map(|payload| StoredTauriWebviewContext {
        payload,
        updated_at: Instant::now(),
    });
    Ok(())
}

fn normalize_tauri_webview_context(
    mut context: TauriWebviewContextPayload,
) -> Option<TauriWebviewContextPayload> {
    if context.secure || route_is_contextual_assist_surface(context.route.as_deref()) {
        return None;
    }

    context.selected_text = context
        .selected_text
        .as_deref()
        .and_then(context_text_from_str);
    context.field_text = context
        .field_text
        .as_deref()
        .and_then(context_text_from_str);
    context.route = context.route.as_deref().and_then(nonempty_string);
    context.url = context.url.as_deref().and_then(nonempty_string);
    context.title = context.title.as_deref().and_then(nonempty_string);
    context.has_selection = context.selected_text.is_some() || context.has_selection;

    if context.selected_text.is_none() && (!context.editable || context.field_text.is_none()) {
        return context.editable.then_some(context);
    }
    Some(context)
}

fn route_is_contextual_assist_surface(route: Option<&str>) -> bool {
    let Some(route) = route else {
        return false;
    };
    let route = route.trim().split(['?', '#']).next().unwrap_or_default();
    matches!(
        route,
        "/contextual-assist" | "/draw-overlay" | "/screen-region-picker"
    )
}

fn tauri_webview_context_store() -> &'static Mutex<Option<StoredTauriWebviewContext>> {
    TAURI_WEBVIEW_CONTEXT.get_or_init(|| Mutex::new(None))
}

fn tauri_webview_target_from_payload(
    payload: TauriWebviewContextPayload,
    context: BrowserProbeContext,
    config: &ContextualAssistConfig,
) -> Option<DetectedAssistTarget> {
    if app_is_excluded(context.app.as_deref(), config) {
        return None;
    }

    let selected_text = payload.selected_text.and_then(|text| {
        if config.show_on_selected_text {
            Some(text)
        } else {
            None
        }
    });
    let field_text = payload.field_text.and_then(|text| {
        if config.show_in_writable_fields {
            Some(text)
        } else {
            None
        }
    });
    let (state, context_text) = if let Some(selected_text) = selected_text {
        (
            if payload.editable {
                "selection-field"
            } else {
                "selection"
            },
            Some(selected_text),
        )
    } else if payload.editable && config.show_in_writable_fields {
        (
            if field_text.is_some() {
                "draft"
            } else {
                "empty-context"
            },
            field_text,
        )
    } else {
        return None;
    };

    Some(DetectedAssistTarget {
        native: NativeAssistTarget {
            state: state.to_string(),
            app: context.app,
            window_title: payload.title.or(payload.route).or(context.window_title),
            url: payload.url,
            context_text,
            source: Some("tauri_webview".to_string()),
            frame_url: None,
            browser_tab_id: None,
            browser_window_id: None,
            window_rect: context.window_rect,
        },
        anchor_x: context.anchor_x,
        anchor_y: context.anchor_y,
    })
}

fn nonempty_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn start_input_tracking(app: AppHandle) {
    #[cfg(target_os = "macos")]
    macos::start_input_tracking(app);
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

fn latest_detected_target() -> Option<DetectedAssistTarget> {
    latest_detected_target_store()
        .lock()
        .ok()
        .and_then(|stored| stored.clone())
}

fn store_latest_detected_target(target: Option<DetectedAssistTarget>) {
    if let Ok(mut stored) = latest_detected_target_store().lock() {
        *stored = target;
    }
}

fn latest_detected_target_store() -> &'static Mutex<Option<DetectedAssistTarget>> {
    LAST_DETECTED_TARGET.get_or_init(|| Mutex::new(None))
}

fn schedule_contextual_assist_hotkey(app: AppHandle) {
    if contextual_assist_hotkey_is_suppressed() {
        return;
    }
    let generation = HOTKEY_TAP_GENERATION
        .fetch_add(1, Ordering::SeqCst)
        .saturating_add(1);
    tauri::async_runtime::spawn(async move {
        sleep(Duration::from_millis(LEFT_OPTION_SINGLE_TAP_DELAY_MS)).await;
        if HOTKEY_TAP_GENERATION.load(Ordering::SeqCst) != generation
            || contextual_assist_hotkey_is_suppressed()
        {
            return;
        }
        open_contextual_assist_from_hotkey(app).await;
    });
}

fn cancel_scheduled_contextual_assist_hotkey() {
    HOTKEY_TAP_GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// Keep the single-tap Contextual Assist listener from consuming the second
/// release of the double-tap gesture that just summoned the HUD. The two
/// listeners use separate event taps, so either callback may observe that
/// release first; a short suppression deadline handles both callback orders.
pub(crate) fn suppress_contextual_assist_hotkey_for_overlay() {
    cancel_scheduled_contextual_assist_hotkey();
    let deadline = contextual_assist_hotkey_clock_ms()
        .saturating_add(LEFT_OPTION_SINGLE_TAP_DELAY_MS)
        .saturating_add(50);
    HOTKEY_TAP_SUPPRESSED_UNTIL_MS.fetch_max(deadline, Ordering::SeqCst);
}

fn contextual_assist_hotkey_is_suppressed() -> bool {
    contextual_assist_hotkey_clock_ms() < HOTKEY_TAP_SUPPRESSED_UNTIL_MS.load(Ordering::SeqCst)
}

fn contextual_assist_hotkey_clock_ms() -> u64 {
    HOTKEY_TAP_CLOCK_START
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

async fn open_contextual_assist_from_hotkey(app: AppHandle) {
    if overlay::contextual_assist_is_expanded() {
        return;
    }

    let desktop_config = app.state::<AppState>().config.lock().await.clone();
    let config = desktop_config.contextual_assist;
    if !config.enabled {
        return;
    }

    let browser_probe_ineligible =
        match probe_browser_contextual_target(&app, &config, desktop_config.network.magicutor_port)
            .await
        {
            BrowserProbeOutcome::Eligible(target) => {
                if let Err(error) = overlay::show_contextual_assist_menu_centered_at_point(
                    &app,
                    target.anchor_x,
                    target.anchor_y,
                    target.native,
                ) {
                    warn!("failed to show contextual assist menu from browser probe: {error}");
                }
                return;
            },
            BrowserProbeOutcome::Ineligible { secure_field: true } => return,
            // The extension already answered for this browser: do NOT fall into
            // the webview hotkey chain below — its pasteboard probe synthesizes
            // a Cmd+C keystroke, which would inject a spurious copy event into a
            // page the extension just said has no eligible target. The screen
            // fallback still opens a menu for the browser window itself.
            BrowserProbeOutcome::Ineligible {
                secure_field: false,
            } => true,
            BrowserProbeOutcome::NotExtensionBrowser => false,
        };

    let mut target = if browser_probe_ineligible {
        None
    } else {
        latest_tauri_webview_hotkey_target(&app, &config).or_else(|| {
            if frontmost_browser_or_webview_active(&app, &config) {
                return frontmost_webview_hotkey_target(&app, &config)
                    .or_else(|| frontmost_webview_empty_hotkey_target(&app, &config));
            }
            detect_contextual_target(&app, &config).or_else(latest_detected_target)
        })
    };
    // Finder's selection is file paths, not text — the AX text detector can
    // never match it, so it gets its own bounded AppleScript probe before
    // the generic screen fallback.
    if target.is_none() {
        target = finder_selection_target(&app, &config).await;
    }
    // No target anywhere must not dead-end the hotkey: fall back to a
    // screen-context target (frontmost app + window capture) so the menu
    // offers observe/write/task actions wherever the user is. Returns None
    // only for secure fields, excluded apps, or our own process.
    let target = target.or_else(|| screen_context_fallback_target(&app, &config));
    let Some(target) = target else {
        return;
    };

    if let Err(error) = overlay::show_contextual_assist_menu_centered_at_point(
        &app,
        target.anchor_x,
        target.anchor_y,
        target.native,
    ) {
        warn!("failed to show contextual assist menu from left option: {error}");
    }
}

fn app_is_excluded(app_name: Option<&str>, config: &ContextualAssistConfig) -> bool {
    let Some(app_name) = app_name else {
        return false;
    };
    config
        .excluded_apps
        .iter()
        .any(|excluded| excluded.eq_ignore_ascii_case(app_name))
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{
        app_is_excluded, cancel_scheduled_contextual_assist_hotkey, context_text_from_str,
        schedule_contextual_assist_hotkey, BrowserProbeContext, ContextualAssistCaptureRect,
        ContextualAssistConfig, DetectedAssistTarget, NativeAssistTarget,
    };
    use core_foundation::array::CFArray;
    use core_foundation::base::{CFRange, CFType, CFTypeRef, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::mach_port::CFMachPortRef;
    use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
    use core_foundation::string::{CFString, CFStringRef};
    use core_graphics::event::{
        CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions,
        CGEventTapPlacement, CGEventTapProxy, CGEventType, CallbackResult, EventField, KeyCode,
    };
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::geometry::{CGPoint, CGRect, CGSize};
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    use objc2_foundation::NSString;
    use std::collections::{HashSet, VecDeque};
    use std::ffi::c_void;
    use std::os::raw::{c_float, c_int};
    use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
    use std::sync::{mpsc, Mutex, Once, OnceLock};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    use tauri::AppHandle;
    use tracing::warn;

    type AXError = i32;
    type AXUIElementRef = CFTypeRef;
    type AXValueRef = CFTypeRef;
    type AXValueType = i32;

    const AX_SUCCESS: AXError = 0;
    const AX_VALUE_CG_POINT: AXValueType = 1;
    const AX_VALUE_CG_SIZE: AXValueType = 2;
    const AX_VALUE_CG_RECT: AXValueType = 3;
    const AX_VALUE_CF_RANGE: AXValueType = 4;
    const RECENT_SELECTION_GESTURE_MS: u64 = 2_500;
    const RECENT_TEXT_FOCUS_MS: u64 = 12_000;
    const MIN_SELECTION_DRAG_DISTANCE: f64 = 6.0;
    const LEFT_OPTION_TAP_MAX_MS: u64 = 650;
    const MAX_AX_DESCENDANT_DEPTH: usize = 6;
    const MAX_AX_DESCENDANT_NODES: usize = 180;
    const MAX_AX_CHILDREN_PER_ATTRIBUTE: isize = 60;
    const WEBVIEW_COPY_PROBE_SETTLE_MS: u64 = 120;
    const WEBVIEW_COPY_PROBE_MAIN_THREAD_TIMEOUT_MS: u64 = 2_000;

    static INPUT_TRACKER_ONCE: Once = Once::new();
    static RECENT_SELECTION_UNTIL_MS: AtomicU64 = AtomicU64::new(0);
    static RECENT_TEXT_FOCUS_UNTIL_MS: AtomicU64 = AtomicU64::new(0);
    static RECENT_MOUSE_X: AtomicI64 = AtomicI64::new(0);
    static RECENT_MOUSE_Y: AtomicI64 = AtomicI64::new(0);
    static DRAG_START: OnceLock<Mutex<Option<DragStart>>> = OnceLock::new();
    static MODIFIER_TRACKER: OnceLock<Mutex<ModifierTracker>> = OnceLock::new();

    #[derive(Debug, Clone, Copy)]
    struct DragStart {
        x: f64,
        y: f64,
        started_at: Instant,
    }

    #[derive(Debug, Default)]
    struct ModifierTracker {
        left_option_started_at: Option<Instant>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ModifierGestureResult {
        None,
        LeftOptionTapStarted,
        LeftOptionTapReleased,
    }

    impl ModifierTracker {
        fn track_flags_changed(
            &mut self,
            keycode: u16,
            left_option_keycode: u16,
            flags: CGEventFlags,
        ) -> ModifierGestureResult {
            let is_left_option = keycode == left_option_keycode;
            let alternate_down = flags.contains(CGEventFlags::CGEventFlagAlternate);
            let other_modifier_down = flags.contains(CGEventFlags::CGEventFlagControl)
                || flags.contains(CGEventFlags::CGEventFlagCommand)
                || flags.contains(CGEventFlags::CGEventFlagShift);

            if is_left_option {
                if alternate_down {
                    let clean_start = !other_modifier_down;
                    self.left_option_started_at = clean_start.then(Instant::now);
                    return ModifierGestureResult::LeftOptionTapStarted;
                }

                let started_at = self.left_option_started_at.take();
                let clean_release = !alternate_down && !other_modifier_down;
                let is_single_tap = clean_release
                    && started_at
                        .map(|started_at| {
                            started_at.elapsed() <= Duration::from_millis(LEFT_OPTION_TAP_MAX_MS)
                        })
                        .unwrap_or(false);
                return if is_single_tap {
                    ModifierGestureResult::LeftOptionTapReleased
                } else {
                    ModifierGestureResult::None
                };
            }

            if other_modifier_down {
                self.left_option_started_at = None;
            }

            ModifierGestureResult::None
        }

        fn cancel_left_option_tap(&mut self) {
            self.left_option_started_at = None;
        }

        fn reset(&mut self) {
            self.left_option_started_at = None;
        }
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
        fn AXIsProcessTrusted() -> u8;
        fn AXUIElementCreateSystemWide() -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> AXError;
        fn AXUIElementCopyParameterizedAttributeValue(
            element: AXUIElementRef,
            parameterized_attribute: CFStringRef,
            parameter: CFTypeRef,
            result: *mut CFTypeRef,
        ) -> AXError;
        fn AXUIElementCopyElementAtPosition(
            application: AXUIElementRef,
            x: c_float,
            y: c_float,
            element: *mut AXUIElementRef,
        ) -> AXError;
        fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut c_int) -> AXError;
        fn AXValueGetType(value: AXValueRef) -> AXValueType;
        fn AXValueGetValue(value: AXValueRef, value_type: AXValueType, value: *mut c_void) -> u8;
        fn AXValueCreate(value_type: AXValueType, value: *const c_void) -> AXValueRef;
    }

    pub(super) fn start_input_tracking(app: AppHandle) {
        INPUT_TRACKER_ONCE.call_once(|| {
            let _ = thread::Builder::new()
                .name("magician-contextual-assist-input".to_string())
                .spawn(move || {
                    let run_loop = CFRunLoop::get_current();
                    let callback_app = app.clone();
                    let tap_port = std::sync::Arc::new(AtomicUsize::new(0));
                    let callback_tap_port = std::sync::Arc::clone(&tap_port);
                    let event_tap = CGEventTap::new(
                        CGEventTapLocation::Session,
                        CGEventTapPlacement::HeadInsertEventTap,
                        CGEventTapOptions::Default,
                        vec![
                            CGEventType::FlagsChanged,
                            CGEventType::KeyDown,
                            CGEventType::LeftMouseDown,
                            CGEventType::LeftMouseDragged,
                            CGEventType::LeftMouseUp,
                        ],
                        move |_proxy: CGEventTapProxy,
                              event_type: CGEventType,
                              event: &CGEvent| {
                            if matches!(
                                event_type,
                                CGEventType::TapDisabledByTimeout
                                    | CGEventType::TapDisabledByUserInput
                            ) {
                                let port = callback_tap_port.load(Ordering::SeqCst);
                                if port != 0 {
                                    unsafe { CGEventTapEnable(port as CFMachPortRef, true) };
                                }
                                reset_modifier_tracker();
                                cancel_scheduled_contextual_assist_hotkey();
                                if let Ok(mut state) = drag_start_store().lock() {
                                    *state = None;
                                }
                                warn!("contextual assist input tap disabled by macOS; re-enabled");
                                return CallbackResult::Keep;
                            }

                            if matches!(event_type, CGEventType::KeyDown) {
                                cancel_scheduled_contextual_assist_hotkey();
                                cancel_left_option_tap();
                                return CallbackResult::Keep;
                            }

                            if matches!(event_type, CGEventType::FlagsChanged) {
                                match track_modifier_event(event) {
                                    ModifierGestureResult::LeftOptionTapStarted => {
                                        cancel_scheduled_contextual_assist_hotkey();
                                    },
                                    ModifierGestureResult::LeftOptionTapReleased => {
                                        schedule_contextual_assist_hotkey(callback_app.clone());
                                    },
                                    ModifierGestureResult::None => {},
                                }
                                return CallbackResult::Keep;
                            }

                            if matches!(event_type, CGEventType::LeftMouseDown) {
                                cancel_scheduled_contextual_assist_hotkey();
                                cancel_left_option_tap();
                                let point = event.location();
                                let (click_x, click_y) =
                                    ax_point_to_physical(&callback_app, point.x, point.y)
                                        .unwrap_or((point.x, point.y));
                                if crate::overlay::handle_contextual_assist_global_click(
                                    &callback_app,
                                    click_x,
                                    click_y,
                                ) {
                                    return CallbackResult::Drop;
                                }
                            }
                            track_mouse_event(event_type, event);
                            CallbackResult::Keep
                        },
                    );

                    let Ok(event_tap) = event_tap else {
                        warn!("contextual assist drag fallback unavailable; grant Input Monitoring if browser selections do not expose Accessibility selected text");
                        return;
                    };

                    tap_port.store(
                        event_tap.mach_port().as_concrete_TypeRef() as usize,
                        Ordering::SeqCst,
                    );

                    let Ok(loop_source) = event_tap.mach_port().create_runloop_source(0) else {
                        warn!("contextual assist drag fallback runloop source creation failed");
                        return;
                    };

                    run_loop.add_source(&loop_source, unsafe { kCFRunLoopCommonModes });
                    event_tap.enable();
                    CFRunLoop::run_current();
                });
        });
    }

    pub(super) fn detect_contextual_target(
        app: &AppHandle,
        config: &ContextualAssistConfig,
    ) -> Option<DetectedAssistTarget> {
        if unsafe { AXIsProcessTrusted() } == 0 {
            return None;
        }

        let system = unsafe { CFType::wrap_under_create_rule(AXUIElementCreateSystemWide()) };
        let focused_app = copy_attribute(system.as_CFTypeRef(), "AXFocusedApplication");
        let app_name = focused_app
            .as_ref()
            .and_then(|element| copy_string_attribute(element.as_CFTypeRef(), "AXTitle"));
        if app_is_excluded(app_name.as_deref(), config) {
            return None;
        }
        let focused_window = focused_app
            .as_ref()
            .and_then(|element| copy_attribute(element.as_CFTypeRef(), "AXFocusedWindow"));
        let window_title = focused_window
            .as_ref()
            .and_then(|window| copy_string_attribute(window.as_CFTypeRef(), "AXTitle"));
        let window_anchor = focused_window
            .as_ref()
            .and_then(|window| window_center_anchor(app, window.as_CFTypeRef()));
        let window_capture_rect = focused_window
            .as_ref()
            .and_then(|window| window_capture_rect(app, window.as_CFTypeRef()));

        let focused = copy_attribute(system.as_CFTypeRef(), "AXFocusedUIElement")?;
        let pointer_hit = focused_app
            .as_ref()
            .and_then(|element| recent_pointer_element(element.as_CFTypeRef()));
        let focused = pointer_hit
            .as_ref()
            .filter(|element| should_prefer_hit_element(element.as_CFTypeRef()))
            .unwrap_or(&focused);
        if let Some(target) = detect_element_target(
            app,
            config,
            focused.as_CFTypeRef(),
            app_name.as_deref(),
            window_title.as_deref(),
            window_anchor,
        ) {
            return Some(with_window_capture_rect(
                target,
                window_capture_rect.clone(),
            ));
        }

        if let Some(target) = detect_descendant_target(
            app,
            config,
            focused.as_CFTypeRef(),
            app_name.as_deref(),
            window_title.as_deref(),
            window_anchor,
        ) {
            return Some(with_window_capture_rect(
                target,
                window_capture_rect.clone(),
            ));
        }

        if let Some(target) = web_container_recent_target(
            app,
            config,
            focused.as_CFTypeRef(),
            app_name.as_deref(),
            window_title.as_deref(),
            window_anchor,
        ) {
            return Some(with_window_capture_rect(
                target,
                window_capture_rect.clone(),
            ));
        }

        browser_webview_recent_target(
            app,
            config,
            app_name.as_deref(),
            window_title.as_deref(),
            window_anchor,
        )
        .map(|target| with_window_capture_rect(target, window_capture_rect))
    }

    fn with_window_capture_rect(
        mut target: DetectedAssistTarget,
        window_rect: Option<ContextualAssistCaptureRect>,
    ) -> DetectedAssistTarget {
        if target.native.window_rect.is_none() {
            target.native.window_rect = window_rect;
        }
        target
    }

    /// Frontmost app name and focused-window title without any text-target
    /// requirements. Cheap (two attribute reads); used by screen-ask to
    /// stamp captures with the context they were taken in. Returns None
    /// without Accessibility trust, off-macOS, or when the frontmost app is
    /// this process — a capture taken with our own HUD or picker focused
    /// must not be labeled with our window.
    pub(super) fn frontmost_app_and_window_title() -> Option<(String, Option<String>)> {
        if unsafe { AXIsProcessTrusted() } == 0 {
            return None;
        }
        let system = unsafe { CFType::wrap_under_create_rule(AXUIElementCreateSystemWide()) };
        let focused_app = copy_attribute(system.as_CFTypeRef(), "AXFocusedApplication")?;
        if is_own_process(focused_app.as_CFTypeRef()) {
            return None;
        }
        let app_name = copy_string_attribute(focused_app.as_CFTypeRef(), "AXTitle")?;
        let window_title = copy_attribute(focused_app.as_CFTypeRef(), "AXFocusedWindow")
            .as_ref()
            .and_then(|window| copy_string_attribute(window.as_CFTypeRef(), "AXTitle"));
        Some((app_name, window_title))
    }

    /// Capture rect of the frontmost (non-self) window — see the outer
    /// `frontmost_window_capture_rect` wrapper.
    pub(super) fn frontmost_window_capture_rect(
        app: &AppHandle,
    ) -> Option<ContextualAssistCaptureRect> {
        if unsafe { AXIsProcessTrusted() } == 0 {
            return None;
        }
        let system = unsafe { CFType::wrap_under_create_rule(AXUIElementCreateSystemWide()) };
        let focused_app = copy_attribute(system.as_CFTypeRef(), "AXFocusedApplication")?;
        if is_own_process(focused_app.as_CFTypeRef()) {
            return None;
        }
        let focused_window = copy_attribute(focused_app.as_CFTypeRef(), "AXFocusedWindow")?;
        window_capture_rect(app, focused_window.as_CFTypeRef())
    }

    pub(super) fn ax_trusted() -> bool {
        unsafe { AXIsProcessTrusted() != 0 }
    }

    /// Finder file/folder selection via AppleScript — hotkey invoke only,
    /// never in the ambient 300 ms poll (each probe spawns an osascript
    /// process). Needs the Finder Automation TCC grant; on denial, timeout,
    /// or an empty selection this returns None and the hotkey falls through
    /// to the screen-context fallback (the Finder window still gets a menu).
    /// The desktop deliberately never reads file CONTENTS here: the model
    /// gets paths plus a window capture, and any deeper access happens
    /// through the agent's governed file tools under their own grants.
    pub(super) async fn finder_selection_target(
        app: &AppHandle,
        config: &ContextualAssistConfig,
    ) -> Option<DetectedAssistTarget> {
        // AX values (CFType) are not Send, so every read completes and is
        // converted to owned data BEFORE the osascript await — the future
        // must stay Send for tauri::async_runtime::spawn.
        let (app_name, window_title, anchor, window_rect) = {
            if unsafe { AXIsProcessTrusted() } == 0 {
                return None;
            }
            let system = unsafe { CFType::wrap_under_create_rule(AXUIElementCreateSystemWide()) };
            let focused_app = copy_attribute(system.as_CFTypeRef(), "AXFocusedApplication")?;
            if is_own_process(focused_app.as_CFTypeRef()) {
                return None;
            }
            let app_name = copy_string_attribute(focused_app.as_CFTypeRef(), "AXTitle");
            if !is_finder_app(app_name.as_deref()) || app_is_excluded(app_name.as_deref(), config) {
                return None;
            }

            let focused_window = copy_attribute(focused_app.as_CFTypeRef(), "AXFocusedWindow")?;
            let window_title = copy_string_attribute(focused_window.as_CFTypeRef(), "AXTitle");
            let anchor = window_center_anchor(app, focused_window.as_CFTypeRef())
                .or_else(|| recent_text_focus_anchor(app));
            let window_rect = window_capture_rect(app, focused_window.as_CFTypeRef());
            (app_name, window_title, anchor, window_rect)
        };

        let paths = finder_selection_paths().await?;
        let context_text = context_text_from_str(&format_selection_paths(&paths))?;
        let (anchor_x, anchor_y) = anchor?;

        Some(DetectedAssistTarget {
            native: NativeAssistTarget {
                state: super::STATE_FILES.to_string(),
                app: app_name,
                window_title,
                url: None,
                context_text: Some(context_text),
                source: Some("finder_selection_probe".to_string()),
                frame_url: None,
                browser_tab_id: None,
                browser_window_id: None,
                window_rect,
            },
            anchor_x,
            anchor_y,
        })
    }

    fn is_finder_app(app_name: Option<&str>) -> bool {
        app_name
            .map(|name| name.trim().eq_ignore_ascii_case("finder"))
            .unwrap_or(false)
    }

    /// Finder selection as one POSIX path per line — bounded so a hung
    /// Finder cannot stall the invoke. The explicit per-item loop (rather
    /// than `POSIX path of <alias list>`) is deliberate: applying `POSIX
    /// path` to a list is not a reliable coercion for multi-selection.
    async fn finder_selection_paths() -> Option<Vec<String>> {
        const FINDER_PROBE_TIMEOUT_MS: u64 = 600;
        const FINDER_SELECTION_SCRIPT: &str = "tell application \"Finder\"
	set sel to selection as alias list
	set out to \"\"
	repeat with f in sel
		set out to out & (POSIX path of f) & linefeed
	end repeat
	return out
end tell";
        let output = tokio::time::timeout(
            std::time::Duration::from_millis(FINDER_PROBE_TIMEOUT_MS),
            tokio::process::Command::new("/usr/bin/osascript")
                .arg("-e")
                .arg(FINDER_SELECTION_SCRIPT)
                .output(),
        )
        .await
        .ok()?
        .ok()?;
        if !output.status.success() {
            return None;
        }
        let paths = parse_finder_selection_paths(&String::from_utf8_lossy(&output.stdout));
        if paths.is_empty() {
            None
        } else {
            Some(paths)
        }
    }

    fn parse_finder_selection_paths(stdout: &str) -> Vec<String> {
        const MAX_FINDER_PATHS: usize = 50;
        const MAX_FINDER_PATH_CHARS: usize = 2048;
        stdout
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with('/'))
            .take(MAX_FINDER_PATHS)
            .map(|line| line.chars().take(MAX_FINDER_PATH_CHARS).collect::<String>())
            .collect()
    }

    fn format_selection_paths(paths: &[String]) -> String {
        paths
            .iter()
            .map(|path| format!("- {path}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Last-resort hotkey target: no text target was found anywhere, and the
    /// hotkey must not dead-end. Builds an `empty-no-context` target from the
    /// frontmost app/window so the menu offers observe/write/task actions
    /// grounded in a window capture. Returns None only when even that is
    /// wrong: our own process, an excluded app, or a focused secure field
    /// (a screenshot-backed menu must never visualize a password).
    pub(super) fn screen_context_fallback_target(
        app: &AppHandle,
        config: &ContextualAssistConfig,
    ) -> Option<DetectedAssistTarget> {
        if unsafe { AXIsProcessTrusted() } == 0 {
            return None;
        }

        let system = unsafe { CFType::wrap_under_create_rule(AXUIElementCreateSystemWide()) };
        let focused_app = copy_attribute(system.as_CFTypeRef(), "AXFocusedApplication")?;
        if is_own_process(focused_app.as_CFTypeRef()) {
            return None;
        }
        let app_name = copy_string_attribute(focused_app.as_CFTypeRef(), "AXTitle");
        if app_is_excluded(app_name.as_deref(), config) {
            return None;
        }

        if let Some(focused) = copy_attribute(system.as_CFTypeRef(), "AXFocusedUIElement") {
            let role = copy_string_attribute(focused.as_CFTypeRef(), "AXRole");
            let subrole = copy_string_attribute(focused.as_CFTypeRef(), "AXSubrole");
            if is_secure_role(
                role.as_deref().unwrap_or_default(),
                subrole.as_deref().unwrap_or_default(),
            ) {
                return None;
            }
        }

        let focused_window = copy_attribute(focused_app.as_CFTypeRef(), "AXFocusedWindow");
        let window_title = focused_window
            .as_ref()
            .and_then(|window| copy_string_attribute(window.as_CFTypeRef(), "AXTitle"));
        let window_anchor = focused_window
            .as_ref()
            .and_then(|window| window_center_anchor(app, window.as_CFTypeRef()));
        let window_rect = focused_window
            .as_ref()
            .and_then(|window| window_capture_rect(app, window.as_CFTypeRef()));
        let (anchor_x, anchor_y) = window_anchor.or_else(|| recent_text_focus_anchor(app))?;

        Some(DetectedAssistTarget {
            native: NativeAssistTarget {
                state: super::STATE_EMPTY_NO_CONTEXT.to_string(),
                app: app_name,
                window_title,
                url: None,
                context_text: None,
                source: Some("screen_context_fallback".to_string()),
                frame_url: None,
                browser_tab_id: None,
                browser_window_id: None,
                window_rect,
            },
            anchor_x,
            anchor_y,
        })
    }

    pub(super) fn frontmost_extension_browser_context(
        app: &AppHandle,
        config: &ContextualAssistConfig,
    ) -> Option<BrowserProbeContext> {
        if unsafe { AXIsProcessTrusted() } == 0 {
            return None;
        }

        let system = unsafe { CFType::wrap_under_create_rule(AXUIElementCreateSystemWide()) };
        let focused_app = copy_attribute(system.as_CFTypeRef(), "AXFocusedApplication")?;
        let app_name = copy_string_attribute(focused_app.as_CFTypeRef(), "AXTitle");
        if app_is_excluded(app_name.as_deref(), config)
            || !is_extension_browser_app(app_name.as_deref())
        {
            return None;
        }

        let focused_window = copy_attribute(focused_app.as_CFTypeRef(), "AXFocusedWindow");
        let window_title = focused_window
            .as_ref()
            .and_then(|window| copy_string_attribute(window.as_CFTypeRef(), "AXTitle"));
        let window_rect = focused_window
            .as_ref()
            .and_then(|window| window_capture_rect(app, window.as_CFTypeRef()));
        let (anchor_x, anchor_y) = focused_window
            .as_ref()
            .and_then(|window| window_center_anchor(app, window.as_CFTypeRef()))
            .or_else(|| recent_text_focus_anchor(app))
            .or_else(|| recent_selection_gesture_anchor(app))
            .or_else(|| monitor_center_anchor(app))?;

        Some(BrowserProbeContext {
            app: app_name,
            window_title,
            window_rect,
            anchor_x,
            anchor_y,
        })
    }

    pub(super) fn frontmost_webview_hotkey_target(
        app: &AppHandle,
        config: &ContextualAssistConfig,
    ) -> Option<DetectedAssistTarget> {
        if !config.show_on_selected_text {
            return None;
        }
        let context = frontmost_browser_or_webview_context(app, config)?;
        let context_text = copy_selected_text_via_pasteboard_probe(app);
        let fallback_anchor = if context_text.is_none() {
            recent_selection_gesture_anchor(app)
        } else {
            None
        };
        if context_text.is_none() && fallback_anchor.is_none() {
            return None;
        }
        Some(DetectedAssistTarget {
            native: NativeAssistTarget {
                state: "selection".to_string(),
                app: context.app,
                window_title: context.window_title,
                url: None,
                context_text,
                source: Some(
                    if fallback_anchor.is_some() {
                        "webview_selection_hotkey_fallback"
                    } else {
                        "webview_pasteboard_probe"
                    }
                    .to_string(),
                ),
                frame_url: None,
                browser_tab_id: None,
                browser_window_id: None,
                window_rect: context.window_rect,
            },
            anchor_x: fallback_anchor.map(|(x, _)| x).unwrap_or(context.anchor_x),
            anchor_y: fallback_anchor.map(|(_, y)| y).unwrap_or(context.anchor_y),
        })
    }

    pub(super) fn frontmost_webview_empty_hotkey_target(
        app: &AppHandle,
        config: &ContextualAssistConfig,
    ) -> Option<DetectedAssistTarget> {
        if !config.show_in_writable_fields || recent_text_focus_anchor(app).is_none() {
            return None;
        }
        let context = frontmost_browser_or_webview_context(app, config)?;
        Some(DetectedAssistTarget {
            native: NativeAssistTarget {
                state: "empty-context".to_string(),
                app: context.app,
                window_title: context.window_title,
                url: None,
                context_text: None,
                source: Some("webview_focus_probe".to_string()),
                frame_url: None,
                browser_tab_id: None,
                browser_window_id: None,
                window_rect: context.window_rect,
            },
            anchor_x: context.anchor_x,
            anchor_y: context.anchor_y,
        })
    }

    pub(super) fn frontmost_browser_or_webview_active(
        app: &AppHandle,
        config: &ContextualAssistConfig,
    ) -> bool {
        frontmost_browser_or_webview_context(app, config).is_some()
    }

    pub(super) fn frontmost_tauri_webview_context(
        app: &AppHandle,
        config: &ContextualAssistConfig,
    ) -> Option<BrowserProbeContext> {
        if unsafe { AXIsProcessTrusted() } == 0 {
            return None;
        }

        let system = unsafe { CFType::wrap_under_create_rule(AXUIElementCreateSystemWide()) };
        let focused_app = copy_attribute(system.as_CFTypeRef(), "AXFocusedApplication")?;
        if !is_own_process(focused_app.as_CFTypeRef()) {
            return None;
        }

        let app_name = copy_string_attribute(focused_app.as_CFTypeRef(), "AXTitle")
            .or_else(|| Some("Magican Desktop".to_string()));
        if app_is_excluded(app_name.as_deref(), config) {
            return None;
        }

        let focused_window = copy_attribute(focused_app.as_CFTypeRef(), "AXFocusedWindow");
        let window_title = focused_window
            .as_ref()
            .and_then(|window| copy_string_attribute(window.as_CFTypeRef(), "AXTitle"));
        let window_rect = focused_window
            .as_ref()
            .and_then(|window| window_capture_rect(app, window.as_CFTypeRef()));
        let (anchor_x, anchor_y) = focused_window
            .as_ref()
            .and_then(|window| window_center_anchor(app, window.as_CFTypeRef()))
            .or_else(|| monitor_center_anchor(app))?;

        Some(BrowserProbeContext {
            app: app_name,
            window_title,
            window_rect,
            anchor_x,
            anchor_y,
        })
    }

    fn frontmost_browser_or_webview_context(
        app: &AppHandle,
        config: &ContextualAssistConfig,
    ) -> Option<BrowserProbeContext> {
        if unsafe { AXIsProcessTrusted() } == 0 {
            return None;
        }

        let system = unsafe { CFType::wrap_under_create_rule(AXUIElementCreateSystemWide()) };
        let focused_app = copy_attribute(system.as_CFTypeRef(), "AXFocusedApplication")?;
        let app_name = copy_string_attribute(focused_app.as_CFTypeRef(), "AXTitle");
        if app_is_excluded(app_name.as_deref(), config)
            || !is_browser_or_webview_app(app_name.as_deref())
        {
            return None;
        }

        let focused_window = copy_attribute(focused_app.as_CFTypeRef(), "AXFocusedWindow");
        let window_title = focused_window
            .as_ref()
            .and_then(|window| copy_string_attribute(window.as_CFTypeRef(), "AXTitle"));
        let window_rect = focused_window
            .as_ref()
            .and_then(|window| window_capture_rect(app, window.as_CFTypeRef()));
        let (anchor_x, anchor_y) = recent_selection_gesture_anchor(app)
            .or_else(|| recent_text_focus_anchor(app))
            .or_else(|| {
                focused_window
                    .as_ref()
                    .and_then(|window| window_center_anchor(app, window.as_CFTypeRef()))
            })
            .or_else(|| monitor_center_anchor(app))?;

        Some(BrowserProbeContext {
            app: app_name,
            window_title,
            window_rect,
            anchor_x,
            anchor_y,
        })
    }

    fn detect_element_target(
        app: &AppHandle,
        config: &ContextualAssistConfig,
        element: AXUIElementRef,
        app_name: Option<&str>,
        window_title: Option<&str>,
        window_anchor: Option<(f64, f64)>,
    ) -> Option<DetectedAssistTarget> {
        if is_own_process(element) {
            return None;
        }

        let role = copy_string_attribute(element, "AXRole").unwrap_or_default();
        let subrole = copy_string_attribute(element, "AXSubrole").unwrap_or_default();
        if is_secure_role(&role, &subrole) {
            return None;
        }
        let terminal_like = is_terminal_app(app_name);
        let editable = copy_bool_attribute(element, "AXEditable").unwrap_or(false);
        let writable = editable || is_writable_role(&role, &subrole);
        let selectable = is_selectable_role(&role, &subrole);
        let frame = focused_rect(element);
        let selected_text = copy_string_attribute(element, "AXSelectedText").unwrap_or_default();
        let selected_text_range = copy_attribute(element, "AXSelectedTextRange");
        let selected_marker_range = copy_attribute(element, "AXSelectedTextMarkerRange");
        let has_text_selection_api =
            selected_text_range.is_some() || selected_marker_range.is_some();
        let selected_marker_text = selected_marker_range
            .as_ref()
            .and_then(|range| {
                copy_parameterized_string_attribute(
                    element,
                    "AXStringForTextMarkerRange",
                    range.as_CFTypeRef(),
                )
            })
            .unwrap_or_default();
        let selected_context_text = context_text_from_str(&selected_text)
            .or_else(|| context_text_from_str(&selected_marker_text));

        if config.show_on_selected_text && selected_context_text.is_some() {
            let fallback_anchor = selected_bounds(element)
                .or_else(|| {
                    selected_marker_range
                        .as_ref()
                        .and_then(|range| selected_marker_bounds(element, range.as_CFTypeRef()))
                })
                .or(frame)
                .map(|rect| anchor_from_rect(app, rect));
            let (anchor_x, anchor_y) = window_anchor.or(fallback_anchor)?;
            return Some(DetectedAssistTarget {
                native: NativeAssistTarget {
                    state: if writable {
                        "selection-field".to_string()
                    } else {
                        "selection".to_string()
                    },
                    app: app_name.map(ToOwned::to_owned),
                    window_title: window_title.map(ToOwned::to_owned),
                    url: None,
                    context_text: selected_context_text,
                    source: Some("accessibility".to_string()),
                    frame_url: None,
                    browser_tab_id: None,
                    browser_window_id: None,
                    window_rect: None,
                },
                anchor_x,
                anchor_y,
            });
        }

        if config.show_on_selected_text && selectable {
            if let Some(recent_anchor) = recent_selection_gesture_anchor(app) {
                let (anchor_x, anchor_y) = window_anchor.unwrap_or(recent_anchor);
                return Some(DetectedAssistTarget {
                    native: NativeAssistTarget {
                        state: if writable {
                            "selection-field".to_string()
                        } else {
                            "selection".to_string()
                        },
                        app: app_name.map(ToOwned::to_owned),
                        window_title: window_title.map(ToOwned::to_owned),
                        url: None,
                        context_text: None,
                        source: Some("accessibility".to_string()),
                        frame_url: None,
                        browser_tab_id: None,
                        browser_window_id: None,
                        window_rect: None,
                    },
                    anchor_x,
                    anchor_y,
                });
            }
        }

        if config.show_in_writable_fields && writable && !terminal_like {
            let value = copy_string_attribute(element, "AXValue").unwrap_or_default();
            let context_text = context_text_from_str(&value);
            let fallback_anchor = recent_text_focus_anchor(app)
                .or_else(|| frame.map(|rect| anchor_from_rect(app, rect)));
            let (anchor_x, anchor_y) = window_anchor.or(fallback_anchor)?;
            return Some(DetectedAssistTarget {
                native: NativeAssistTarget {
                    state: if context_text.is_none() {
                        "empty-context".to_string()
                    } else {
                        "draft".to_string()
                    },
                    app: app_name.map(ToOwned::to_owned),
                    window_title: window_title.map(ToOwned::to_owned),
                    url: None,
                    context_text,
                    source: Some("accessibility".to_string()),
                    frame_url: None,
                    browser_tab_id: None,
                    browser_window_id: None,
                    window_rect: None,
                },
                anchor_x,
                anchor_y,
            });
        }

        if config.show_in_writable_fields
            && !terminal_like
            && is_text_container_role(&role, &subrole)
            && !is_web_container_role(&role, &subrole)
            && has_text_selection_api
        {
            if let Some(recent_anchor) = recent_text_focus_anchor(app) {
                let (anchor_x, anchor_y) = window_anchor.unwrap_or(recent_anchor);
                return Some(DetectedAssistTarget {
                    native: NativeAssistTarget {
                        state: "empty-context".to_string(),
                        app: app_name.map(ToOwned::to_owned),
                        window_title: window_title.map(ToOwned::to_owned),
                        url: None,
                        context_text: None,
                        source: Some("accessibility".to_string()),
                        frame_url: None,
                        browser_tab_id: None,
                        browser_window_id: None,
                        window_rect: None,
                    },
                    anchor_x,
                    anchor_y,
                });
            }
        }

        None
    }

    fn detect_descendant_target(
        app: &AppHandle,
        config: &ContextualAssistConfig,
        root: AXUIElementRef,
        app_name: Option<&str>,
        window_title: Option<&str>,
        window_anchor: Option<(f64, f64)>,
    ) -> Option<DetectedAssistTarget> {
        let mut queue = VecDeque::new();
        let mut seen = HashSet::new();
        push_child_elements(root, 1, &mut seen, &mut queue);

        let mut visited = 0;
        while let Some((element, depth)) = queue.pop_front() {
            visited += 1;
            if visited > MAX_AX_DESCENDANT_NODES {
                break;
            }
            if let Some(target) = detect_element_target(
                app,
                config,
                element.as_CFTypeRef(),
                app_name,
                window_title,
                window_anchor,
            ) {
                return Some(target);
            }
            if depth < MAX_AX_DESCENDANT_DEPTH {
                push_child_elements(element.as_CFTypeRef(), depth + 1, &mut seen, &mut queue);
            }
        }

        None
    }

    fn push_child_elements(
        element: AXUIElementRef,
        depth: usize,
        seen: &mut HashSet<usize>,
        queue: &mut VecDeque<(CFType, usize)>,
    ) {
        for child in child_elements(element) {
            let key = child.as_CFTypeRef() as usize;
            if seen.insert(key) {
                queue.push_back((child, depth));
            }
        }
    }

    fn child_elements(element: AXUIElementRef) -> Vec<CFType> {
        let mut children = Vec::new();

        for attribute in ["AXFocusedUIElement"] {
            if let Some(child) = copy_attribute(element, attribute) {
                children.push(child);
            }
        }

        for attribute in [
            "AXChildren",
            "AXVisibleChildren",
            "AXContents",
            "AXRows",
            "AXColumns",
            "AXCells",
        ] {
            let Some(value) = copy_attribute(element, attribute) else {
                continue;
            };
            let Some(array) = value.downcast_into::<CFArray>() else {
                continue;
            };
            let child_count = array.len().min(MAX_AX_CHILDREN_PER_ATTRIBUTE);
            for child in array.get_values(CFRange {
                location: 0,
                length: child_count,
            }) {
                if child.is_null() {
                    continue;
                }
                children.push(unsafe { CFType::wrap_under_get_rule(child as CFTypeRef) });
            }
        }

        children
    }

    fn browser_webview_recent_target(
        app: &AppHandle,
        config: &ContextualAssistConfig,
        app_name: Option<&str>,
        window_title: Option<&str>,
        window_anchor: Option<(f64, f64)>,
    ) -> Option<DetectedAssistTarget> {
        if !is_browser_or_webview_app(app_name) {
            return None;
        }
        recent_ambient_target(app, config, app_name, window_title, window_anchor, false)
    }

    fn web_container_recent_target(
        app: &AppHandle,
        config: &ContextualAssistConfig,
        element: AXUIElementRef,
        app_name: Option<&str>,
        window_title: Option<&str>,
        window_anchor: Option<(f64, f64)>,
    ) -> Option<DetectedAssistTarget> {
        let role = copy_string_attribute(element, "AXRole").unwrap_or_default();
        let subrole = copy_string_attribute(element, "AXSubrole").unwrap_or_default();
        if !is_web_container_role(&role, &subrole) {
            return None;
        }
        recent_ambient_target(app, config, app_name, window_title, window_anchor, false)
    }

    fn recent_ambient_target(
        app: &AppHandle,
        config: &ContextualAssistConfig,
        app_name: Option<&str>,
        window_title: Option<&str>,
        window_anchor: Option<(f64, f64)>,
        allow_recent_text_focus: bool,
    ) -> Option<DetectedAssistTarget> {
        if config.show_on_selected_text {
            if let Some(recent_anchor) = recent_selection_gesture_anchor(app) {
                let (anchor_x, anchor_y) = window_anchor.unwrap_or(recent_anchor);
                return Some(DetectedAssistTarget {
                    native: NativeAssistTarget {
                        state: "selection".to_string(),
                        app: app_name.map(ToOwned::to_owned),
                        window_title: window_title.map(ToOwned::to_owned),
                        url: None,
                        context_text: None,
                        source: Some("ambient_selection_probe".to_string()),
                        frame_url: None,
                        browser_tab_id: None,
                        browser_window_id: None,
                        window_rect: None,
                    },
                    anchor_x,
                    anchor_y,
                });
            }
        }

        if allow_recent_text_focus && config.show_in_writable_fields {
            if let Some(recent_anchor) = recent_text_focus_anchor(app) {
                let (anchor_x, anchor_y) = window_anchor.unwrap_or(recent_anchor);
                return Some(DetectedAssistTarget {
                    native: NativeAssistTarget {
                        state: "empty-context".to_string(),
                        app: app_name.map(ToOwned::to_owned),
                        window_title: window_title.map(ToOwned::to_owned),
                        url: None,
                        context_text: None,
                        source: Some("ambient_focus_probe".to_string()),
                        frame_url: None,
                        browser_tab_id: None,
                        browser_window_id: None,
                        window_rect: None,
                    },
                    anchor_x,
                    anchor_y,
                });
            }
        }

        None
    }

    fn is_browser_or_webview_app(app_name: Option<&str>) -> bool {
        let Some(app_name) = app_name else {
            return false;
        };
        let normalized = app_name.trim().to_ascii_lowercase();
        [
            "arc",
            "brave",
            "chrome",
            "chromium",
            "edge",
            "electron",
            "figma",
            "firefox",
            "github desktop",
            "notion",
            "opera",
            "safari",
            "slack",
            "visual studio code",
            "vivaldi",
        ]
        .iter()
        .any(|needle| normalized.contains(needle))
    }

    fn is_extension_browser_app(app_name: Option<&str>) -> bool {
        let Some(app_name) = app_name else {
            return false;
        };
        let normalized = app_name.trim().to_ascii_lowercase();
        [
            "arc",
            "brave",
            "chrome",
            "chromium",
            "google chrome",
            "microsoft edge",
            "opera",
            "vivaldi",
        ]
        .iter()
        .any(|needle| normalized.contains(needle))
    }

    fn is_terminal_app(app_name: Option<&str>) -> bool {
        let Some(app_name) = app_name else {
            return false;
        };
        let normalized = app_name.trim().to_ascii_lowercase();
        [
            "terminal",
            "iterm",
            "warp",
            "ghostty",
            "wezterm",
            "kitty",
            "alacritty",
            "rio",
            "tabby",
            "hyper",
        ]
        .iter()
        .any(|needle| normalized.contains(needle))
    }

    fn is_own_process(element: AXUIElementRef) -> bool {
        let mut pid: c_int = 0;
        let result = unsafe { AXUIElementGetPid(element, &mut pid) };
        result == AX_SUCCESS && pid as u32 == std::process::id()
    }

    fn copy_attribute(element: AXUIElementRef, attribute: &str) -> Option<CFType> {
        let attribute = CFString::new(attribute);
        let mut value: CFTypeRef = std::ptr::null();
        let result = unsafe {
            AXUIElementCopyAttributeValue(element, attribute.as_concrete_TypeRef(), &mut value)
        };
        if result == AX_SUCCESS && !value.is_null() {
            Some(unsafe { CFType::wrap_under_create_rule(value) })
        } else {
            None
        }
    }

    fn copy_parameterized_attribute(
        element: AXUIElementRef,
        attribute: &str,
        parameter: CFTypeRef,
    ) -> Option<CFType> {
        let attribute = CFString::new(attribute);
        let mut value: CFTypeRef = std::ptr::null();
        let result = unsafe {
            AXUIElementCopyParameterizedAttributeValue(
                element,
                attribute.as_concrete_TypeRef(),
                parameter,
                &mut value,
            )
        };
        if result == AX_SUCCESS && !value.is_null() {
            Some(unsafe { CFType::wrap_under_create_rule(value) })
        } else {
            None
        }
    }

    fn recent_pointer_element(application: AXUIElementRef) -> Option<CFType> {
        if !recent_pointer_active() {
            return None;
        }
        let x = RECENT_MOUSE_X.load(Ordering::SeqCst) as c_float;
        let y = RECENT_MOUSE_Y.load(Ordering::SeqCst) as c_float;
        if x == 0.0 && y == 0.0 {
            return None;
        }
        let mut element: AXUIElementRef = std::ptr::null();
        let result = unsafe { AXUIElementCopyElementAtPosition(application, x, y, &mut element) };
        if result == AX_SUCCESS && !element.is_null() {
            Some(unsafe { CFType::wrap_under_create_rule(element) })
        } else {
            None
        }
    }

    fn should_prefer_hit_element(element: AXUIElementRef) -> bool {
        let role = copy_string_attribute(element, "AXRole").unwrap_or_default();
        let subrole = copy_string_attribute(element, "AXSubrole").unwrap_or_default();
        if is_secure_role(&role, &subrole) {
            return true;
        }
        let editable = copy_bool_attribute(element, "AXEditable").unwrap_or(false);
        let selected_text = copy_string_attribute(element, "AXSelectedText").unwrap_or_default();
        let has_text_selection_api = copy_attribute(element, "AXSelectedTextRange").is_some()
            || copy_attribute(element, "AXSelectedTextMarkerRange").is_some();
        editable
            || is_writable_role(&role, &subrole)
            || !selected_text.trim().is_empty()
            || (is_text_container_role(&role, &subrole) && has_text_selection_api)
    }

    fn copy_string_attribute(element: AXUIElementRef, attribute: &str) -> Option<String> {
        copy_attribute(element, attribute)
            .and_then(|value| value.downcast_into::<CFString>())
            .map(|value| value.to_string())
    }

    fn copy_bool_attribute(element: AXUIElementRef, attribute: &str) -> Option<bool> {
        copy_attribute(element, attribute)
            .and_then(|value| value.downcast_into::<CFBoolean>())
            .map(bool::from)
    }

    fn copy_parameterized_string_attribute(
        element: AXUIElementRef,
        attribute: &str,
        parameter: CFTypeRef,
    ) -> Option<String> {
        copy_parameterized_attribute(element, attribute, parameter)
            .and_then(|value| value.downcast_into::<CFString>())
            .map(|value| value.to_string())
    }

    fn focused_rect(element: AXUIElementRef) -> Option<CGRect> {
        copy_ax_rect_attribute(element, "AXFrame").or_else(|| {
            let origin = copy_ax_point_attribute(element, "AXPosition")?;
            let size = copy_ax_size_attribute(element, "AXSize")?;
            Some(CGRect::new(&origin, &size))
        })
    }

    fn selected_bounds(element: AXUIElementRef) -> Option<CGRect> {
        let range_value = copy_attribute(element, "AXSelectedTextRange")?;
        let range = ax_value_cf_range(&range_value)?;
        if range.length <= 0 {
            return None;
        }
        let range_value_ref =
            unsafe { AXValueCreate(AX_VALUE_CF_RANGE, &range as *const CFRange as *const c_void) };
        if range_value_ref.is_null() {
            return None;
        }
        let range_parameter = unsafe { CFType::wrap_under_create_rule(range_value_ref) };
        let bounds = copy_parameterized_attribute(
            element,
            "AXBoundsForRange",
            range_parameter.as_CFTypeRef(),
        )?;
        ax_value_rect(&bounds)
    }

    fn selected_marker_bounds(element: AXUIElementRef, marker_range: CFTypeRef) -> Option<CGRect> {
        let bounds =
            copy_parameterized_attribute(element, "AXBoundsForTextMarkerRange", marker_range)?;
        ax_value_rect(&bounds)
    }

    fn copy_ax_rect_attribute(element: AXUIElementRef, attribute: &str) -> Option<CGRect> {
        copy_attribute(element, attribute).and_then(|value| ax_value_rect(&value))
    }

    fn copy_ax_point_attribute(element: AXUIElementRef, attribute: &str) -> Option<CGPoint> {
        copy_attribute(element, attribute).and_then(|value| ax_value_point(&value))
    }

    fn copy_ax_size_attribute(element: AXUIElementRef, attribute: &str) -> Option<CGSize> {
        copy_attribute(element, attribute).and_then(|value| ax_value_size(&value))
    }

    fn ax_value_rect(value: &CFType) -> Option<CGRect> {
        let value_ref = value.as_CFTypeRef() as AXValueRef;
        if unsafe { AXValueGetType(value_ref) } != AX_VALUE_CG_RECT {
            return None;
        }
        let mut rect = CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(0.0, 0.0));
        let ok = unsafe {
            AXValueGetValue(
                value_ref,
                AX_VALUE_CG_RECT,
                &mut rect as *mut CGRect as *mut c_void,
            )
        };
        if ok != 0 && rect.size.width > 0.0 && rect.size.height > 0.0 {
            Some(rect)
        } else {
            None
        }
    }

    fn ax_value_point(value: &CFType) -> Option<CGPoint> {
        let value_ref = value.as_CFTypeRef() as AXValueRef;
        if unsafe { AXValueGetType(value_ref) } != AX_VALUE_CG_POINT {
            return None;
        }
        let mut point = CGPoint::new(0.0, 0.0);
        let ok = unsafe {
            AXValueGetValue(
                value_ref,
                AX_VALUE_CG_POINT,
                &mut point as *mut CGPoint as *mut c_void,
            )
        };
        (ok != 0).then_some(point)
    }

    fn ax_value_size(value: &CFType) -> Option<CGSize> {
        let value_ref = value.as_CFTypeRef() as AXValueRef;
        if unsafe { AXValueGetType(value_ref) } != AX_VALUE_CG_SIZE {
            return None;
        }
        let mut size = CGSize::new(0.0, 0.0);
        let ok = unsafe {
            AXValueGetValue(
                value_ref,
                AX_VALUE_CG_SIZE,
                &mut size as *mut CGSize as *mut c_void,
            )
        };
        if ok != 0 && size.width > 0.0 && size.height > 0.0 {
            Some(size)
        } else {
            None
        }
    }

    fn ax_value_cf_range(value: &CFType) -> Option<CFRange> {
        let value_ref = value.as_CFTypeRef() as AXValueRef;
        if unsafe { AXValueGetType(value_ref) } != AX_VALUE_CF_RANGE {
            return None;
        }
        let mut range = CFRange {
            location: 0,
            length: 0,
        };
        let ok = unsafe {
            AXValueGetValue(
                value_ref,
                AX_VALUE_CF_RANGE,
                &mut range as *mut CFRange as *mut c_void,
            )
        };
        (ok != 0).then_some(range)
    }

    fn is_writable_role(role: &str, subrole: &str) -> bool {
        matches!(
            role,
            "AXTextField" | "AXTextArea" | "AXComboBox" | "AXTextView"
        ) || matches!(subrole, "AXSearchField")
    }

    fn is_selectable_role(role: &str, subrole: &str) -> bool {
        is_writable_role(role, subrole)
            || matches!(
                role,
                "AXWebArea"
                    | "AXTextArea"
                    | "AXTextField"
                    | "AXTextView"
                    | "AXStaticText"
                    | "AXGroup"
                    | "AXScrollArea"
                    | "AXLayoutArea"
                    | "AXLayoutItem"
            )
            || matches!(subrole, "AXDocument" | "AXTextAttachment")
    }

    fn is_text_container_role(role: &str, subrole: &str) -> bool {
        is_selectable_role(role, subrole)
            || matches!(role, "AXWebArea" | "AXGroup" | "AXScrollArea")
            || matches!(subrole, "AXDocument")
    }

    fn is_web_container_role(role: &str, subrole: &str) -> bool {
        matches!(role, "AXWebArea") || matches!(subrole, "AXDocument")
    }

    fn is_secure_role(role: &str, subrole: &str) -> bool {
        matches!(role, "AXSecureTextField") || matches!(subrole, "AXSecureTextField")
    }

    struct PasteboardSnapshot {
        text: Option<String>,
    }

    impl PasteboardSnapshot {
        fn capture(pasteboard: &NSPasteboard) -> Self {
            Self {
                text: pasteboard_text(pasteboard),
            }
        }

        fn restore(&self, pasteboard: &NSPasteboard) {
            match self.text.as_deref() {
                Some(text) => {
                    let _ = set_pasteboard_text(pasteboard, text);
                },
                None => {
                    pasteboard.clearContents();
                },
            };
        }
    }

    fn copy_selected_text_via_pasteboard_probe(app: &AppHandle) -> Option<String> {
        if MainThreadMarker::new().is_some() {
            return copy_selected_text_via_pasteboard_probe_on_main();
        }

        let (tx, rx) = mpsc::channel();
        let app = app.clone();
        if let Err(error) = app.run_on_main_thread(move || {
            let _ = tx.send(copy_selected_text_via_pasteboard_probe_on_main());
        }) {
            warn!("failed to schedule contextual assist pasteboard probe on main thread: {error}");
            return None;
        }
        match rx.recv_timeout(Duration::from_millis(
            WEBVIEW_COPY_PROBE_MAIN_THREAD_TIMEOUT_MS,
        )) {
            Ok(result) => result,
            Err(error) => {
                warn!("timed out waiting for contextual assist pasteboard probe: {error}");
                None
            },
        }
    }

    fn copy_selected_text_via_pasteboard_probe_on_main() -> Option<String> {
        let pasteboard = NSPasteboard::generalPasteboard();
        let snapshot = PasteboardSnapshot::capture(&pasteboard);
        let sentinel = format!("__magician_contextual_assist_copy_probe_{}__", now_millis());
        set_pasteboard_text(&pasteboard, &sentinel)?;

        let copied_text = if post_command_c().is_ok() {
            thread::sleep(Duration::from_millis(WEBVIEW_COPY_PROBE_SETTLE_MS));
            pasteboard_text(&pasteboard)
        } else {
            None
        };
        snapshot.restore(&pasteboard);

        let copied_text = context_text_from_str(copied_text.as_deref()?)?;
        (copied_text != sentinel).then_some(copied_text)
    }

    fn pasteboard_text(pasteboard: &NSPasteboard) -> Option<String> {
        pasteboard
            .stringForType(unsafe { NSPasteboardTypeString })
            .map(|value| value.to_string())
    }

    fn set_pasteboard_text(pasteboard: &NSPasteboard, text: &str) -> Option<()> {
        pasteboard.clearContents();
        let text = NSString::from_str(text);
        pasteboard
            .setString_forType(&text, unsafe { NSPasteboardTypeString })
            .then_some(())
    }

    fn post_command_c() -> Result<(), ()> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)?;
        let key_down = CGEvent::new_keyboard_event(source.clone(), KeyCode::ANSI_C, true)?;
        let key_up = CGEvent::new_keyboard_event(source, KeyCode::ANSI_C, false)?;
        let flags = CGEventFlags::CGEventFlagCommand;
        key_down.set_flags(flags);
        key_up.set_flags(flags);
        key_down.post(CGEventTapLocation::HID);
        key_up.post(CGEventTapLocation::HID);
        Ok(())
    }

    fn post_command_v() -> Result<(), ()> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)?;
        let key_down = CGEvent::new_keyboard_event(source.clone(), KeyCode::ANSI_V, true)?;
        let key_up = CGEvent::new_keyboard_event(source, KeyCode::ANSI_V, false)?;
        let flags = CGEventFlags::CGEventFlagCommand;
        key_down.set_flags(flags);
        key_up.set_flags(flags);
        key_down.post(CGEventTapLocation::HID);
        key_up.post(CGEventTapLocation::HID);
        Ok(())
    }

    /// Stage `text` on the pasteboard and synthesize Cmd+V for the currently
    /// focused app. The caller owns pasteboard restoration: the snapshot is
    /// returned so the previous clipboard can be put back after the target
    /// app has consumed the paste. Runs on the main thread (pasteboard +
    /// event posting follow the existing probe's discipline).
    fn paste_text_into_focused_app_on_main(text: &str) -> Option<PasteboardSnapshot> {
        let pasteboard = NSPasteboard::generalPasteboard();
        let snapshot = PasteboardSnapshot::capture(&pasteboard);
        if set_pasteboard_text(&pasteboard, text).is_none() {
            return None;
        }
        if post_command_v().is_err() {
            snapshot.restore(&pasteboard);
            return None;
        }
        Some(snapshot)
    }

    fn run_on_main_blocking<F>(app: &AppHandle, timeout_ms: u64, work: F)
    where
        F: FnOnce() + Send + 'static,
    {
        if MainThreadMarker::new().is_some() {
            work();
            return;
        }
        let (tx, rx) = mpsc::channel();
        if app
            .run_on_main_thread(move || {
                work();
                let _ = tx.send(());
            })
            .is_err()
        {
            return;
        }
        let _ = rx.recv_timeout(Duration::from_millis(timeout_ms));
    }

    /// Stage the insertion paste on the main thread; the returned token
    /// carries the user's previous clipboard for a later restore. Failure
    /// here leaves the pasteboard untouched (the on-main helper restores on
    /// its own paste-post failure).
    pub(super) fn stage_paste_for_focused_app(
        app: &AppHandle,
        text: &str,
    ) -> Option<super::PasteboardRestoreToken> {
        if MainThreadMarker::new().is_some() {
            return paste_text_into_focused_app_on_main(text)
                .map(|snapshot| super::PasteboardRestoreToken(snapshot.text));
        }
        let (tx, rx) = mpsc::channel();
        let text = text.to_string();
        if app
            .run_on_main_thread(move || {
                let _ = tx.send(
                    paste_text_into_focused_app_on_main(&text)
                        .map(|snapshot| super::PasteboardRestoreToken(snapshot.text)),
                );
            })
            .is_err()
        {
            return None;
        }
        match rx.recv_timeout(Duration::from_millis(super::INSERT_MAIN_THREAD_TIMEOUT_MS)) {
            Ok(token) => token,
            Err(_) => None,
        }
    }

    pub(super) fn restore_pasteboard_after_insert(
        app: &AppHandle,
        token: &super::PasteboardRestoreToken,
    ) {
        let snapshot = PasteboardSnapshot {
            text: token.0.clone(),
        };
        run_on_main_blocking(app, super::INSERT_MAIN_THREAD_TIMEOUT_MS, move || {
            let pasteboard = NSPasteboard::generalPasteboard();
            snapshot.restore(&pasteboard);
        });
    }

    fn track_modifier_event(event: &CGEvent) -> ModifierGestureResult {
        let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
        let mut tracker = match modifier_tracker_store().lock() {
            Ok(tracker) => tracker,
            Err(_) => return ModifierGestureResult::None,
        };
        tracker.track_flags_changed(keycode, KeyCode::OPTION as u16, event.get_flags())
    }

    fn cancel_left_option_tap() {
        if let Ok(mut tracker) = modifier_tracker_store().lock() {
            tracker.cancel_left_option_tap();
        }
    }

    fn reset_modifier_tracker() {
        if let Ok(mut tracker) = modifier_tracker_store().lock() {
            tracker.reset();
        }
    }

    fn modifier_tracker_store() -> &'static Mutex<ModifierTracker> {
        MODIFIER_TRACKER.get_or_init(|| Mutex::new(ModifierTracker::default()))
    }

    fn track_mouse_event(event_type: CGEventType, event: &CGEvent) {
        let point = event.location();
        match event_type {
            CGEventType::LeftMouseDown => {
                if let Ok(mut state) = drag_start_store().lock() {
                    *state = Some(DragStart {
                        x: point.x,
                        y: point.y,
                        started_at: Instant::now(),
                    });
                }
            },
            CGEventType::LeftMouseUp => {
                let start = drag_start_store()
                    .lock()
                    .ok()
                    .and_then(|mut state| state.take());
                let Some(start) = start else {
                    return;
                };
                let dx = point.x - start.x;
                let dy = point.y - start.y;
                let dragged = (dx * dx + dy * dy).sqrt() >= MIN_SELECTION_DRAG_DISTANCE;
                let elapsed = start.started_at.elapsed();
                if !dragged && elapsed <= Duration::from_secs(2) {
                    RECENT_MOUSE_X.store(point.x.round() as i64, Ordering::SeqCst);
                    RECENT_MOUSE_Y.store(point.y.round() as i64, Ordering::SeqCst);
                    RECENT_TEXT_FOCUS_UNTIL_MS.store(
                        now_millis().saturating_add(RECENT_TEXT_FOCUS_MS),
                        Ordering::SeqCst,
                    );
                } else if dragged
                    && elapsed >= Duration::from_millis(60)
                    && elapsed <= Duration::from_secs(8)
                {
                    RECENT_MOUSE_X.store(point.x.round() as i64, Ordering::SeqCst);
                    RECENT_MOUSE_Y.store(point.y.round() as i64, Ordering::SeqCst);
                    RECENT_SELECTION_UNTIL_MS.store(
                        now_millis().saturating_add(RECENT_SELECTION_GESTURE_MS),
                        Ordering::SeqCst,
                    );
                }
            },
            _ => {},
        }
    }

    fn drag_start_store() -> &'static Mutex<Option<DragStart>> {
        DRAG_START.get_or_init(|| Mutex::new(None))
    }

    fn recent_selection_gesture_anchor(app: &AppHandle) -> Option<(f64, f64)> {
        if RECENT_SELECTION_UNTIL_MS.load(Ordering::SeqCst) <= now_millis() {
            return None;
        }
        let x = RECENT_MOUSE_X.load(Ordering::SeqCst) as f64;
        let y = RECENT_MOUSE_Y.load(Ordering::SeqCst) as f64;
        bounded_ax_anchor(app, x, y)
    }

    fn recent_text_focus_anchor(app: &AppHandle) -> Option<(f64, f64)> {
        if RECENT_TEXT_FOCUS_UNTIL_MS.load(Ordering::SeqCst) <= now_millis() {
            return None;
        }
        let x = RECENT_MOUSE_X.load(Ordering::SeqCst) as f64;
        let y = RECENT_MOUSE_Y.load(Ordering::SeqCst) as f64;
        bounded_ax_anchor(app, x, y)
    }

    fn recent_pointer_active() -> bool {
        let now = now_millis();
        RECENT_TEXT_FOCUS_UNTIL_MS.load(Ordering::SeqCst) > now
            || RECENT_SELECTION_UNTIL_MS.load(Ordering::SeqCst) > now
    }

    fn now_millis() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0)
    }

    fn anchor_from_rect(app: &AppHandle, rect: CGRect) -> (f64, f64) {
        let x = rect.origin.x + rect.size.width + 8.0;
        let y = rect.origin.y + (rect.size.height / 2.0) - 13.0;
        bounded_ax_anchor(app, x, y).unwrap_or((x, y))
    }

    fn window_center_anchor(app: &AppHandle, window: AXUIElementRef) -> Option<(f64, f64)> {
        let rect = focused_rect(window)?;
        let x = rect.origin.x + (rect.size.width / 2.0);
        let y = rect.origin.y + (rect.size.height / 2.0);
        bounded_ax_anchor(app, x, y)
    }

    fn window_capture_rect(
        app: &AppHandle,
        window: AXUIElementRef,
    ) -> Option<ContextualAssistCaptureRect> {
        let rect = focused_rect(window)?;
        capture_rect_from_ax_rect(app, rect)
    }

    fn capture_rect_from_ax_rect(
        app: &AppHandle,
        rect: CGRect,
    ) -> Option<ContextualAssistCaptureRect> {
        if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
            return None;
        }
        let (x1, y1) = ax_point_to_physical(app, rect.origin.x, rect.origin.y)
            .unwrap_or((rect.origin.x, rect.origin.y));
        let (x2, y2) = ax_point_to_physical(
            app,
            rect.origin.x + rect.size.width,
            rect.origin.y + rect.size.height,
        )
        .unwrap_or((
            rect.origin.x + rect.size.width,
            rect.origin.y + rect.size.height,
        ));
        let min_x = x1.min(x2).round();
        let min_y = y1.min(y2).round();
        let width = (x1.max(x2) - x1.min(x2)).round();
        let height = (y1.max(y2) - y1.min(y2)).round();
        if width < 1.0 || height < 1.0 {
            return None;
        }
        Some(ContextualAssistCaptureRect {
            x: min_x as i32,
            y: min_y as i32,
            width: width as u32,
            height: height as u32,
        })
    }

    fn monitor_center_anchor(app: &AppHandle) -> Option<(f64, f64)> {
        let monitors = app.available_monitors().ok()?;
        let monitor = monitors.first()?;
        Some(monitor_center(monitor))
    }

    fn bounded_ax_anchor(app: &AppHandle, x: f64, y: f64) -> Option<(f64, f64)> {
        if let Some(anchor) = ax_point_to_physical(app, x, y) {
            return Some(anchor);
        }
        let fallback = nearest_monitor_center_anchor(app, x, y);
        if let Some((fallback_x, fallback_y)) = fallback {
            warn!(
                "could not map Accessibility anchor ({x:.1}, {y:.1}) to a known monitor; using nearest monitor center ({fallback_x:.1}, {fallback_y:.1})"
            );
        } else {
            warn!(
                "could not map Accessibility anchor ({x:.1}, {y:.1}) to a known monitor and no monitor fallback was available"
            );
        }
        fallback
    }

    fn nearest_monitor_center_anchor(app: &AppHandle, x: f64, y: f64) -> Option<(f64, f64)> {
        let monitors = app.available_monitors().ok()?;
        monitors
            .iter()
            .map(|monitor| {
                let position = monitor.position();
                let size = monitor.size();
                let min_x = position.x as f64;
                let min_y = position.y as f64;
                let max_x = min_x + size.width as f64;
                let max_y = min_y + size.height as f64;
                let dx = if x < min_x {
                    min_x - x
                } else if x > max_x {
                    x - max_x
                } else {
                    0.0
                };
                let dy = if y < min_y {
                    min_y - y
                } else if y > max_y {
                    y - max_y
                } else {
                    0.0
                };
                ((dx * dx) + (dy * dy), monitor_center(monitor))
            })
            .min_by(|left, right| left.0.total_cmp(&right.0))
            .map(|(_, center)| center)
    }

    fn monitor_center(monitor: &tauri::Monitor) -> (f64, f64) {
        let position = monitor.position();
        let size = monitor.size();
        (
            position.x as f64 + (size.width as f64 / 2.0),
            position.y as f64 + (size.height as f64 / 2.0),
        )
    }

    fn ax_point_to_physical(app: &AppHandle, x: f64, y: f64) -> Option<(f64, f64)> {
        let monitors = app.available_monitors().ok()?;
        for monitor in &monitors {
            let scale = monitor.scale_factor();
            if scale <= 0.0 {
                continue;
            }
            let position = monitor.position();
            let size = monitor.size();
            let logical_min_x = position.x as f64 / scale;
            let logical_min_y = position.y as f64 / scale;
            let logical_max_x = logical_min_x + size.width as f64 / scale;
            let logical_max_y = logical_min_y + size.height as f64 / scale;
            if x >= logical_min_x && x <= logical_max_x && y >= logical_min_y && y <= logical_max_y
            {
                return Some((
                    position.x as f64 + (x - logical_min_x) * scale,
                    position.y as f64 + (y - logical_min_y) * scale,
                ));
            }
        }
        for monitor in &monitors {
            let position = monitor.position();
            let size = monitor.size();
            let min_x = position.x as f64;
            let min_y = position.y as f64;
            let max_x = min_x + size.width as f64;
            let max_y = min_y + size.height as f64;
            if x >= min_x && x <= max_x && y >= min_y && y <= max_y {
                return Some((x, y));
            }
        }
        None
    }

    #[cfg(test)]
    mod macos_tests {
        use super::*;

        #[test]
        fn browser_or_webview_app_names_match_common_hosts() {
            assert!(is_browser_or_webview_app(Some("Google Chrome")));
            assert!(is_browser_or_webview_app(Some("Safari")));
            assert!(is_browser_or_webview_app(Some("GitHub Desktop")));
            assert!(is_browser_or_webview_app(Some("Visual Studio Code")));
            assert!(!is_browser_or_webview_app(Some("Notes")));
            assert!(!is_browser_or_webview_app(None));
        }

        #[test]
        fn extension_browser_app_names_match_chromium_hosts_only() {
            assert!(is_extension_browser_app(Some("Google Chrome")));
            assert!(is_extension_browser_app(Some("Brave Browser")));
            assert!(is_extension_browser_app(Some("Microsoft Edge")));
            assert!(is_extension_browser_app(Some("Arc")));
            assert!(!is_extension_browser_app(Some("Safari")));
            assert!(!is_extension_browser_app(Some("Firefox")));
            assert!(!is_extension_browser_app(Some("GitHub Desktop")));
            assert!(!is_extension_browser_app(None));
        }

        #[test]
        fn terminal_app_names_match_common_hosts() {
            assert!(is_terminal_app(Some("Terminal")));
            assert!(is_terminal_app(Some("iTerm2")));
            assert!(is_terminal_app(Some("Warp")));
            assert!(is_terminal_app(Some("Ghostty")));
            assert!(is_terminal_app(Some("WezTerm")));
            assert!(!is_terminal_app(Some("Google Chrome")));
            assert!(!is_terminal_app(None));
        }

        #[test]
        fn finder_app_names_match_only_finder() {
            assert!(is_finder_app(Some("Finder")));
            assert!(is_finder_app(Some("finder")));
            assert!(is_finder_app(Some("  Finder  ")));
            assert!(!is_finder_app(Some("FinderDuplicator")));
            assert!(!is_finder_app(Some("Google Chrome")));
            assert!(!is_finder_app(None));
        }

        #[test]
        fn finder_selection_parser_keeps_bounded_posix_paths() {
            let stdout = "/Users/a/report.pdf\n\nrelative.txt\n/Users/a/notes.md\n";
            assert_eq!(
                parse_finder_selection_paths(stdout),
                vec![
                    "/Users/a/report.pdf".to_string(),
                    "/Users/a/notes.md".to_string()
                ]
            );

            let many = (0..80)
                .map(|index| format!("/tmp/file-{index}.txt"))
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(parse_finder_selection_paths(&many).len(), 50);

            let huge = format!("/tmp/{}", "x".repeat(4096));
            assert_eq!(parse_finder_selection_paths(&huge)[0].chars().count(), 2048);

            assert!(parse_finder_selection_paths("no paths here").is_empty());
        }

        #[test]
        fn finder_selection_paths_format_as_context_list() {
            let formatted = format_selection_paths(&[
                "/Users/a/report.pdf".to_string(),
                "/Users/a/folder".to_string(),
            ]);
            assert_eq!(formatted, "- /Users/a/report.pdf\n- /Users/a/folder");
            // The 50k context cap applies through context_text_from_str, and
            // 50 bounded paths can never reach it.
            assert!(context_text_from_str(&formatted).is_some());
        }

        #[test]
        fn web_container_roles_match_browser_surfaces() {
            assert!(is_web_container_role("AXWebArea", ""));
            assert!(is_web_container_role("AXGroup", "AXDocument"));
            assert!(!is_web_container_role("AXTextField", ""));
        }

        #[test]
        fn left_option_tap_triggers_on_clean_release() {
            let left_option = KeyCode::OPTION as u16;
            let mut tracker = ModifierTracker::default();

            assert_eq!(
                tracker.track_flags_changed(
                    left_option,
                    left_option,
                    CGEventFlags::CGEventFlagAlternate
                ),
                ModifierGestureResult::LeftOptionTapStarted
            );
            assert_eq!(
                tracker.track_flags_changed(left_option, left_option, CGEventFlags::empty()),
                ModifierGestureResult::LeftOptionTapReleased
            );
        }

        #[test]
        fn left_option_tap_cancels_after_regular_keydown() {
            let left_option = KeyCode::OPTION as u16;
            let mut tracker = ModifierTracker::default();

            assert_eq!(
                tracker.track_flags_changed(
                    left_option,
                    left_option,
                    CGEventFlags::CGEventFlagAlternate
                ),
                ModifierGestureResult::LeftOptionTapStarted
            );
            tracker.cancel_left_option_tap();

            assert_eq!(
                tracker.track_flags_changed(left_option, left_option, CGEventFlags::empty()),
                ModifierGestureResult::None
            );
        }

        #[test]
        fn left_option_tap_cancels_when_combined_with_other_modifier() {
            let left_option = KeyCode::OPTION as u16;
            let left_control = KeyCode::CONTROL as u16;
            let mut tracker = ModifierTracker::default();

            assert_eq!(
                tracker.track_flags_changed(
                    left_option,
                    left_option,
                    CGEventFlags::CGEventFlagAlternate
                ),
                ModifierGestureResult::LeftOptionTapStarted
            );
            assert_eq!(
                tracker.track_flags_changed(
                    left_control,
                    left_option,
                    CGEventFlags::CGEventFlagAlternate | CGEventFlags::CGEventFlagControl
                ),
                ModifierGestureResult::None
            );
            assert_eq!(
                tracker.track_flags_changed(
                    left_control,
                    left_option,
                    CGEventFlags::CGEventFlagAlternate
                ),
                ModifierGestureResult::None
            );

            assert_eq!(
                tracker.track_flags_changed(left_option, left_option, CGEventFlags::empty()),
                ModifierGestureResult::None
            );
        }

        #[test]
        fn second_left_option_press_can_cancel_pending_single_tap() {
            let left_option = KeyCode::OPTION as u16;
            let mut tracker = ModifierTracker::default();

            assert_eq!(
                tracker.track_flags_changed(
                    left_option,
                    left_option,
                    CGEventFlags::CGEventFlagAlternate
                ),
                ModifierGestureResult::LeftOptionTapStarted
            );
            assert_eq!(
                tracker.track_flags_changed(left_option, left_option, CGEventFlags::empty()),
                ModifierGestureResult::LeftOptionTapReleased
            );
            assert_eq!(
                tracker.track_flags_changed(
                    left_option,
                    left_option,
                    CGEventFlags::CGEventFlagAlternate
                ),
                ModifierGestureResult::LeftOptionTapStarted
            );
        }

        #[test]
        fn modifier_tracker_uses_live_flags_after_reset() {
            let left_option = KeyCode::OPTION as u16;
            let mut tracker = ModifierTracker::default();

            assert_eq!(
                tracker.track_flags_changed(
                    left_option,
                    left_option,
                    CGEventFlags::CGEventFlagAlternate
                ),
                ModifierGestureResult::LeftOptionTapStarted
            );
            tracker.reset();

            assert_eq!(
                tracker.track_flags_changed(
                    left_option,
                    left_option,
                    CGEventFlags::CGEventFlagAlternate
                ),
                ModifierGestureResult::LeftOptionTapStarted
            );
        }

        #[test]
        fn modifier_tracker_release_without_prior_down_does_not_flip_state() {
            let left_option = KeyCode::OPTION as u16;
            let mut tracker = ModifierTracker::default();

            assert_eq!(
                tracker.track_flags_changed(left_option, left_option, CGEventFlags::empty()),
                ModifierGestureResult::None
            );
            assert_eq!(
                tracker.track_flags_changed(
                    left_option,
                    left_option,
                    CGEventFlags::CGEventFlagAlternate
                ),
                ModifierGestureResult::LeftOptionTapStarted
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ContextualAssistConfig;

    #[test]
    fn excluded_apps_match_case_insensitively() {
        let mut config = ContextualAssistConfig::default();
        config.excluded_apps = vec!["Password Manager".to_string()];

        assert!(app_is_excluded(Some("password manager"), &config));
        assert!(!app_is_excluded(Some("Safari"), &config));
        assert!(!app_is_excluded(None, &config));
    }

    #[test]
    fn browser_probe_state_prefers_extension_state() {
        let probe = BrowserProbeResponse {
            eligible: true,
            reason: Some("focused_writable".to_string()),
            state: Some("draft".to_string()),
            title: None,
            tab_title: None,
            url: None,
            tab_url: None,
            frame_url: None,
            tab_id: None,
            window_id: None,
            is_writable: true,
            has_selection: false,
        };

        assert_eq!(state_from_browser_probe(&probe), "draft");
    }

    #[test]
    fn browser_probe_state_infers_selection_field() {
        let probe = BrowserProbeResponse {
            eligible: true,
            reason: Some("input_selection".to_string()),
            state: None,
            title: None,
            tab_title: None,
            url: None,
            tab_url: None,
            frame_url: None,
            tab_id: None,
            window_id: None,
            is_writable: true,
            has_selection: true,
        };

        assert_eq!(state_from_browser_probe(&probe), "selection-field");
    }

    #[test]
    fn context_text_trims_empty_and_caps_long_text() {
        assert_eq!(
            context_text_from_str("  selected text  ").as_deref(),
            Some("selected text")
        );
        assert_eq!(context_text_from_str("   "), None);

        let long_text = "x".repeat(MAX_CONTEXT_TEXT_CHARS + 8);
        assert_eq!(
            context_text_from_str(&long_text).map(|text| text.chars().count()),
            Some(MAX_CONTEXT_TEXT_CHARS)
        );
    }

    #[test]
    fn contextual_writing_error_detail_prefers_details_reason() {
        let detail = contextual_writing_error_detail(
            r#"{"code":"invalid_request_payload","error":"Invalid request payload","details":{"reason":"JSON payload exceeds 33554432 bytes"}}"#,
        );
        assert_eq!(detail, "JSON payload exceeds 33554432 bytes");

        assert_eq!(
            contextual_writing_error_detail(r#"{"error":"fallback"}"#),
            "fallback"
        );
    }

    #[test]
    fn contextual_assist_action_catalog_covers_core_states() {
        let catalog = contextual_assist_action_catalog();
        let state_ids = catalog
            .iter()
            .map(|state| state.id.as_str())
            .collect::<std::collections::HashSet<_>>();

        for expected in [
            STATE_SELECTION,
            STATE_SELECTION_FIELD,
            STATE_EMPTY_CONTEXT,
            STATE_EMPTY_NO_CONTEXT,
            STATE_DRAFT,
            STATE_SECURE,
            STATE_EXCLUDED,
            STATE_UNSUPPORTED,
        ] {
            assert!(state_ids.contains(expected), "missing state {expected}");
        }

        let secure = catalog
            .iter()
            .find(|state| state.id == STATE_SECURE)
            .expect("secure state exists");
        assert!(secure.actions.is_empty());
    }

    #[test]
    fn empty_no_context_state_supports_the_screen_fallback_menu() {
        // The hotkey's no-target fallback routes here (see
        // `screen_context_fallback_target`), so this state must keep the
        // observe/screenshot action plus the HUD/task escapes — otherwise the
        // fallback menu would open with nothing meaningful to offer.
        let state = contextual_assist_action_catalog()
            .into_iter()
            .find(|state| state.id == STATE_EMPTY_NO_CONTEXT)
            .expect("empty-no-context state exists");
        let action_ids = state
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>();

        assert!(action_ids.contains(&ACTION_OBSERVE_THEN_DRAFT));
        assert!(action_ids.contains(&ACTION_WRITE_FROM_CONTEXT));
        assert!(action_ids.contains(&ACTION_CREATE_TASK));
        assert!(action_ids.contains(&ACTION_OPEN_HUD));
    }

    #[test]
    fn insert_flag_guard_holds_across_scope_and_releases_on_drop() {
        assert!(!contextual_insert_in_progress());
        {
            let _guard = ContextualInsertFlagGuard::acquire();
            assert!(contextual_insert_in_progress());
        }
        assert!(!contextual_insert_in_progress());
    }

    #[test]
    fn insertion_focus_verification_is_strict_where_it_can_be() {
        // App must match, case/whitespace-insensitively.
        assert!(insertion_target_matches(
            "Safari",
            Some("Docs"),
            Some("safari "),
            Some("Docs")
        ));
        assert!(!insertion_target_matches(
            "Safari",
            None,
            Some("Chrome"),
            None
        ));
        // Both titles known → they must agree.
        assert!(!insertion_target_matches(
            "Safari",
            Some("Page A"),
            Some("Safari"),
            Some("Page B")
        ));
        // Missing titles degrade to the app check — titles are volatile.
        assert!(insertion_target_matches(
            "Safari",
            None,
            Some("Safari"),
            Some("older title")
        ));
        assert!(insertion_target_matches(
            "Safari",
            Some("Docs"),
            Some("Safari"),
            None
        ));
        // A missing/empty target app can never be verified.
        assert!(!insertion_target_matches("Safari", None, None, None));
        assert!(!insertion_target_matches("Safari", None, Some("  "), None));
    }

    #[test]
    fn page_context_state_summarizes_without_text() {
        // The browser's page-level menu item routes here with a URL and no
        // contextText, so the summarize action must be screenshot-backed
        // (the backend accepts text-less requests only when a screenshot is
        // required) and the state must expose the task/HUD escapes.
        let state = contextual_assist_action_catalog()
            .into_iter()
            .find(|state| state.id == STATE_PAGE_CONTEXT)
            .expect("page-context state exists");
        let summarize = state
            .actions
            .iter()
            .find(|action| action.id == ACTION_SUMMARIZE_PAGE)
            .expect("summarize_page action exists");
        assert!(summarize.requires_screenshot);

        let action_ids = state
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>();
        assert!(action_ids.contains(&ACTION_CREATE_TASK));
        assert!(action_ids.contains(&ACTION_OPEN_HUD));

        assert_eq!(target_text_kind_for_state(STATE_PAGE_CONTEXT), "page_url");
    }

    #[test]
    fn files_state_offers_finder_selection_actions() {
        let state = contextual_assist_action_catalog()
            .into_iter()
            .find(|state| state.id == STATE_FILES)
            .expect("files state exists");
        let summarize = state
            .actions
            .iter()
            .find(|action| action.id == ACTION_SUMMARIZE)
            .expect("summarize action exists");
        // Paths alone cannot ground a summary; the Finder window capture
        // must accompany them.
        assert!(summarize.requires_screenshot);

        let action_ids = state
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>();
        assert!(action_ids.contains(&ACTION_CREATE_TASK));
        assert!(action_ids.contains(&ACTION_OPEN_HUD));
        assert_eq!(target_text_kind_for_state(STATE_FILES), "file_paths");
    }

    #[test]
    fn contextual_assist_action_catalog_uses_stable_canonical_ids() {
        let draft = contextual_assist_action_catalog()
            .into_iter()
            .find(|state| state.id == STATE_DRAFT)
            .expect("draft state exists");
        let action_ids = draft
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>();

        assert!(action_ids.contains(&ACTION_CONTINUE_DRAFT));
        assert!(action_ids.contains(&ACTION_IMPROVE_DRAFT));
        assert!(action_ids.contains(&ACTION_SHORTEN));
        assert!(action_ids.contains(&ACTION_CLARIFY));
        assert!(action_ids.contains(&ACTION_CREATE_TASK));
        assert!(action_ids.contains(&ACTION_SCHEDULE_FOLLOWUP));
        assert!(action_ids.contains(&ACTION_OPEN_HUD));
    }

    #[test]
    fn contextual_assist_action_catalog_has_no_duplicate_state_actions() {
        for state in contextual_assist_action_catalog() {
            let mut seen = std::collections::HashSet::new();
            for action in &state.actions {
                assert!(
                    seen.insert(action.id.as_str()),
                    "duplicate action `{}` in state `{}`",
                    action.id,
                    state.id
                );
            }
        }
    }

    #[test]
    fn contextual_assist_action_context_trims_target_metadata() {
        let target = NativeAssistTarget {
            state: STATE_SELECTION.to_string(),
            app: Some(" Notes ".to_string()),
            window_title: Some(" Passport ".to_string()),
            url: Some(" https://example.test/doc ".to_string()),
            context_text: Some("  selected text  ".to_string()),
            source: Some("test".to_string()),
            frame_url: None,
            browser_tab_id: None,
            browser_window_id: None,
            window_rect: Some(ContextualAssistCaptureRect {
                x: 10,
                y: 20,
                width: 300,
                height: 200,
            }),
        };

        let context = contextual_assist_action_context(
            STATE_SELECTION.to_string(),
            "active".to_string(),
            Some(&target),
        );

        assert_eq!(context.app.as_deref(), Some("Notes"));
        assert_eq!(context.window_title.as_deref(), Some("Passport"));
        assert_eq!(context.url.as_deref(), Some("https://example.test/doc"));
        assert_eq!(context.context_text.as_deref(), Some("selected text"));
        assert!(context.has_context_text);
    }

    #[test]
    fn contextual_assist_action_routing_targets_writing_assistant_for_sites() {
        let action = contextual_assist_action_for_state(STATE_SELECTION, ACTION_REWRITE).unwrap();
        let target = NativeAssistTarget {
            state: STATE_SELECTION.to_string(),
            app: Some("Chrome".to_string()),
            window_title: Some("Inbox".to_string()),
            url: Some("https://mail.example.test/thread/123?x=1".to_string()),
            context_text: Some("hello".to_string()),
            source: Some("chrome_context_menu".to_string()),
            frame_url: None,
            browser_tab_id: Some(42),
            browser_window_id: Some(7),
            window_rect: None,
        };
        let context = contextual_assist_action_context(
            STATE_SELECTION.to_string(),
            "professional".to_string(),
            Some(&target),
        );
        let routing = contextual_assist_action_routing(&action, &context);

        assert_eq!(routing.agent_id, WRITING_ASSISTANT_AGENT_ID);
        assert_eq!(routing.source_kind, "site");
        assert_eq!(routing.source_key, "site:https://mail.example.test");
        assert_eq!(
            routing.root_url.as_deref(),
            Some("https://mail.example.test")
        );
        assert_eq!(routing.target_text_kind, "selected_text");
        assert_eq!(routing.action_intent, "rewrite_selection");
        assert_eq!(routing.personality, "professional");
        assert!(routing
            .session_key
            .starts_with("contextual-writing:site-https-mail-example-test"));
    }

    #[test]
    fn contextual_assist_action_routing_falls_back_to_app_key() {
        let action = contextual_assist_action_for_state(STATE_DRAFT, ACTION_IMPROVE_DRAFT).unwrap();
        let target = NativeAssistTarget {
            state: STATE_DRAFT.to_string(),
            app: Some("Notes".to_string()),
            window_title: Some("Draft".to_string()),
            url: None,
            context_text: Some("draft text".to_string()),
            source: Some("accessibility".to_string()),
            frame_url: None,
            browser_tab_id: None,
            browser_window_id: None,
            window_rect: Some(ContextualAssistCaptureRect {
                x: 80,
                y: 90,
                width: 640,
                height: 480,
            }),
        };
        let context = contextual_assist_action_context(
            STATE_DRAFT.to_string(),
            "active".to_string(),
            Some(&target),
        );
        let routing = contextual_assist_action_routing(&action, &context);

        assert_eq!(routing.source_kind, "app");
        assert_eq!(routing.source_key, "app:Notes");
        assert_eq!(routing.root_url, None);
        assert_eq!(routing.target_text_kind, "field_text");
        assert_eq!(routing.session_key, "contextual-writing:app-notes");
    }

    #[test]
    fn contextual_assist_screenshot_policy_skips_only_shorten_generation_actions() {
        for state in contextual_assist_action_catalog() {
            for action in state.actions {
                if action.opens_hud {
                    assert!(
                        !action.requires_screenshot,
                        "HUD action should defer to the full HUD surface"
                    );
                } else if action.id == ACTION_SHORTEN {
                    assert!(
                        !action.requires_screenshot,
                        "Shorten/make concise must remain text-only"
                    );
                } else {
                    assert!(
                        action.requires_screenshot,
                        "action `{}` in state `{}` should capture screen context",
                        action.id, state.id
                    );
                }
            }
        }
    }

    #[test]
    fn contextual_assist_visual_context_uses_chrome_visible_tab_when_available() {
        let action = contextual_assist_action_for_state(STATE_SELECTION, ACTION_DRAFT_REPLY)
            .expect("reply action exists");
        let rect = ContextualAssistCaptureRect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };
        let target = NativeAssistTarget {
            state: STATE_SELECTION.to_string(),
            app: Some("Google Chrome".to_string()),
            window_title: Some("Inbox".to_string()),
            url: Some("https://mail.example.test/thread/123".to_string()),
            context_text: Some("selected text".to_string()),
            source: Some("chrome_context_menu".to_string()),
            frame_url: Some("https://mail.example.test/thread/123".to_string()),
            browser_tab_id: Some(101),
            browser_window_id: Some(202),
            window_rect: Some(rect.clone()),
        };

        let visual_context = contextual_assist_visual_context(&action, Some(&target));

        assert!(visual_context.screenshot.required);
        assert_eq!(
            visual_context.screenshot.source,
            SCREENSHOT_SOURCE_CHROME_VISIBLE_TAB
        );
        assert_eq!(visual_context.screenshot.browser_tab_id, Some(101));
        assert_eq!(visual_context.screenshot.browser_window_id, Some(202));
        assert_eq!(visual_context.screenshot.window_rect, Some(rect));
        assert!(!visual_context.screenshot.degraded);
    }

    #[test]
    fn contextual_assist_browser_capture_fallback_preserves_window_rect() {
        let request = ContextualAssistScreenshotRequest {
            policy: SCREENSHOT_POLICY_REQUIRED.to_string(),
            required: true,
            source: SCREENSHOT_SOURCE_CHROME_VISIBLE_TAB.to_string(),
            reason: "writing_action_needs_visible_tab_context".to_string(),
            degraded: false,
            window_rect: Some(ContextualAssistCaptureRect {
                x: 10,
                y: 20,
                width: 800,
                height: 600,
            }),
            browser_tab_id: Some(101),
            browser_window_id: Some(202),
        };

        let fallback = contextual_assist_browser_capture_fallback_request(&request);

        assert_eq!(fallback.source, SCREENSHOT_SOURCE_ACTIVE_WINDOW_REGION);
        assert!(fallback.degraded);
        assert_eq!(fallback.window_rect, request.window_rect);
        assert_eq!(fallback.browser_tab_id, Some(101));
        assert_eq!(fallback.browser_window_id, Some(202));
    }

    #[test]
    fn contextual_assist_visual_context_uses_window_region_for_native_apps() {
        let action = contextual_assist_action_for_state(STATE_DRAFT, ACTION_IMPROVE_DRAFT).unwrap();
        let rect = ContextualAssistCaptureRect {
            x: 80,
            y: 90,
            width: 640,
            height: 480,
        };
        let target = NativeAssistTarget {
            state: STATE_DRAFT.to_string(),
            app: Some("Notes".to_string()),
            window_title: Some("Draft".to_string()),
            url: None,
            context_text: Some("draft text".to_string()),
            source: Some("accessibility".to_string()),
            frame_url: None,
            browser_tab_id: None,
            browser_window_id: None,
            window_rect: Some(rect.clone()),
        };

        let visual_context = contextual_assist_visual_context(&action, Some(&target));

        assert!(visual_context.screenshot.required);
        assert_eq!(
            visual_context.screenshot.source,
            SCREENSHOT_SOURCE_ACTIVE_WINDOW_REGION
        );
        assert_eq!(visual_context.screenshot.window_rect, Some(rect));
        assert!(!visual_context.screenshot.degraded);
    }

    #[test]
    fn contextual_assist_visual_context_skips_screenshot_for_shorten() {
        let action = contextual_assist_action_for_state(STATE_DRAFT, ACTION_SHORTEN).unwrap();
        let target = NativeAssistTarget {
            state: STATE_DRAFT.to_string(),
            app: Some("Notes".to_string()),
            window_title: Some("Draft".to_string()),
            url: None,
            context_text: Some("draft text".to_string()),
            source: Some("accessibility".to_string()),
            frame_url: None,
            browser_tab_id: None,
            browser_window_id: None,
            window_rect: Some(ContextualAssistCaptureRect {
                x: 80,
                y: 90,
                width: 640,
                height: 480,
            }),
        };

        let visual_context = contextual_assist_visual_context(&action, Some(&target));

        assert_eq!(visual_context.screenshot.policy, SCREENSHOT_POLICY_SKIPPED);
        assert!(!visual_context.screenshot.required);
        assert_eq!(visual_context.screenshot.source, SCREENSHOT_SOURCE_NONE);
        assert_eq!(visual_context.screenshot.reason, "make_concise_text_only");
    }

    #[test]
    fn tauri_webview_context_normalizes_text_and_excludes_internal_routes() {
        let normalized = normalize_tauri_webview_context(TauriWebviewContextPayload {
            route: Some("/hud".to_string()),
            url: Some("http://localhost:5173/hud".to_string()),
            title: Some(" HUD ".to_string()),
            selected_text: Some("  hello  ".to_string()),
            field_text: Some("  draft  ".to_string()),
            editable: true,
            has_selection: false,
            secure: false,
        })
        .expect("selected text should normalize");

        assert_eq!(normalized.title.as_deref(), Some("HUD"));
        assert_eq!(normalized.selected_text.as_deref(), Some("hello"));
        assert_eq!(normalized.field_text.as_deref(), Some("draft"));
        assert!(normalized.has_selection);

        assert!(normalize_tauri_webview_context(TauriWebviewContextPayload {
            route: Some("/contextual-assist?preview=1".to_string()),
            url: None,
            title: None,
            selected_text: Some("ignored".to_string()),
            field_text: None,
            editable: false,
            has_selection: true,
            secure: false,
        })
        .is_none());

        assert!(normalize_tauri_webview_context(TauriWebviewContextPayload {
            route: Some("/settings".to_string()),
            url: None,
            title: None,
            selected_text: None,
            field_text: None,
            editable: true,
            has_selection: false,
            secure: false,
        })
        .is_some());
    }

    #[test]
    fn browser_probe_title_must_match_native_window_when_available() {
        let probe = BrowserProbeResponse {
            eligible: true,
            reason: None,
            state: Some("draft".to_string()),
            title: Some("Inbox - Example".to_string()),
            tab_title: None,
            url: None,
            tab_url: None,
            frame_url: None,
            tab_id: None,
            window_id: None,
            is_writable: true,
            has_selection: false,
        };

        assert!(browser_probe_matches_window_title(
            &probe,
            Some("Inbox - Example - Google Chrome"),
            Some("Google Chrome")
        ));
        assert!(!browser_probe_matches_window_title(
            &probe,
            Some("Calendar - Google Chrome"),
            Some("Google Chrome")
        ));
        assert!(browser_probe_matches_window_title(
            &probe,
            None,
            Some("Google Chrome")
        ));
        assert!(browser_probe_matches_window_title(
            &probe,
            Some("Arc"),
            Some("Arc")
        ));
    }
}
