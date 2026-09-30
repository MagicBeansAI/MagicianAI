//! Screen capture-and-ask — the ⇧⌥S (screenshot) and ⇧⌥R (clip) global
//! chords (plan: `docs/archive/plans/2026-06-11-screen-capture-and-ask.md`).
//!
//! Flow on chord press:
//!   1. ⇧⌥S → POST `/screen/capture`: the backend grabs the main display,
//!      resolves today's dated session under the `screens` thread, and
//!      stages the shot as a chat attachment. Capture happens BEFORE the
//!      HUD shows, so the asking surface is never in frame.
//!      ⇧⌥R → POST `/screen/clip/toggle`: first press starts the recorder
//!      (macOS shows its system screen-recording indicator — the OS-level
//!      truth, no app claim needed), second press (or the backend's 30s
//!      cap) stops it; the stop response stages the mp4 + sampled frames.
//!   2. On a staged result: stash the capture descriptor in
//!      [`ScreenAskState`] and summon the overlay HUD, then emit
//!      [`SCREEN_ASK_EVENT`] with the same payload. Two delivery paths on
//!      purpose: the event reaches an already-loaded HUD webview instantly;
//!      a freshly-created HUD window misses the emit (its listeners aren't
//!      mounted yet) and pulls the stashed capture via the
//!      `take_screen_ask_capture` command on mount instead.
//!
//! The HUD binds its ChatPanel to the returned thread/session and surfaces
//! the staged attachment chips; the user's typed (or dictated) question
//! rides the normal chat send with `attachment_ids`, and the existing
//! vision path takes it from there.
//!
//! Failure posture: if the backend is unreachable or capture fails, we log
//! and do NOT summon the HUD — opening an empty asking surface with nothing
//! staged would misreport what happened. The tray already surfaces backend
//! health.

use serde::{Deserialize, Serialize};
use tauri::utils::config::Color;
use tauri::{AppHandle, Emitter, Manager, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};
use tracing::{info, warn};

/// Event emitted to the HUD webview when a capture is staged and the overlay
/// has been summoned.
pub const SCREEN_ASK_EVENT: &str = "screen-ask-capture";
const SCREEN_REGION_PICKER_WINDOW_LABEL: &str = "screen-region-picker";
#[cfg(debug_assertions)]
const SCREEN_REGION_PICKER_DEV_URL: &str = "http://localhost:5173/screen-region-picker";
#[cfg(not(debug_assertions))]
const SCREEN_REGION_PICKER_APP_PATH: &str = "/screen-region-picker";
const SCREEN_REGION_PICKER_RESET_EVENT: &str = "screen-region-picker-reset";

/// Scope the desktop acts under — mirrors the web UI's
/// `scopeIdentityStore` defaults (single-operator deployment).

#[derive(Debug, Default)]
pub struct ScreenAskState {
    pub registered_shortcut: Option<String>,
    pub registered_clip_shortcut: Option<String>,
    pub registered_region_shortcut: Option<String>,
    pub registered_watch_shortcut: Option<String>,
    /// True once the startup sync has run — distinguishes "not yet
    /// registered" (tray shows the defaults) from "explicitly disabled"
    /// (tray shows no accelerator).
    pub chords_synced: bool,
    /// Last capture awaiting pickup by a freshly-created HUD webview
    /// (consumed once by `take_screen_ask_capture`).
    pub pending_capture: Option<ScreenAskCapture>,
}

/// One staged attachment, in the backend's response shape.
#[derive(Debug, Clone, Serialize)]
pub struct ScreenAskAttachment {
    pub attachment_id: String,
    pub stored_name: String,
    pub mime_type: String,
    pub size_bytes: u64,
}

/// What the HUD needs to bind ChatPanel and surface the staged chips.
/// Field names mirror the backend's capture/clip responses.
#[derive(Debug, Clone, Serialize)]
pub struct ScreenAskCapture {
    pub capture_id: String,
    pub mode: String,
    pub thread_id: String,
    pub session_id: String,
    pub session_title: String,
    pub attachments: Vec<ScreenAskAttachment>,
    /// Provenance the backend echoes back: the app that was frontmost when
    /// the chord fired. The HUD shows it on the staged chip so a capture is
    /// self-describing ("screen capture — Safari"), not anonymous pixels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_app: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_window_title: Option<String>,
}

/// Frontmost app/window at chord time, for the capture POST body. Reading it
/// AFTER the region picker hides (and its 140 ms settle) is deliberate: the
/// picker is Magician's own window, and the user's app must be frontmost again
/// for the provenance to describe what was actually on screen.
fn capture_source_context() -> serde_json::Value {
    match crate::contextual_assist::frontmost_app_and_window_title() {
        Some((app, window_title)) => serde_json::json!({
            "source_app": app,
            "source_window_title": window_title,
        }),
        None => serde_json::json!({}),
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenRegionSelection {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub viewport_width: f64,
    pub viewport_height: f64,
}

#[derive(Debug, Clone, Serialize)]
struct ScreenRegionRectPayload {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

pub fn validate_screen_ask_shortcut(shortcut: &str) -> Result<String, String> {
    // Optional chord: empty / none / disabled → feature off. A non-empty
    // value must parse as a real chord.
    let normalized = crate::config::normalize_optional_shortcut(shortcut);
    if normalized.is_empty() {
        return Ok(String::new());
    }
    normalized
        .parse::<Shortcut>()
        .map_err(|e| format!("Invalid screen-ask shortcut '{}': {}", normalized, e))?;
    Ok(normalized)
}

fn handle_screen_ask_shortcut(app: &AppHandle, _shortcut: &Shortcut, event: ShortcutEvent) {
    // Keydown edge only — same double-fire guard as the overlay chord.
    if event.state != ShortcutState::Pressed {
        return;
    }
    trigger_screen_ask(app);
}

fn handle_screen_clip_shortcut(app: &AppHandle, _shortcut: &Shortcut, event: ShortcutEvent) {
    if event.state != ShortcutState::Pressed {
        return;
    }
    trigger_screen_clip(app);
}

fn handle_screen_region_shortcut(app: &AppHandle, _shortcut: &Shortcut, event: ShortcutEvent) {
    if event.state != ShortcutState::Pressed {
        return;
    }
    trigger_screen_region(app);
}

fn handle_screen_watch_shortcut(app: &AppHandle, _shortcut: &Shortcut, event: ShortcutEvent) {
    if event.state != ShortcutState::Pressed {
        return;
    }
    trigger_screen_watch(app);
}

/// Which chord a sync call manages (each tracks its own registration).
#[derive(Clone, Copy)]
pub enum ScreenChord {
    Screenshot,
    Clip,
    Region,
    Watch,
}

/// Register/unregister a screen chord to match config. Mirrors
/// `overlay::sync_overlay_shortcut` against [`ScreenAskState`].
pub async fn sync_screen_chord(
    app: &AppHandle,
    chord: ScreenChord,
    shortcut: &str,
) -> Result<String, String> {
    let desired = validate_screen_ask_shortcut(shortcut)?;
    let desired_opt = if desired.is_empty() {
        None
    } else {
        Some(desired.clone())
    };
    let handler = match chord {
        ScreenChord::Screenshot => {
            handle_screen_ask_shortcut as fn(&AppHandle, &Shortcut, ShortcutEvent)
        },
        ScreenChord::Clip => handle_screen_clip_shortcut,
        ScreenChord::Region => handle_screen_region_shortcut,
        ScreenChord::Watch => handle_screen_watch_shortcut,
    };
    let label = match chord {
        ScreenChord::Screenshot => "screen-ask",
        ScreenChord::Clip => "screen-clip",
        ScreenChord::Region => "screen-region",
        ScreenChord::Watch => "screen-watch",
    };
    let state = app.state::<crate::AppState>();
    let previous = {
        let screen_ask = state.screen_ask.lock().await;
        match chord {
            ScreenChord::Screenshot => screen_ask.registered_shortcut.clone(),
            ScreenChord::Clip => screen_ask.registered_clip_shortcut.clone(),
            ScreenChord::Region => screen_ask.registered_region_shortcut.clone(),
            ScreenChord::Watch => screen_ask.registered_watch_shortcut.clone(),
        }
    };

    if previous != desired_opt {
        if let Some(current) = previous.as_deref() {
            if app.global_shortcut().is_registered(current) {
                app.global_shortcut().unregister(current).map_err(|e| {
                    format!("Failed to unregister {} shortcut {}: {}", label, current, e)
                })?;
            }
        }
        if let Some(desired) = desired_opt.as_deref() {
            if let Err(error) = app.global_shortcut().on_shortcut(desired, handler) {
                if let Some(current) = previous.as_deref() {
                    let _ = app.global_shortcut().on_shortcut(current, handler);
                }
                return Err(format!(
                    "Failed to register {} shortcut {}: {}",
                    label, desired, error
                ));
            }
            info!("Registered {} shortcut {}", label, desired);
        } else {
            info!("{} chord disabled", label);
        }
    }

    {
        let mut screen_ask = state.screen_ask.lock().await;
        match chord {
            ScreenChord::Screenshot => screen_ask.registered_shortcut = desired_opt,
            ScreenChord::Clip => screen_ask.registered_clip_shortcut = desired_opt,
            ScreenChord::Region => screen_ask.registered_region_shortcut = desired_opt,
            ScreenChord::Watch => screen_ask.registered_watch_shortcut = desired_opt,
        }
        screen_ask.chords_synced = true;
    }
    // The tray's Screen section shows each chord as a menu accelerator —
    // rebuild it so the labels track config (same pattern as the overlay
    // chord's sync).
    let update_version = state.pending_app_update.lock().await.clone();
    crate::tray::refresh_menu(app, update_version.as_deref());
    Ok(desired)
}

/// Current chord strings for the tray menu accelerators, in
/// (screenshot, region, clip, watch) order. Before the startup sync the
/// config DEFAULTS show (the chords are default-on, so that's what
/// registration will produce); after it, an explicitly disabled chord
/// shows no accelerator (empty string).
pub fn current_screen_chords(app: &AppHandle) -> (String, String, String, String) {
    let state = app.state::<crate::AppState>();
    if let Ok(screen_ask) = state.screen_ask.try_lock() {
        if screen_ask.chords_synced {
            let get = |registered: &Option<String>| registered.clone().unwrap_or_default();
            return (
                get(&screen_ask.registered_shortcut),
                get(&screen_ask.registered_region_shortcut),
                get(&screen_ask.registered_clip_shortcut),
                get(&screen_ask.registered_watch_shortcut),
            );
        }
    }
    (
        crate::config::DEFAULT_SCREEN_ASK_SHORTCUT.to_string(),
        crate::config::DEFAULT_SCREEN_REGION_SHORTCUT.to_string(),
        crate::config::DEFAULT_SCREEN_CLIP_SHORTCUT.to_string(),
        crate::config::DEFAULT_SCREEN_WATCH_SHORTCUT.to_string(),
    )
}

fn screen_region_picker_webview_url() -> Result<tauri::WebviewUrl, String> {
    #[cfg(debug_assertions)]
    {
        let url: tauri::Url = SCREEN_REGION_PICKER_DEV_URL.parse().map_err(|error| {
            format!(
                "Invalid screen region picker dev URL '{}': {}",
                SCREEN_REGION_PICKER_DEV_URL, error
            )
        })?;
        Ok(tauri::WebviewUrl::External(url))
    }

    #[cfg(not(debug_assertions))]
    {
        Ok(crate::tray::desktop_app_webview_url(
            SCREEN_REGION_PICKER_APP_PATH,
        ))
    }
}

fn ensure_screen_region_picker_window(app: &AppHandle) -> Result<(), String> {
    if app
        .get_webview_window(SCREEN_REGION_PICKER_WINDOW_LABEL)
        .is_some()
    {
        return Ok(());
    }

    let picker_url = screen_region_picker_webview_url()?;
    let window =
        tauri::WebviewWindowBuilder::new(app, SCREEN_REGION_PICKER_WINDOW_LABEL, picker_url)
            .title("Magican Region Picker")
            .decorations(false)
            .resizable(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .visible(false)
            .transparent(true)
            .background_color(Color(0, 0, 0, 0))
            .build()
            .map_err(|e| format!("Failed to create screen region picker window: {}", e))?;

    let close_app = app.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = hide_screen_region_picker_window(&close_app);
        }
    });

    Ok(())
}

fn show_screen_region_picker_window(app: &AppHandle) -> Result<(), String> {
    ensure_screen_region_picker_window(app)?;
    let Some(window) = app.get_webview_window(SCREEN_REGION_PICKER_WINDOW_LABEL) else {
        return Err("screen region picker window was not created".to_string());
    };
    fill_region_picker_screen(app, &window);
    let _ = window.set_ignore_cursor_events(false);
    let _ = window.set_background_color(Some(Color(0, 0, 0, 0)));
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
    let _ = window.emit(SCREEN_REGION_PICKER_RESET_EVENT, ());
    info!("screen-region: rect-aware picker shown");
    Ok(())
}

fn hide_screen_region_picker_window(app: &AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(SCREEN_REGION_PICKER_WINDOW_LABEL) {
        let _ = window.hide();
    }
    Ok(())
}

fn fill_region_picker_screen(app: &AppHandle, window: &tauri::WebviewWindow) {
    let monitors = app.available_monitors().unwrap_or_default();
    let cursor_pos = app.cursor_position().ok();
    let cursor_monitor = cursor_pos.and_then(|pos| {
        monitors
            .iter()
            .find(|monitor| {
                let size = monitor.size();
                let position = monitor.position();
                let mx = position.x as f64;
                let my = position.y as f64;
                let mw = size.width as f64;
                let mh = size.height as f64;
                pos.x >= mx && pos.x < mx + mw && pos.y >= my && pos.y < my + mh
            })
            .cloned()
    });
    let monitor = cursor_monitor
        .or_else(|| window.primary_monitor().ok().flatten())
        .or_else(|| monitors.first().cloned());

    if let Some(monitor) = monitor {
        let size = monitor.size();
        let position = monitor.position();
        let _ = window.set_position(tauri::PhysicalPosition::new(position.x, position.y));
        let _ = window.set_size(tauri::PhysicalSize::new(size.width, size.height));
        info!(
            "screen-region: picker covers monitor {}x{} at ({}, {})",
            size.width, size.height, position.x, position.y
        );
    }
}

fn selection_to_region_rect(
    app: &AppHandle,
    selection: &ScreenRegionSelection,
) -> Result<ScreenRegionRectPayload, String> {
    if !selection.x.is_finite()
        || !selection.y.is_finite()
        || !selection.width.is_finite()
        || !selection.height.is_finite()
        || !selection.viewport_width.is_finite()
        || !selection.viewport_height.is_finite()
        || selection.viewport_width <= 0.0
        || selection.viewport_height <= 0.0
    {
        return Err("invalid screen-region selection dimensions".to_string());
    }

    let Some(window) = app.get_webview_window(SCREEN_REGION_PICKER_WINDOW_LABEL) else {
        return Err("screen region picker window is not available".to_string());
    };
    let position = window
        .outer_position()
        .map_err(|e| format!("screen region picker position unavailable: {e}"))?;
    let size = window
        .inner_size()
        .map_err(|e| format!("screen region picker size unavailable: {e}"))?;
    if size.width == 0 || size.height == 0 {
        return Err("screen region picker has zero size".to_string());
    }

    let x1 = selection
        .x
        .min(selection.x + selection.width)
        .clamp(0.0, selection.viewport_width);
    let y1 = selection
        .y
        .min(selection.y + selection.height)
        .clamp(0.0, selection.viewport_height);
    let x2 = selection
        .x
        .max(selection.x + selection.width)
        .clamp(0.0, selection.viewport_width);
    let y2 = selection
        .y
        .max(selection.y + selection.height)
        .clamp(0.0, selection.viewport_height);
    if (x2 - x1) < 4.0 || (y2 - y1) < 4.0 {
        return Err("screen region selection is too small".to_string());
    }

    let scale_x = size.width as f64 / selection.viewport_width;
    let scale_y = size.height as f64 / selection.viewport_height;
    let left = position.x as f64 + x1 * scale_x;
    let top = position.y as f64 + y1 * scale_y;
    let right = position.x as f64 + x2 * scale_x;
    let bottom = position.y as f64 + y2 * scale_y;

    let rect_x = left.floor() as i32;
    let rect_y = top.floor() as i32;
    let rect_width = (right.ceil() - left.floor()).max(1.0) as u32;
    let rect_height = (bottom.ceil() - top.floor()).max(1.0) as u32;
    Ok(ScreenRegionRectPayload {
        x: rect_x,
        y: rect_y,
        width: rect_width,
        height: rect_height,
    })
}

/// Screenshot capture → stash → summon HUD → emit. Safe to call from any
/// thread (offloads to the async runtime), so a future wake-voice trigger can
/// call this exact function.
pub fn trigger_screen_ask(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut body = serde_json::json!({ "mode": "screenshot" });
        if let Some(source) = capture_source_context().as_object() {
            for (key, value) in source {
                body[key.clone()] = value.clone();
            }
        }
        match post_screen_endpoint(&app, "screen/capture", 15, body).await {
            Ok(body) => match parse_staged_capture(&body) {
                Ok(capture) => deliver_capture(&app, capture).await,
                Err(error) => warn!("screen-ask: bad capture response: {}", error),
            },
            Err(error) => warn!("screen-ask: capture failed: {}", error),
        }
    });
}

/// Region capture (⇧⌥A): Magician shows its own transparent region picker so
/// the selected crop carries a real global screen rect. The backend still owns
/// the actual capture via `/screen/capture { mode:"region", region_rect }`.
/// Escape from the picker is a deliberate cancel — no HUD, no noise.
pub fn trigger_screen_region(app: &AppHandle) {
    if let Err(error) = show_screen_region_picker_window(app) {
        warn!("screen-region: failed to show rect-aware picker: {}", error);
    }
}

/// Clip chord: first press starts the recorder, second press stops + stages.
/// The backend owns all recording state (single toggle endpoint), so this
/// stays stateless — a backend-side cap auto-stop needs no mirroring here.
pub fn trigger_screen_clip(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // Stop+stage runs ffmpeg + stages several files — generous timeout.
        match post_screen_endpoint(&app, "screen/clip/toggle", 45, serde_json::json!({})).await {
            Ok(body) => {
                let phase = body.get("phase").and_then(|v| v.as_str()).unwrap_or("");
                match phase {
                    "recording" => {
                        // No HUD yet — the macOS system screen-recording
                        // indicator is the user-facing signal until stop.
                        info!(
                            "screen-clip: recording started (cap {}s)",
                            body.get("max_duration_s")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0)
                        );
                    },
                    "staged" => match parse_staged_capture(&body) {
                        Ok(capture) => deliver_capture(&app, capture).await,
                        Err(error) => warn!("screen-clip: bad staged response: {}", error),
                    },
                    other => warn!("screen-clip: unexpected phase `{}`", other),
                }
            },
            Err(error) => warn!("screen-clip: toggle failed: {}", error),
        }
    });
}

/// Watch chord (⇧⌥W): toggle a continuous observation session.
///
/// Press → a notes-mode observation starts INSTANTLY (capture is already
/// running — nothing is lost) and the HUD opens on the fresh observation
/// session: the 👁 announce line is visible and the composer is the
/// "what should I watch for?" inbox — typing a condition retargets the
/// running session to watch mode (via the skill); doing nothing/Escape
/// leaves it as a pure notes session. Notes mode is the default by
/// inaction, exactly as designed.
///
/// Press again → the session stops (teardown summary + memory) and the
/// HUD opens on the finished session, ask-ready ("what did I do?").
pub fn trigger_screen_watch(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // Stop includes the teardown summary (local LLM) — generous timeout.
        match post_screen_endpoint(&app, "screen/observe/toggle", 75, serde_json::json!({})).await {
            Ok(body) => {
                let phase = body.get("phase").and_then(|v| v.as_str()).unwrap_or("");
                match phase {
                    "observing" | "stopped" => {
                        info!(
                            "screen-watch: {} ({})",
                            phase,
                            body.get("observe_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?")
                        );
                        // Both edges open the HUD on the observation session:
                        // at start it's the optional "what should I watch
                        // for?" inbox; at stop it's the review surface. No
                        // chips either way — the narration IS the content.
                        let capture = ScreenAskCapture {
                            capture_id: body
                                .get("observe_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("observe")
                                .to_string(),
                            mode: "observe".to_string(),
                            thread_id: body
                                .get("thread_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("screen-watch")
                                .to_string(),
                            session_id: body
                                .get("session_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            session_title: body
                                .get("purpose")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            attachments: Vec::new(),
                            // Observation spans time; a single chord-time app
                            // label would misdescribe it, so it stays unlabelled.
                            source_app: None,
                            source_window_title: None,
                        };
                        deliver_capture(&app, capture).await;
                    },
                    "idle" => info!("screen-watch: nothing to stop"),
                    other => warn!("screen-watch: unexpected phase `{}`", other),
                }
            },
            Err(error) => warn!("screen-watch: toggle failed: {}", error),
        }
    });
}

/// Stash + summon + emit — shared tail of both staged flows.
async fn deliver_capture(app: &AppHandle, capture: ScreenAskCapture) {
    {
        let state = app.state::<crate::AppState>();
        let mut screen_ask = state.screen_ask.lock().await;
        screen_ask.pending_capture = Some(capture.clone());
    }
    if let Err(error) = crate::overlay::show_overlay_window(app) {
        warn!("screen-ask: failed to summon HUD: {}", error);
    }
    let _ = app.emit(SCREEN_ASK_EVENT, &capture);
    info!(
        "screen-ask: {} {} staged on session {} ({} attachment(s))",
        capture.mode,
        capture.capture_id,
        capture.session_id,
        capture.attachments.len()
    );
}

async fn post_screen_endpoint(
    app: &AppHandle,
    path: &str,
    timeout_secs: u64,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let url = {
        let state = app.state::<crate::AppState>();
        let config = state.config.lock().await;
        config.engine_url(&format!("/api/magician/v2/{path}"))
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| format!("http client: {}", e))?;
    let response = crate::magician_auth::authorize(client.post(&url))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("POST {}: {}", url, e))?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("parse response: {}", e))?;
    if !status.is_success() {
        let detail = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        return Err(format!("backend returned {}: {}", status, detail));
    }
    Ok(body)
}

fn parse_staged_capture(body: &serde_json::Value) -> Result<ScreenAskCapture, String> {
    let field = |name: &str| -> Result<String, String> {
        body.get(name)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| format!("response missing `{}`", name))
    };
    let attachments = body
        .get("attachments")
        .and_then(|v| v.as_array())
        .ok_or("response missing `attachments`")?
        .iter()
        .map(|item| {
            let sub = |name: &str| {
                item.get(name)
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .ok_or_else(|| format!("attachment missing `{}`", name))
            };
            Ok(ScreenAskAttachment {
                attachment_id: sub("attachment_id")?,
                stored_name: sub("stored_name")?,
                mime_type: sub("mime_type")?,
                size_bytes: item.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    if attachments.is_empty() {
        return Err("response carried no attachments".to_string());
    }
    let optional_string = |name: &str| {
        body.get(name)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|value| !value.trim().is_empty())
    };
    Ok(ScreenAskCapture {
        capture_id: field("capture_id")?,
        mode: field("mode")?,
        thread_id: field("thread_id")?,
        session_id: field("session_id")?,
        session_title: field("session_title")?,
        attachments,
        source_app: optional_string("source_app"),
        source_window_title: optional_string("source_window_title"),
    })
}

/// HUD pull path: a freshly-created HUD webview mounts AFTER the summon emit,
/// so it asks for the pending capture instead. Consume-once.
#[tauri::command]
pub async fn take_screen_ask_capture(app: AppHandle) -> Result<Option<ScreenAskCapture>, String> {
    let state = app.state::<crate::AppState>();
    let mut screen_ask = state.screen_ask.lock().await;
    Ok(screen_ask.pending_capture.take())
}

/// HUD "attach what I was looking at" (one tap, explicit — no silent
/// capture): briefly hide the HUD, capture the previous frontmost app's
/// window region through the same staging lane the region chord uses
/// (provenance labeled with that app), then re-summon the HUD with the
/// capture staged as a seed chip the user can still discard by not sending.
/// When the HUD is already bound to a chat session, the capture stages into
/// THAT session — rebinding the HUD to the daily screens thread here would
/// clear whatever the user already staged in this conversation.
#[tauri::command]
pub async fn hud_attach_screen_context(
    app: AppHandle,
    session_id: Option<String>,
) -> Result<(), String> {
    crate::overlay::hide_overlay_window(&app)?;
    // Let the HUD fully leave the screen so the region capture sees the
    // real pixels, and focus returns to the app the user was in.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let source = crate::contextual_assist::frontmost_app_and_window_title();
    let Some(rect) = crate::contextual_assist::frontmost_window_capture_rect(&app) else {
        let _ = crate::overlay::show_overlay_window(&app);
        return Err("no capturable window was frontmost".to_string());
    };

    let mut body = serde_json::json!({
        "mode": "region",
        "region_rect": ScreenRegionRectPayload {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        },
    });
    if let Some(session_id) = session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        body["session_id"] = serde_json::json!(session_id);
    }
    if let Some((app_name, window_title)) = source {
        body["source_app"] = serde_json::json!(app_name);
        if let Some(window_title) = window_title {
            body["source_window_title"] = serde_json::json!(window_title);
        }
    }

    let result = match post_screen_endpoint(&app, "screen/capture", 20, body).await {
        Ok(response) => match parse_staged_capture(&response) {
            // deliver_capture re-summons the HUD and emits the seed event the
            // mounted ChatPanel re-seeds from.
            Ok(capture) => {
                deliver_capture(&app, capture).await;
                Ok(())
            },
            Err(error) => Err(format!("bad capture response: {error}")),
        },
        Err(error) => Err(format!("capture failed: {error}")),
    };

    if result.is_err() {
        // Never leave the HUD hidden because a capture failed.
        let _ = crate::overlay::show_overlay_window(&app);
    }
    result
}

#[tauri::command]
pub async fn cancel_screen_region_selection(app: AppHandle) -> Result<(), String> {
    hide_screen_region_picker_window(&app)?;
    info!("screen-region: picker cancelled");
    Ok(())
}

#[tauri::command]
pub async fn complete_screen_region_selection(
    app: AppHandle,
    selection: ScreenRegionSelection,
) -> Result<(), String> {
    let rect = selection_to_region_rect(&app, &selection)?;
    hide_screen_region_picker_window(&app)?;
    tokio::time::sleep(std::time::Duration::from_millis(140)).await;

    let mut body = serde_json::json!({
        "mode": "region",
        "region_rect": rect,
    });
    if let Some(source) = capture_source_context().as_object() {
        for (key, value) in source {
            body[key.clone()] = value.clone();
        }
    }
    match post_screen_endpoint(&app, "screen/capture", 20, body).await {
        Ok(body) => {
            if body.get("cancelled").and_then(|v| v.as_bool()) == Some(true) {
                info!("screen-region: backend cancelled rect capture");
                return Ok(());
            }
            let capture = parse_staged_capture(&body)
                .map_err(|error| format!("screen-region: bad capture response: {error}"))?;
            deliver_capture(&app, capture).await;
            info!(
                "screen-region: rect-aware capture staged x={} y={} width={} height={}",
                rect.x, rect.y, rect.width, rect.height
            );
            Ok(())
        },
        Err(error) => Err(format!("screen-region capture failed: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_staged_capture;

    fn capture_body() -> serde_json::Value {
        serde_json::json!({
            "capture_id": "cap1",
            "mode": "screenshot",
            "thread_id": "thread-screens",
            "session_id": "sess1",
            "session_title": "Screens — 2026-08-22",
            "attachments": [{
                "attachment_id": "att1",
                "stored_name": "screen.png",
                "mime_type": "image/png",
                "size_bytes": 12
            }]
        })
    }

    #[test]
    fn parse_reads_optional_capture_provenance() {
        let mut body = capture_body();
        body["source_app"] = serde_json::json!("Safari");
        body["source_window_title"] = serde_json::json!("Docs — Apple");

        let capture = parse_staged_capture(&body).expect("capture parses");
        assert_eq!(capture.source_app.as_deref(), Some("Safari"));
        assert_eq!(capture.source_window_title.as_deref(), Some("Docs — Apple"));
    }

    #[test]
    fn parse_without_provenance_keeps_capture_valid() {
        // The observe and clip flows never send source fields; the parse must
        // not require them, and empty strings collapse to None so chips never
        // render a dangling " — ".
        let capture = parse_staged_capture(&capture_body()).expect("capture parses");
        assert_eq!(capture.source_app, None);
        assert_eq!(capture.source_window_title, None);

        let mut body = capture_body();
        body["source_app"] = serde_json::json!("   ");
        let capture = parse_staged_capture(&body).expect("capture parses");
        assert_eq!(capture.source_app, None);
    }
}
