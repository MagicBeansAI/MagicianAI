//! Native window and runtime bridge for the notch orb.
//!
//! AppKit owns placement and focus behavior; the webview owns every pixel.  The
//! boundary is intentionally small: lifecycle snapshots, presentation modes,
//! captions, and audio envelopes cross it as Tauri events.

use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};
use tracing::{info, warn};

use crate::orb_state::{
    self, OrbAction, OrbAudioLevelPayload, OrbCaptionPayload, OrbEndedPayload, OrbEndedReason,
    OrbMachine, OrbPhase, OrbSnapshot, OrbState,
};

pub const ORB_WINDOW_LABEL: &str = "magician-notch-orb";
pub const ORB_PRESENTATION_EVENT: &str = "orb://presentation";
pub const PAUSE_ONE_HOUR_MS: u64 = 60 * 60 * 1_000;

const RESTING_MIN_WIDTH_PT: f64 = 214.0;
const EXPANDED_WIDTH_PT: f64 = 480.0;
const EXPANDED_HEIGHT_PT: f64 = 300.0;
const SPOTLIGHT_WIDTH_PT: f64 = 620.0;
const SPOTLIGHT_HEIGHT_PT: f64 = 500.0;
const WAKE_HOLD_MS: u64 = 1_250;
const SETTLE_DURATION_MS: u64 = 520;
const DOCK_SNAP_DISTANCE_PT: f64 = 56.0;
/// Tucked width: just the Orb, flush with the right bezel.
const EDGE_PEEK_WIDTH_PT: f64 = 46.0;
const EDGE_PILL_HEIGHT_PT: f64 = 52.0;
/// Notchless displays have no safe-area inset; the menu bar is still this tall.
const MENU_BAR_FALLBACK_PT: f64 = 25.0;
/// Extra gap so the tab sits under the menu bar instead of on its items.
const EDGE_BELOW_MENU_PT: f64 = 18.0;
const EDGE_SLIDE_MS: u64 = 220;
const INTRO_HOLD_MS: u64 = 3_200;
pub const ORB_INTRO_EVENT: &str = "orb://intro";

#[cfg(target_os = "macos")]
tauri_nspanel::tauri_panel! {
    panel!(MagicianOrbPanel {
        config: {
            can_become_key_window: true,
            can_become_main_window: false,
            is_floating_panel: true
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrbPresentation {
    Hidden,
    Resting,
    Expanded,
    Spotlight,
    Settling,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrbPresentationPayload {
    pub mode: OrbPresentation,
    pub duration_ms: u64,
    pub notch: bool,
    pub docked: bool,
    pub notch_width_points: f64,
    pub notch_height_points: f64,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct NotchMetrics {
    top_inset_points: f64,
    notch_width_points: f64,
    has_notch: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct OrbPlacement {
    x: f64,
    y: f64,
}

#[derive(Debug)]
pub struct OrbWindowRuntime {
    pub expanded: bool,
    pub registered_shortcut: Option<String>,
    shortcut_recording: bool,
    pub auto_expand_on_wake: bool,
    pub reduced_motion: bool,
    presentation: OrbPresentation,
    animation_generation: u64,
    notch: NotchMetrics,
    resting_placement: Option<OrbPlacement>,
    expanded_placement: Option<OrbPlacement>,
    last_programmatic_rect: Option<OrbRect>,
    move_revision: u64,
    /// Resting right-edge tab is widened to show the status line.
    edge_open: bool,
    /// Startup card is showing the live hotkeys before it settles away.
    intro_open: bool,
    intro_played: bool,
    intro_generation: u64,
    intro_hints: Vec<OrbHotkeyHint>,
}

impl Default for OrbWindowRuntime {
    fn default() -> Self {
        Self {
            expanded: false,
            registered_shortcut: None,
            shortcut_recording: false,
            auto_expand_on_wake: true,
            reduced_motion: false,
            presentation: OrbPresentation::Hidden,
            animation_generation: 0,
            notch: NotchMetrics::default(),
            resting_placement: None,
            expanded_placement: None,
            last_programmatic_rect: None,
            move_revision: 0,
            edge_open: false,
            intro_open: false,
            intro_played: false,
            intro_generation: 0,
            intro_hints: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrbHotkeyHint {
    pub keys: String,
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrbIntroPayload {
    pub visible: bool,
    pub hints: Vec<OrbHotkeyHint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OrbRect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

fn scaled(value: f64, scale: f64) -> u32 {
    (value * scale).round().max(1.0) as u32
}

/// Flush the window's right edge to the screen edge so the tab reads as a notch.
fn right_edge_origin_x(position_x: i32, monitor_width_px: u32, window_width_px: u32) -> i32 {
    position_x + monitor_width_px.saturating_sub(window_width_px) as i32
}

/// Sit just under the menu bar. The menu bar itself stays clickable.
fn resting_edge_y(position_y: i32, scale: f64, notch: NotchMetrics) -> i32 {
    let menu = if notch.top_inset_points > 1.0 {
        notch.top_inset_points
    } else {
        MENU_BAR_FALLBACK_PT
    };
    position_y + scaled(menu + EDGE_BELOW_MENU_PT, scale) as i32
}

fn monitor_for_orb(app: &AppHandle) -> Option<tauri::Monitor> {
    let monitors = app.available_monitors().ok()?;
    monitors
        .iter()
        .find(|monitor| {
            monitor
                .name()
                .is_some_and(|name| name.to_ascii_lowercase().contains("built-in"))
        })
        .cloned()
        .or_else(|| app.primary_monitor().ok().flatten())
        .or_else(|| monitors.into_iter().next())
}

fn rect_for(app: &AppHandle, presentation: OrbPresentation) -> Option<OrbRect> {
    let monitor = monitor_for_orb(app)?;
    let position = monitor.position();
    let size = monitor.size();
    let scale = monitor.scale_factor();
    let (notch, placement, edge_open, intro_open) = app
        .state::<crate::AppState>()
        .orb_window
        .lock()
        .map(|state| {
            let placement = placement_for_presentation(&state, presentation);
            let intro_open = state.intro_open
                && matches!(
                    presentation,
                    OrbPresentation::Hidden | OrbPresentation::Resting
                );
            (state.notch, placement, state.edge_open, intro_open)
        })
        .unwrap_or_default();

    Some(rect_for_monitor(
        position,
        size,
        scale,
        notch,
        presentation,
        if intro_open { None } else { placement },
        edge_open,
        intro_open,
    ))
}

fn rect_for_monitor(
    position: &PhysicalPosition<i32>,
    size: &PhysicalSize<u32>,
    scale: f64,
    notch: NotchMetrics,
    presentation: OrbPresentation,
    placement: Option<OrbPlacement>,
    edge_open: bool,
    intro: bool,
) -> OrbRect {
    let logical_monitor_width = size.width as f64 / scale;
    let logical_monitor_height = size.height as f64 / scale;
    let edge_home = matches!(
        presentation,
        OrbPresentation::Hidden | OrbPresentation::Resting
    ) && placement.is_none();
    let (width_pt, height_pt) = match presentation {
        // Tucked, only the Orb is on screen. Hover or an active turn slides
        // the status line out to the left. A dragged placement keeps the full pill.
        OrbPresentation::Hidden | OrbPresentation::Resting => {
            if intro {
                (SPOTLIGHT_WIDTH_PT, SPOTLIGHT_HEIGHT_PT)
            } else if edge_home && !edge_open {
                (EDGE_PEEK_WIDTH_PT, EDGE_PILL_HEIGHT_PT)
            } else {
                (RESTING_MIN_WIDTH_PT, EDGE_PILL_HEIGHT_PT)
            }
        },
        OrbPresentation::Expanded | OrbPresentation::Settling => {
            (EXPANDED_WIDTH_PT, EXPANDED_HEIGHT_PT)
        },
        OrbPresentation::Spotlight => (SPOTLIGHT_WIDTH_PT, SPOTLIGHT_HEIGHT_PT),
    };
    let width = scaled(width_pt.min(logical_monitor_width - 24.0), scale);
    let height = scaled(height_pt.min(logical_monitor_height - 24.0), scale);

    let centered_x = position.x + ((size.width.saturating_sub(width)) / 2) as i32;
    let centered_y = position.y + ((size.height.saturating_sub(height)) / 2) as i32;
    let default_x = if intro {
        centered_x
    } else {
        match presentation {
            OrbPresentation::Hidden | OrbPresentation::Resting => {
                right_edge_origin_x(position.x, size.width, width)
            },
            OrbPresentation::Expanded | OrbPresentation::Settling | OrbPresentation::Spotlight => {
                centered_x
            },
        }
    };
    let default_y = if intro {
        centered_y
    } else {
        match presentation {
            OrbPresentation::Spotlight => centered_y,
            // The resting tab is below the menu bar, flush to the right edge.
            // Expanded still grows at the top center, and Spotlight is the
            // center-screen arrival. A saved placement replaces this home.
            OrbPresentation::Hidden | OrbPresentation::Resting => {
                resting_edge_y(position.y, scale, notch)
            },
            OrbPresentation::Expanded | OrbPresentation::Settling => position.y,
        }
    };
    let (x, y) = placement
        .map(|placement| {
            let available_width = size.width.saturating_sub(width) as f64;
            let available_height = size.height.saturating_sub(height) as f64;
            (
                position.x + (available_width * placement.x.clamp(0.0, 1.0)).round() as i32,
                position.y + (available_height * placement.y.clamp(0.0, 1.0)).round() as i32,
            )
        })
        .unwrap_or((default_x, default_y));
    OrbRect {
        x,
        y,
        width,
        height,
    }
}

fn placement_for_presentation(
    runtime: &OrbWindowRuntime,
    presentation: OrbPresentation,
) -> Option<OrbPlacement> {
    match presentation {
        OrbPresentation::Hidden | OrbPresentation::Resting | OrbPresentation::Settling => {
            runtime.resting_placement
        },
        OrbPresentation::Expanded => runtime.expanded_placement,
        OrbPresentation::Spotlight => None,
    }
}

fn presentation_is_docked(runtime: &OrbWindowRuntime, presentation: OrbPresentation) -> bool {
    !matches!(presentation, OrbPresentation::Spotlight)
        && placement_for_presentation(runtime, presentation).is_none()
}

fn position_is_near_dock(position: &PhysicalPosition<i32>, dock: OrbRect, scale: f64) -> bool {
    let threshold = scaled(DOCK_SNAP_DISTANCE_PT, scale) as i32;
    (position.x - dock.x).abs() <= threshold && (position.y - dock.y).abs() <= threshold
}

fn notch_metrics_from_visible_top_areas(
    top_inset_points: f64,
    screen_width_points: f64,
    left_visible_width_points: f64,
    right_visible_width_points: f64,
) -> NotchMetrics {
    let obscured_width =
        (screen_width_points - left_visible_width_points - right_visible_width_points).max(0.0);
    let has_notch = top_inset_points > 0.5
        && left_visible_width_points > 0.0
        && right_visible_width_points > 0.0
        && obscured_width > 24.0
        && obscured_width < screen_width_points;
    NotchMetrics {
        top_inset_points: has_notch.then_some(top_inset_points).unwrap_or(0.0),
        notch_width_points: has_notch.then_some(obscured_width).unwrap_or(0.0),
        has_notch,
    }
}

#[cfg(target_os = "macos")]
fn native_notch_metrics() -> NotchMetrics {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSScreen;

    // `tauri-nspanel` owns all NSPanel behavior. This direct AppKit query is the
    // one deliberate gap: Tauri and the plugin do not expose NSScreen's
    // safe-area / auxiliary top areas, which are the authoritative notch shape.
    let Some(mtm) = MainThreadMarker::new() else {
        return NotchMetrics::default();
    };
    let screens = NSScreen::screens(mtm);
    let mut fallback = None;
    for screen in screens.iter() {
        let safe = screen.safeAreaInsets();
        let frame = screen.frame();
        let left = screen.auxiliaryTopLeftArea();
        let right = screen.auxiliaryTopRightArea();
        let metrics = notch_metrics_from_visible_top_areas(
            safe.top,
            frame.size.width,
            left.size.width,
            right.size.width,
        );
        let name = screen.localizedName().to_string().to_ascii_lowercase();
        if name.contains("built-in") {
            return metrics;
        }
        if fallback.is_none() && metrics.has_notch {
            fallback = Some(metrics);
        }
    }
    fallback.unwrap_or_default()
}

#[cfg(not(target_os = "macos"))]
fn native_notch_metrics() -> NotchMetrics {
    NotchMetrics::default()
}

fn set_presentation_state(app: &AppHandle, presentation: OrbPresentation) -> u64 {
    let mut generation = 0;
    let mut notch = NotchMetrics::default();
    let mut docked = false;
    if let Ok(mut runtime) = app.state::<crate::AppState>().orb_window.lock() {
        runtime.presentation = presentation;
        runtime.expanded = matches!(
            presentation,
            OrbPresentation::Expanded | OrbPresentation::Spotlight | OrbPresentation::Settling
        );
        runtime.animation_generation = runtime.animation_generation.saturating_add(1);
        generation = runtime.animation_generation;
        notch = runtime.notch;
        docked = presentation_is_docked(&runtime, presentation);
    }
    let _ = app.emit(
        ORB_PRESENTATION_EVENT,
        OrbPresentationPayload {
            mode: presentation,
            duration_ms: match presentation {
                OrbPresentation::Settling => SETTLE_DURATION_MS,
                OrbPresentation::Spotlight => WAKE_HOLD_MS,
                _ => 260,
            },
            notch: notch.has_notch,
            docked,
            notch_width_points: notch.notch_width_points,
            notch_height_points: notch.top_inset_points,
            revision: generation,
        },
    );
    generation
}

fn animation_is_current(app: &AppHandle, generation: u64) -> bool {
    app.state::<crate::AppState>()
        .orb_window
        .lock()
        .map(|runtime| runtime.animation_generation == generation)
        .unwrap_or(false)
}

fn remember_programmatic_rect(app: &AppHandle, rect: OrbRect) {
    if let Ok(mut runtime) = app.state::<crate::AppState>().orb_window.lock() {
        runtime.last_programmatic_rect = Some(rect);
    }
}

fn apply_rect(app: &AppHandle, window: &tauri::WebviewWindow, rect: OrbRect) {
    remember_programmatic_rect(app, rect);
    let _ = window.set_position(PhysicalPosition::new(rect.x, rect.y));
    let _ = window.set_size(PhysicalSize::new(rect.width, rect.height));
}

fn configure_interaction(window: &tauri::WebviewWindow, interactive: bool) {
    let _ = window.set_ignore_cursor_events(!interactive);
    configure_focusability(window, interactive);
}

fn configure_interaction_before_show(window: &tauri::WebviewWindow, interactive: bool) {
    #[cfg(target_os = "macos")]
    configure_interaction(window, interactive);

    #[cfg(not(target_os = "macos"))]
    configure_focusability(window, interactive);
}

#[cfg(target_os = "macos")]
fn configure_focusability(_window: &tauri::WebviewWindow, _interactive: bool) {
    // `tauri-nspanel` changes the native window's Objective-C class from Tao's
    // window subclass to `MagicianOrbPanel`. Tao's `set_focusable` reaches into
    // a private `focusable` ivar on its original class, so calling it after the
    // class swap aborts inside AppKit. The panel's `canBecomeKeyWindow` selector,
    // non-activating style, and explicit click-through state own this behavior.
}

#[cfg(not(target_os = "macos"))]
fn configure_focusability(window: &tauri::WebviewWindow, interactive: bool) {
    let _ = window.set_focusable(interactive);
}

fn ease_out_quint(value: f64) -> f64 {
    1.0 - (1.0 - value).powi(5)
}

fn animate_to(
    app: AppHandle,
    target: OrbRect,
    generation: u64,
    focus: bool,
    terminal_presentation: Option<OrbPresentation>,
    duration_ms: u64,
) {
    let Some(window) = app.get_webview_window(ORB_WINDOW_LABEL) else {
        return;
    };
    let start_position = window
        .outer_position()
        .unwrap_or(PhysicalPosition::new(target.x, target.y));
    let start_size = window
        .inner_size()
        .unwrap_or(PhysicalSize::new(target.width, target.height));
    remember_programmatic_rect(&app, target);
    tauri::async_runtime::spawn(async move {
        const STEPS: u64 = 28;
        for step in 1..=STEPS {
            if !animation_is_current(&app, generation) {
                return;
            }
            let t = ease_out_quint(step as f64 / STEPS as f64);
            let x = start_position.x as f64 + (target.x - start_position.x) as f64 * t;
            let y = start_position.y as f64 + (target.y - start_position.y) as f64 * t;
            let width =
                start_size.width as f64 + (target.width as f64 - start_size.width as f64) * t;
            let height =
                start_size.height as f64 + (target.height as f64 - start_size.height as f64) * t;
            let _ = window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
            let _ = window.set_size(PhysicalSize::new(
                width.round().max(1.0) as u32,
                height.round().max(1.0) as u32,
            ));
            tokio::time::sleep(Duration::from_millis(duration_ms.max(STEPS) / STEPS)).await;
        }
        if !animation_is_current(&app, generation) {
            return;
        }
        apply_rect(&app, &window, target);
        if let Some(presentation) = terminal_presentation {
            set_presentation_state(&app, presentation);
        }
        if focus {
            let _ = window.set_focus();
        }
    });
}

fn placement_from_config(x: Option<f64>, y: Option<f64>) -> Option<OrbPlacement> {
    match (x, y) {
        (Some(x), Some(y)) if x.is_finite() && y.is_finite() => Some(OrbPlacement {
            x: x.clamp(0.0, 1.0),
            y: y.clamp(0.0, 1.0),
        }),
        _ => None,
    }
}

fn schedule_orb_placement_save(app: &AppHandle) {
    let revision = {
        let app_state = app.state::<crate::AppState>();
        let Ok(mut runtime) = app_state.orb_window.lock() else {
            return;
        };
        runtime.move_revision = runtime.move_revision.saturating_add(1);
        runtime.move_revision
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(350)).await;
        let (presentation, programmatic_rect, notch) = {
            let app_state = app.state::<crate::AppState>();
            let Ok(runtime) = app_state.orb_window.lock() else {
                return;
            };
            if runtime.move_revision != revision || runtime.intro_open {
                return;
            }
            (
                runtime.presentation,
                runtime.last_programmatic_rect,
                runtime.notch,
            )
        };
        if !matches!(
            presentation,
            OrbPresentation::Resting | OrbPresentation::Expanded
        ) {
            return;
        }
        let Some(window) = app.get_webview_window(ORB_WINDOW_LABEL) else {
            return;
        };
        let Ok(position) = window.outer_position() else {
            return;
        };
        let Ok(size) = window.inner_size() else {
            return;
        };
        if programmatic_rect.is_some_and(|rect| rect.x == position.x && rect.y == position.y) {
            return;
        }
        let monitor = window
            .current_monitor()
            .ok()
            .flatten()
            .or_else(|| monitor_for_orb(&app));
        let Some(monitor) = monitor else {
            return;
        };
        let monitor_position = monitor.position();
        let monitor_size = monitor.size();
        let monitor_scale = monitor.scale_factor();
        let dock_rect = rect_for_monitor(
            monitor_position,
            monitor_size,
            monitor_scale,
            notch,
            presentation,
            None,
            false,
            false,
        );
        let docked = position_is_near_dock(&position, dock_rect, monitor_scale);
        // A tucked tab is only the Orb. Once it is parked somewhere else, grow
        // it to the full pill and keep the edge the user was holding.
        let (parked_x, parked_y, parked_width, parked_height) =
            if !docked && matches!(presentation, OrbPresentation::Resting) {
                let full_width = scaled(RESTING_MIN_WIDTH_PT, monitor_scale);
                let full_height = scaled(EDGE_PILL_HEIGHT_PT, monitor_scale);
                let right = position.x + size.width as i32;
                (
                    right - full_width as i32,
                    position.y,
                    full_width,
                    full_height,
                )
            } else {
                (position.x, position.y, size.width, size.height)
            };
        let available_width = monitor_size.width.saturating_sub(parked_width).max(1) as f64;
        let available_height = monitor_size.height.saturating_sub(parked_height).max(1) as f64;
        let placement = (!docked).then_some(OrbPlacement {
            x: ((parked_x - monitor_position.x) as f64 / available_width).clamp(0.0, 1.0),
            y: ((parked_y - monitor_position.y) as f64 / available_height).clamp(0.0, 1.0),
        });
        {
            let app_state = app.state::<crate::AppState>();
            let Ok(mut runtime) = app_state.orb_window.lock() else {
                return;
            };
            if runtime.move_revision != revision {
                return;
            }
            match presentation {
                OrbPresentation::Resting => runtime.resting_placement = placement,
                OrbPresentation::Expanded => runtime.expanded_placement = placement,
                _ => return,
            }
            runtime.last_programmatic_rect = Some(OrbRect {
                x: parked_x,
                y: parked_y,
                width: parked_width,
                height: parked_height,
            });
        }
        let config_to_save = {
            let state = app.state::<crate::AppState>();
            let mut config = state.config.lock().await;
            match presentation {
                OrbPresentation::Resting => {
                    config.orb.resting_x = placement.map(|value| value.x);
                    config.orb.resting_y = placement.map(|value| value.y);
                },
                OrbPresentation::Expanded => {
                    config.orb.expanded_x = placement.map(|value| value.x);
                    config.orb.expanded_y = placement.map(|value| value.y);
                },
                _ => return,
            }
            config.clone()
        };
        if docked {
            apply_rect(&app, &window, dock_rect);
        } else if parked_width != size.width || parked_x != position.x {
            apply_rect(
                &app,
                &window,
                OrbRect {
                    x: parked_x,
                    y: parked_y,
                    width: parked_width,
                    height: parked_height,
                },
            );
        }
        // Moving away changes the renderer from a notch-owned silhouette to
        // the transparent floating treatment; snapping back does the inverse.
        set_presentation_state(&app, presentation);
        if let Err(error) = crate::config::save_config(&config_to_save) {
            warn!(error = %error, "Failed to persist Ambient Orb position");
        } else {
            info!(
                ?presentation,
                docked,
                x = placement.map(|value| value.x),
                y = placement.map(|value| value.y),
                "Persisted Ambient Orb position"
            );
        }
    });
}

pub fn initialize_orb_window(app: &AppHandle) -> Result<(), String> {
    if app.get_webview_window(ORB_WINDOW_LABEL).is_some() {
        return Ok(());
    }
    if let Ok(mut runtime) = app.state::<crate::AppState>().orb_window.lock() {
        runtime.notch = native_notch_metrics();
    }
    let initial = rect_for(app, OrbPresentation::Resting).ok_or("no display available for orb")?;

    #[cfg(target_os = "macos")]
    {
        use tauri::{LogicalPosition, LogicalSize, Position, Size};
        use tauri_nspanel::{CollectionBehavior, PanelBuilder, PanelLevel, StyleMask};

        let scale = monitor_for_orb(app)
            .map(|m| m.scale_factor())
            .unwrap_or(1.0);
        let panel = PanelBuilder::<_, MagicianOrbPanel>::new(app, ORB_WINDOW_LABEL)
            .url(crate::tray::desktop_app_webview_url("/orb"))
            .title("Magican Orb")
            .position(Position::Logical(LogicalPosition::new(
                initial.x as f64 / scale,
                initial.y as f64 / scale,
            )))
            .size(Size::Logical(LogicalSize::new(
                initial.width as f64 / scale,
                initial.height as f64 / scale,
            )))
            .level(PanelLevel::Status)
            .floating(true)
            .no_activate(true)
            .transparent(true)
            .opaque(false)
            .has_shadow(false)
            .hides_on_deactivate(false)
            .becomes_key_only_if_needed(true)
            .ignores_mouse_events(true)
            .released_when_closed(false)
            .style_mask(StyleMask::empty().borderless().nonactivating_panel())
            .collection_behavior(
                CollectionBehavior::new()
                    .can_join_all_spaces()
                    .full_screen_auxiliary()
                    .stationary()
                    .ignores_cycle(),
            )
            .with_window(|window| {
                window
                    .decorations(false)
                    .resizable(false)
                    .always_on_top(true)
                    .skip_taskbar(true)
                    .visible(false)
                    .transparent(true)
                    .shadow(false)
                    .focused(false)
            })
            .build()
            .map_err(|error| format!("failed to create notch orb panel: {error}"))?;
        panel.hide();
    }

    #[cfg(not(target_os = "macos"))]
    {
        tauri::WebviewWindowBuilder::new(
            app,
            ORB_WINDOW_LABEL,
            crate::tray::desktop_app_webview_url("/orb"),
        )
        .title("Magican Orb")
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .transparent(true)
        .shadow(false)
        .inner_size(RESTING_MIN_WIDTH_PT, EDGE_PILL_HEIGHT_PT)
        .build()
        .map_err(|error| format!("failed to create orb window: {error}"))?;
    }

    if let Some(window) = app.get_webview_window(ORB_WINDOW_LABEL) {
        // Tao's Linux backend cannot apply an input shape until GTK has
        // realized the native window. The orb starts hidden, so defer Linux
        // click-through until the first show request; focusability is safe to
        // seed while hidden.
        configure_interaction_before_show(&window, false);
        let close_app = app.clone();
        window.on_window_event(move |event| match event {
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                collapse(&close_app);
            },
            WindowEvent::Moved(_) => schedule_orb_placement_save(&close_app),
            _ => {},
        });
    }
    Ok(())
}

fn tuck_edge(app: &AppHandle) {
    if let Ok(mut runtime) = app.state::<crate::AppState>().orb_window.lock() {
        runtime.edge_open = false;
        runtime.intro_open = false;
    }
}

fn show_at(app: &AppHandle, presentation: OrbPresentation, focus: bool) {
    if setup_blocks_orb(app) {
        hide(app);
        return;
    }
    if matches!(
        presentation,
        OrbPresentation::Hidden | OrbPresentation::Resting
    ) {
        tuck_edge(app);
    }
    if let Err(error) = initialize_orb_window(app) {
        warn!("Cannot show notch orb: {error}");
        return;
    }
    let Some(window) = app.get_webview_window(ORB_WINDOW_LABEL) else {
        return;
    };
    let Some(rect) = rect_for(app, presentation) else {
        return;
    };
    let interactive = !matches!(presentation, OrbPresentation::Hidden);
    configure_interaction_before_show(&window, interactive);
    let generation = set_presentation_state(app, presentation);
    apply_rect(app, &window, rect);
    let _ = window.show();
    let _ = window.unminimize();
    if focus {
        let _ = window.set_focus();
    }
    // Re-assert interaction after showing; WebKit can reset the native panel's
    // mouse-event policy while attaching the view on first presentation.
    configure_interaction(&window, interactive);
    info!(?presentation, generation, "Presented notch orb");
}

fn animate_at(app: &AppHandle, presentation: OrbPresentation, focus: bool) {
    if setup_blocks_orb(app) {
        hide(app);
        return;
    }
    if let Err(error) = initialize_orb_window(app) {
        warn!("Cannot animate notch orb: {error}");
        return;
    }
    let Some(window) = app.get_webview_window(ORB_WINDOW_LABEL) else {
        return;
    };
    let Some(target) = rect_for(app, presentation) else {
        return;
    };
    configure_interaction_before_show(&window, true);
    let generation = set_presentation_state(app, presentation);
    let _ = window.show();
    let _ = window.unminimize();
    configure_interaction(&window, true);
    animate_to(
        app.clone(),
        target,
        generation,
        focus,
        None,
        SETTLE_DURATION_MS,
    );
}

fn settle_to_resting(app: &AppHandle, focus: bool) {
    tuck_edge(app);
    let reduced_motion = app
        .state::<crate::AppState>()
        .orb_window
        .lock()
        .map(|runtime| runtime.reduced_motion)
        .unwrap_or(false);
    if reduced_motion {
        show_at(app, OrbPresentation::Resting, focus);
        return;
    }
    let Some(window) = app.get_webview_window(ORB_WINDOW_LABEL) else {
        show_resting(app);
        return;
    };
    let Some(target) = rect_for(app, OrbPresentation::Resting) else {
        return;
    };
    configure_interaction(&window, true);
    let generation = set_presentation_state(app, OrbPresentation::Settling);
    animate_to(
        app.clone(),
        target,
        generation,
        focus,
        Some(OrbPresentation::Resting),
        SETTLE_DURATION_MS,
    );
}

pub fn show_resting(app: &AppHandle) {
    show_at(app, OrbPresentation::Resting, false);
}

pub fn expand(app: &AppHandle) {
    animate_at(app, OrbPresentation::Expanded, true);
}

fn expand_without_focus(app: &AppHandle) {
    show_at(app, OrbPresentation::Expanded, false);
}

pub fn collapse(app: &AppHandle) {
    settle_to_resting(app, false);
}

pub fn spotlight(app: &AppHandle) {
    animate_at(app, OrbPresentation::Spotlight, true);
}

pub fn toggle(app: &AppHandle) {
    if setup_blocks_orb(app) {
        hide(app);
        return;
    }
    let expanded = app
        .state::<crate::AppState>()
        .orb_window
        .lock()
        .map(|runtime| runtime.expanded)
        .unwrap_or(false);
    if expanded {
        collapse(app);
    } else {
        expand(app);
    }
}

pub fn hide(app: &AppHandle) {
    set_presentation_state(app, OrbPresentation::Hidden);
    if let Some(window) = app.get_webview_window(ORB_WINDOW_LABEL) {
        let _ = window.hide();
    }
}

fn setup_blocks_orb(app: &AppHandle) -> bool {
    app.state::<crate::AppState>()
        .orb_setup_blocked
        .load(Ordering::Acquire)
}

/// Hide every Orb surface while guided setup owns the desktop. The persisted
/// Orb preference is left untouched so completing setup can restore the exact
/// behavior the user selected previously.
pub fn block_for_setup(app: &AppHandle) {
    app.state::<crate::AppState>()
        .orb_setup_blocked
        .store(true, Ordering::Release);
    crate::voice_wake::suspend_native_wake(app);
    crate::voice_note::request_end_orb_conversation(app.clone());
    hide(app);
}

/// Release the transient setup gate and apply the persisted Orb settings.
pub fn release_setup_gate(app: &AppHandle, config: &crate::config::OrbConfig) {
    app.state::<crate::AppState>()
        .orb_setup_blocked
        .store(false, Ordering::Release);
    arm_from_config(app, config);
    play_startup_intro(app);
}

/// One card per process, after setup has released the orb. It shows the
/// shortcuts as they are configured, then settles to the right-edge tab.
/// A resting orb hides completely once that settle finishes.
fn play_startup_intro(app: &AppHandle) {
    if setup_blocks_orb(app) {
        return;
    }
    let state = app.state::<crate::AppState>();
    let mut runtime = match state.orb_window.lock() {
        Ok(runtime) => runtime,
        Err(_) => return,
    };
    if runtime.intro_played {
        return;
    }
    runtime.intro_played = true;
    let config_state = app.state::<crate::AppState>();
    let Some(config) = config_state.config.try_lock().ok() else {
        runtime.intro_played = false;
        drop(runtime);
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let state = app.state::<crate::AppState>();
            let _config = state.config.lock().await;
            drop(_config);
            play_startup_intro(&app);
        });
        return;
    };
    let hints = startup_hotkey_hints(
        &config.general.quick_overlay_gesture,
        &config.orb.hotkey,
        config.orb.wake_enabled,
        config
            .orb
            .wake_phrases
            .first()
            .map(String::as_str)
            .unwrap_or(""),
    );
    if hints.is_empty() {
        return;
    }
    runtime.intro_hints = hints;
    runtime.intro_generation = runtime.intro_generation.saturating_add(1);
    runtime.intro_open = true;
    runtime.edge_open = true;
    drop(runtime);
    drop(config);
    show_intro_frame(app);
    emit_intro(app, true);
    let generation = app
        .state::<crate::AppState>()
        .orb_window
        .lock()
        .map(|runtime| runtime.intro_generation)
        .unwrap_or(0);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(INTRO_HOLD_MS)).await;
        let still = app
            .state::<crate::AppState>()
            .orb_window
            .lock()
            .map(|runtime| runtime.intro_generation == generation && runtime.intro_open)
            .unwrap_or(false);
        if !still {
            return;
        }
        let stay = app
            .state::<crate::AppState>()
            .config
            .lock()
            .await
            .orb
            .enabled;
        if let Ok(mut runtime) = app.state::<crate::AppState>().orb_window.lock() {
            runtime.intro_open = false;
            runtime.edge_open = false;
        }
        emit_intro(&app, false);
        if stay && !setup_blocks_orb(&app) {
            settle_to_resting(&app, false);
        } else {
            hide(&app);
        }
    });
}

fn show_intro_frame(app: &AppHandle) {
    if let Err(error) = initialize_orb_window(app) {
        warn!("Cannot show the orb startup card: {error}");
        return;
    }
    let Some(window) = app.get_webview_window(ORB_WINDOW_LABEL) else {
        return;
    };
    let Some(rect) = rect_for(app, OrbPresentation::Resting) else {
        return;
    };
    configure_interaction_before_show(&window, true);
    let _generation = set_presentation_state(app, OrbPresentation::Resting);
    apply_rect(app, &window, rect);
    let _ = window.show();
    let _ = window.unminimize();
    configure_interaction(&window, true);
}

fn emit_intro(app: &AppHandle, visible: bool) {
    let hints = app
        .state::<crate::AppState>()
        .orb_window
        .lock()
        .map(|runtime| runtime.intro_hints.clone())
        .unwrap_or_default();
    let _ = app.emit(ORB_INTRO_EVENT, OrbIntroPayload { visible, hints });
}

fn startup_hotkey_hints(
    gesture: &str,
    orb_hotkey: &str,
    wake_enabled: bool,
    wake_phrase: &str,
) -> Vec<OrbHotkeyHint> {
    let mut hints = Vec::new();
    if let Some(key) = crate::voice_gesture::overlay_modifier_label(gesture) {
        hints.push(OrbHotkeyHint {
            keys: format!("Hold {key}"),
            action: "Talk".to_string(),
        });
        hints.push(OrbHotkeyHint {
            keys: format!("Double-tap {key}"),
            action: "Quick Automate".to_string(),
        });
    }
    if let Some(keys) = display_shortcut(orb_hotkey) {
        hints.push(OrbHotkeyHint {
            keys,
            action: "Show the Orb".to_string(),
        });
    }
    let phrase = wake_phrase.trim();
    if wake_enabled && !phrase.is_empty() {
        hints.push(OrbHotkeyHint {
            keys: format!("Say “{phrase}”"),
            action: "Wake".to_string(),
        });
    }
    hints
}

/// Turn a stored chord such as `Alt+Space` into the symbols people see on the keys.
fn display_shortcut(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty()
        || raw.eq_ignore_ascii_case("none")
        || raw.eq_ignore_ascii_case("disabled")
        || raw.eq_ignore_ascii_case("off")
    {
        return None;
    }
    let parts: Vec<String> = raw
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| match part.to_ascii_lowercase().as_str() {
            "alt" | "option" | "opt" => "⌥".to_string(),
            "cmd" | "command" | "meta" => "⌘".to_string(),
            "shift" => "⇧".to_string(),
            "ctrl" | "control" => "⌃".to_string(),
            "space" => "Space".to_string(),
            _ => part.to_string(),
        })
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

#[tauri::command]
pub fn get_orb_intro(app: AppHandle) -> Result<OrbIntroPayload, String> {
    let state = app.state::<crate::AppState>();
    let runtime = state
        .orb_window
        .lock()
        .map_err(|_| "orb window state is unavailable".to_string())?;
    Ok(OrbIntroPayload {
        visible: runtime.intro_open,
        hints: runtime.intro_hints.clone(),
    })
}

pub fn present_wake(app: &AppHandle) {
    let reduced_motion = app
        .state::<crate::AppState>()
        .orb_window
        .lock()
        .map(|runtime| runtime.reduced_motion)
        .unwrap_or(false);
    if reduced_motion {
        // Preserve the truthful phase change without manufacturing a large
        // moving surface for users who requested reduced motion.
        show_at(app, OrbPresentation::Resting, false);
        return;
    }
    // A wake may be visually prominent, but it must not steal the user's
    // keyboard focus from the app they are working in.
    animate_at(app, OrbPresentation::Spotlight, false);
    let generation = app
        .state::<crate::AppState>()
        .orb_window
        .lock()
        .map(|runtime| runtime.animation_generation)
        .unwrap_or(0);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(WAKE_HOLD_MS)).await;
        if !animation_is_current(&app, generation) {
            return;
        }
        settle_to_resting(&app, false);
    });
}

pub fn current_snapshot(app: &AppHandle) -> OrbSnapshot {
    app.state::<crate::AppState>()
        .orb
        .lock()
        .map(|machine| machine.snapshot())
        .unwrap_or_else(|_| OrbMachine::default().snapshot())
}

pub fn dispatch(app: &AppHandle, action: OrbAction) -> OrbSnapshot {
    let now_ms = orb_state::epoch_ms();
    let (before, after, changed) = {
        let state = app.state::<crate::AppState>();
        let mut machine = state.orb.lock().expect("orb state lock poisoned");
        // Read while holding the lifecycle lock. External lease teardown sets
        // the atomic first and then dispatches its release, so either this read
        // sees no lease or that release waits and clears the busy projection.
        let external_voice_active = state.external_voice_owner.load(Ordering::Acquire) != 0;
        let before = machine.snapshot();
        let changed = machine.apply_with_external_voice(action, now_ms, external_voice_active);
        let after = machine.snapshot();
        (before, after, changed)
    };
    if !changed {
        return after;
    }

    let _ = app.emit(orb_state::ORB_PHASE_EVENT, after.clone());
    if let Some(window) = app.get_webview_window(ORB_WINDOW_LABEL) {
        let _ = window.set_title(&format!("Magican — {}", after.status));
    }
    react_to_transition(app, &before, &after);
    sync_keep_awake(app, &after);
    crate::tray::refresh_menu(app, None);
    after
}

fn react_to_transition(app: &AppHandle, before: &OrbSnapshot, after: &OrbSnapshot) {
    if setup_blocks_orb(app) {
        crate::voice_wake::suspend_native_wake(app);
        hide(app);
        return;
    }
    match after.phase {
        None => hide(app),
        Some(OrbPhase::Armed) if after.state == "recoverable_error" => expand_without_focus(app),
        Some(OrbPhase::Armed) if after.state == "hold_ready" => {
            // The microphone is closed. Stay at the side home and do not
            // reopen the wake detector over a parked Live socket.
            let presentation = app
                .state::<crate::AppState>()
                .orb_window
                .lock()
                .map(|runtime| runtime.presentation)
                .unwrap_or(OrbPresentation::Hidden);
            if !matches!(presentation, OrbPresentation::Resting) {
                show_resting(app);
            }
        },
        Some(OrbPhase::Armed) => {
            show_resting(app);
            if before.state != after.state
                && !app
                    .state::<crate::AppState>()
                    .orb_wake_handoff
                    .load(Ordering::Acquire)
            {
                crate::voice_wake::resume_native_wake(app);
            }
        },
        Some(OrbPhase::Heard) => {
            if before.phase != after.phase {
                crate::voice_wake::suspend_native_wake(app);
                let auto_expand = app
                    .state::<crate::AppState>()
                    .orb_window
                    .lock()
                    .map(|runtime| runtime.auto_expand_on_wake)
                    .unwrap_or(true);
                if auto_expand {
                    present_wake(app);
                } else {
                    show_resting(app);
                }
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    dispatch(&app, OrbAction::Connecting);
                    if let Err(error) = crate::voice_note::begin_orb_conversation(app.clone()).await
                    {
                        warn!("Orb conversation failed to start: {error}");
                        if current_snapshot(&app).state != "connecting" {
                            return;
                        }
                        if error.contains("already active") {
                            dispatch(&app, OrbAction::ExternalVoiceStarted);
                            return;
                        }
                        let reason = if error.to_ascii_lowercase().contains("microphone")
                            || error.to_ascii_lowercase().contains("muted")
                        {
                            OrbEndedReason::MicrophoneLost
                        } else {
                            OrbEndedReason::SessionFailed
                        };
                        dispatch(&app, OrbAction::Disarm { reason });
                    }
                });
            }
        },
        Some(OrbPhase::Listening | OrbPhase::Thinking | OrbPhase::Speaking) => {
            if after.state == "cooldown" && before.state != "cooldown" {
                // The follow-up window is real listening time, not a decorative
                // cooldown: re-open the wake detector as soon as realtime audio
                // releases the microphone.
                crate::voice_wake::resume_native_wake(app);
            }
            let (presentation, auto_expand) = app
                .state::<crate::AppState>()
                .orb_window
                .lock()
                .map(|runtime| (runtime.presentation, runtime.auto_expand_on_wake))
                .unwrap_or((OrbPresentation::Hidden, true));
            if auto_expand
                && matches!(
                    presentation,
                    OrbPresentation::Hidden | OrbPresentation::Resting
                )
            {
                expand_without_focus(app);
            }
        },
        Some(OrbPhase::Ended) if matches!(after.state, "paused" | "voice_busy") => {
            show_resting(app)
        },
        Some(OrbPhase::Ended) => expand_without_focus(app),
    }

    if after.state == "disarming" && before.state != "disarming" {
        crate::voice_wake::suspend_native_wake(app);
        crate::voice_note::request_end_orb_conversation(app.clone());
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(760)).await;
            dispatch(&app, OrbAction::CompleteDisarm);
        });
    }
    if after.state == "ended" && before.state != "ended" {
        let reason = app
            .state::<crate::AppState>()
            .orb
            .lock()
            .ok()
            .and_then(|machine| match machine.state() {
                OrbState::Ended { reason } => Some(reason.clone()),
                _ => None,
            })
            .unwrap_or(OrbEndedReason::SessionFailed);
        // A user disarm or bounded listening cap hides the visible Orb, not the
        // independently enabled wake spotter. Re-open that cold-start boundary
        // only for intentional/normal endings; microphone and session failures
        // stay stopped so they cannot create a restart loop.
        if matches!(
            &reason,
            OrbEndedReason::UserDisarm | OrbEndedReason::CapReached
        ) && app
            .state::<crate::AppState>()
            .orb_wake_enabled
            .load(Ordering::Acquire)
            && !app
                .state::<crate::AppState>()
                .orb_wake_handoff
                .load(Ordering::Acquire)
        {
            crate::voice_wake::resume_native_wake(app);
        }
        let _ = app.emit(
            orb_state::ORB_ENDED_EVENT,
            OrbEndedPayload {
                message: reason.message(),
                reason,
            },
        );
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1_350)).await;
            if current_snapshot(&app).state == "ended" {
                hide(&app);
            }
        });
    }
}

fn sync_keep_awake(app: &AppHandle, snapshot: &OrbSnapshot) {
    let should_hold = snapshot.window_open;
    let state = app.state::<crate::AppState>();
    let Ok(mut assertion) = state.orb_keep_awake.lock() else {
        return;
    };
    if should_hold && assertion.is_none() {
        // Keep CPU + mic work alive through idle/lock without forcing the display
        // to remain lit. loginwindow still owns lock-screen presentation.
        *assertion = keepawake::Builder::default()
            .display(false)
            .idle(true)
            .sleep(true)
            .create()
            .ok();
    } else if !should_hold {
        *assertion = None;
    }
}

pub fn emit_caption(
    app: &AppHandle,
    role: &'static str,
    speaker_name: &str,
    text: &str,
    final_caption: bool,
) {
    let text = text.trim();
    let speaker_name = speaker_name.trim();
    if text.is_empty() || speaker_name.is_empty() {
        return;
    }
    let _ = app.emit(
        orb_state::ORB_CAPTION_EVENT,
        OrbCaptionPayload {
            role,
            speaker_name: speaker_name.to_string(),
            text: text.to_string(),
            final_caption,
        },
    );
}

pub fn clear_unfinished_caption(app: &AppHandle, role: &'static str) {
    let _ = app.emit(orb_state::ORB_CAPTION_CLEAR_EVENT, role);
}

fn emit_audio_level(app: &AppHandle, channel: &'static str, level: f32) {
    let _ = app.emit(
        orb_state::ORB_AUDIO_LEVEL_EVENT,
        OrbAudioLevelPayload {
            channel,
            level: level.clamp(0.0, 1.0),
        },
    );
}

/// Publish an audio envelope without touching the webview. Realtime producers
/// only perform an atomic store; the fixed-rate pump below owns serialization
/// and Tauri event delivery.
pub fn set_audio_level(app: &AppHandle, channel: &'static str, level: f32) {
    let level = level.clamp(0.0, 1.0).to_bits();
    let state = app.state::<crate::AppState>();
    match channel {
        "input" => state.orb_input_level.store(level, Ordering::Relaxed),
        "output" => state.orb_output_level.store(level, Ordering::Relaxed),
        _ => {},
    }
}

fn sync_orb_config_fields(app: &AppHandle, config: &crate::config::OrbConfig) -> (bool, String) {
    app.state::<crate::AppState>()
        .orb_enabled
        .store(config.enabled, Ordering::Release);
    if let Ok(mut runtime) = app.state::<crate::AppState>().orb_window.lock() {
        runtime.auto_expand_on_wake = config.auto_expand_on_wake;
        runtime.resting_placement = placement_from_config(config.resting_x, config.resting_y);
        runtime.expanded_placement = placement_from_config(config.expanded_x, config.expanded_y);
    }
    let phrase = config
        .wake_phrases
        .first()
        .cloned()
        .unwrap_or_else(|| "hey assistant".to_string());
    // An empty phrase is what the resting pill says "Hold Left ⌥ to talk"
    // instead of inviting a detector that is not running.
    let shown_phrase = if config.wake_enabled {
        phrase.clone()
    } else {
        String::new()
    };
    dispatch(
        app,
        OrbAction::Configure {
            leash_ms: config.leash_minutes.saturating_mul(60_000),
            cooldown_ms: config.follow_up_seconds.saturating_mul(1_000),
            wake_phrase: shown_phrase,
        },
    );
    let battery_blocked = !config.armed_on_battery && power_source_is_battery();
    (battery_blocked, phrase)
}

fn apply_ready_orb_config(
    app: &AppHandle,
    config: &crate::config::OrbConfig,
    battery_blocked: bool,
    phrase: String,
) {
    crate::voice_wake::configure_native_wake(app, config.wake_enabled && !battery_blocked, phrase);
    if !config.enabled {
        let state = current_snapshot(app).state;
        if matches!(state, "off" | "ended") {
            hide(app);
        } else if state != "disarming" {
            dispatch(
                app,
                OrbAction::Disarm {
                    reason: OrbEndedReason::UserDisarm,
                },
            );
        }
        if config.wake_enabled && !battery_blocked && matches!(state, "off" | "ended") {
            crate::voice_wake::resume_native_wake(app);
        }
        return;
    }
    if battery_blocked {
        let state = current_snapshot(app).state;
        if state == "off" {
            hide(app);
        } else if !matches!(state, "ended" | "disarming") {
            dispatch(
                app,
                OrbAction::Disarm {
                    reason: OrbEndedReason::PowerPolicy,
                },
            );
        }
    } else if matches!(current_snapshot(app).state, "off" | "ended") {
        dispatch(app, OrbAction::Arm);
    }
}

async fn finish_deferred_arm(app: AppHandle) {
    // At most one second disarm can be collected here. This is an explicit,
    // bounded loop: no recursive future or unbounded task requeue is possible.
    for _ in 0..2 {
        tokio::time::sleep(Duration::from_millis(820)).await;
        if current_snapshot(&app).state == "disarming" {
            dispatch(&app, OrbAction::CompleteDisarm);
        }
        let config = app
            .state::<crate::AppState>()
            .config
            .lock()
            .await
            .orb
            .clone();
        let (battery_blocked, phrase) = sync_orb_config_fields(&app, &config);
        if current_snapshot(&app).state != "disarming" {
            apply_ready_orb_config(&app, &config, battery_blocked, phrase);
            return;
        }
        crate::voice_wake::configure_native_wake(&app, false, phrase);
    }
}

pub fn arm_from_config(app: &AppHandle, config: &crate::config::OrbConfig) {
    if setup_blocks_orb(app) {
        let phrase = config
            .wake_phrases
            .first()
            .cloned()
            .unwrap_or_else(|| "hey assistant".to_string());
        crate::voice_wake::configure_native_wake(app, false, phrase);
        hide(app);
        return;
    }
    let (battery_blocked, phrase) = sync_orb_config_fields(app, config);
    if config.enabled && !battery_blocked && current_snapshot(app).state == "disarming" {
        // A settings save can race the terminal animation just like an explicit
        // re-arm. Keep the mic stopped until the old lifecycle and owned
        // realtime sequence have completed. The continuation has a distinct
        // entrypoint instead of self-reentering this function.
        crate::voice_wake::configure_native_wake(app, false, phrase);
        tauri::async_runtime::spawn(finish_deferred_arm(app.clone()));
        return;
    }
    apply_ready_orb_config(app, config, battery_blocked, phrase);
}

fn power_source_is_battery() -> bool {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("/usr/bin/pmset").args(["-g", "batt"]).output();
        return output
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .is_some_and(|text| power_report_is_battery(&text));
    }
    #[cfg(not(target_os = "macos"))]
    false
}

fn power_report_is_battery(report: &str) -> bool {
    report.lines().next().is_some_and(|line| {
        line.trim_start()
            .starts_with("Now drawing from 'Battery Power'")
    })
}

fn ended_for_power_policy(app: &AppHandle) -> bool {
    app.state::<crate::AppState>()
        .orb
        .lock()
        .ok()
        .is_some_and(|machine| {
            matches!(
                machine.state(),
                OrbState::Ended {
                    reason: OrbEndedReason::PowerPolicy
                }
            )
        })
}

pub fn validate_orb_shortcut(shortcut: &str) -> Result<Option<Shortcut>, String> {
    let shortcut = shortcut.trim();
    if shortcut.is_empty()
        || shortcut.eq_ignore_ascii_case("none")
        || shortcut.eq_ignore_ascii_case("disabled")
    {
        return Ok(None);
    }
    shortcut
        .parse::<Shortcut>()
        .map(Some)
        .map_err(|error| format!("Invalid orb shortcut '{shortcut}': {error}"))
}

fn handle_orb_shortcut(app: &AppHandle, _shortcut: &Shortcut, event: ShortcutEvent) {
    if event.state == ShortcutState::Pressed {
        if setup_blocks_orb(app) {
            hide(app);
            return;
        }
        if matches!(current_snapshot(app).state, "off" | "ended" | "disarming") {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = orb_rearm(app).await {
                    warn!("Failed to re-arm orb from shortcut: {error}");
                }
            });
        } else {
            toggle(app);
        }
    }
}

pub fn sync_orb_shortcut(app: &AppHandle, shortcut: &str) -> Result<(), String> {
    let desired = validate_orb_shortcut(shortcut)?.map(|_| shortcut.trim().to_string());
    let previous = app
        .state::<crate::AppState>()
        .orb_window
        .lock()
        .ok()
        .and_then(|state| state.registered_shortcut.clone());

    if let Some(previous) = previous.as_deref() {
        if app.global_shortcut().is_registered(previous) {
            app.global_shortcut()
                .unregister(previous)
                .map_err(|error| format!("failed to unregister old orb shortcut: {error}"))?;
        }
    }
    if let Some(desired) = desired.as_deref() {
        if let Err(error) = app
            .global_shortcut()
            .on_shortcut(desired, handle_orb_shortcut)
        {
            if let Some(previous) = previous.as_deref() {
                let _ = app
                    .global_shortcut()
                    .on_shortcut(previous, handle_orb_shortcut);
            }
            return Err(format!("failed to register orb shortcut: {error}"));
        }
    }
    if let Ok(mut state) = app.state::<crate::AppState>().orb_window.lock() {
        state.registered_shortcut = desired;
    }
    Ok(())
}

pub fn start_tick_loop(app: AppHandle) {
    let lifecycle_app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut power_tick = 0_u8;
        loop {
            interval.tick().await;
            crate::with_autorelease_pool(|| {
                tick_with_policy(&lifecycle_app);
                power_tick = power_tick.wrapping_add(1);
                if power_tick >= 30 {
                    power_tick = 0;
                    enforce_power_policy(&lifecycle_app);
                }
            });
        }
    });
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(33));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let snapshot = current_snapshot(&app);
            let state = app.state::<crate::AppState>();
            let input = f32::from_bits(state.orb_input_level.swap(0, Ordering::Relaxed));
            let output = f32::from_bits(state.orb_output_level.swap(0, Ordering::Relaxed));
            if matches!(
                snapshot.state,
                "heard" | "connecting" | "listening" | "thinking" | "speaking"
            ) {
                emit_audio_level(&app, "input", input);
                emit_audio_level(&app, "output", output);
            }
        }
    });
}

fn tick_with_policy(app: &AppHandle) {
    let snapshot = current_snapshot(app);
    if snapshot
        .paused_until_ms
        .is_some_and(|deadline| deadline <= orb_state::epoch_ms())
    {
        let config = app
            .state::<crate::AppState>()
            .config
            .try_lock()
            .ok()
            .map(|config| config.orb.clone());
        if let Some(config) = config {
            if !config.enabled {
                dispatch(
                    app,
                    OrbAction::Disarm {
                        reason: OrbEndedReason::UserDisarm,
                    },
                );
                return;
            }
            if !config.armed_on_battery && power_source_is_battery() {
                dispatch(
                    app,
                    OrbAction::Disarm {
                        reason: OrbEndedReason::PowerPolicy,
                    },
                );
                return;
            }
        }
    }
    dispatch(app, OrbAction::Tick);
}

fn enforce_power_policy(app: &AppHandle) {
    let config = app
        .state::<crate::AppState>()
        .config
        .try_lock()
        .ok()
        .map(|config| config.orb.clone());
    let Some(config) = config else { return };
    if !config.enabled || config.armed_on_battery {
        return;
    }
    let on_battery = power_source_is_battery();
    let power_policy_ended = ended_for_power_policy(app);
    if on_battery && !matches!(current_snapshot(app).state, "off" | "ended" | "disarming") {
        dispatch(
            app,
            OrbAction::Disarm {
                reason: OrbEndedReason::PowerPolicy,
            },
        );
    } else if !on_battery && (power_policy_ended || current_snapshot(app).state == "off") {
        arm_from_config(app, &config);
    }
}

#[tauri::command]
pub fn get_orb_snapshot(app: AppHandle) -> Result<OrbSnapshot, String> {
    Ok(current_snapshot(&app))
}

#[tauri::command]
pub fn get_orb_presentation(app: AppHandle) -> Result<OrbPresentationPayload, String> {
    let state = app.state::<crate::AppState>();
    let runtime = state
        .orb_window
        .lock()
        .map_err(|_| "orb window state is unavailable".to_string())?;
    Ok(OrbPresentationPayload {
        mode: runtime.presentation,
        duration_ms: 0,
        notch: runtime.notch.has_notch,
        docked: presentation_is_docked(&runtime, runtime.presentation),
        notch_width_points: runtime.notch.notch_width_points,
        notch_height_points: runtime.notch.top_inset_points,
        revision: runtime.animation_generation,
    })
}

#[tauri::command]
pub fn orb_expand(app: AppHandle) -> Result<(), String> {
    expand(&app);
    Ok(())
}

#[tauri::command]
pub fn orb_spotlight(app: AppHandle) -> Result<(), String> {
    spotlight(&app);
    Ok(())
}

/// Forget a dragged spot and return the Orb to the top of the right edge.
#[tauri::command]
pub async fn orb_reset_home(app: AppHandle) -> Result<(), String> {
    {
        let state = app.state::<crate::AppState>();
        let mut config = state.config.lock().await;
        config.orb.resting_x = None;
        config.orb.resting_y = None;
        config.orb.expanded_x = None;
        config.orb.expanded_y = None;
        crate::config::save_config(&config)?;
    }
    if let Ok(mut runtime) = app.state::<crate::AppState>().orb_window.lock() {
        runtime.resting_placement = None;
        runtime.expanded_placement = None;
        runtime.edge_open = false;
    }
    settle_to_resting(&app, false);
    Ok(())
}

/// Widen or tuck the resting right-edge tab. Expanded and Spotlight ignore it,
/// and a dragged Orb keeps its full pill.
#[tauri::command]
pub fn orb_set_edge_open(app: AppHandle, open: bool) -> Result<(), String> {
    let (generation, reduced_motion) = {
        let state = app.state::<crate::AppState>();
        let mut runtime = state
            .orb_window
            .lock()
            .map_err(|_| "orb window state is unavailable".to_string())?;
        if runtime.intro_open
            || !matches!(runtime.presentation, OrbPresentation::Resting)
            || runtime.resting_placement.is_some()
            || runtime.edge_open == open
        {
            return Ok(());
        }
        runtime.edge_open = open;
        runtime.animation_generation = runtime.animation_generation.saturating_add(1);
        (runtime.animation_generation, runtime.reduced_motion)
    };
    let Some(window) = app.get_webview_window(ORB_WINDOW_LABEL) else {
        return Ok(());
    };
    let Some(target) = rect_for(&app, OrbPresentation::Resting) else {
        return Ok(());
    };
    if reduced_motion {
        apply_rect(&app, &window, target);
        return Ok(());
    }
    animate_to(app, target, generation, false, None, EDGE_SLIDE_MS);
    Ok(())
}

#[tauri::command]
pub fn orb_collapse(app: AppHandle) -> Result<(), String> {
    collapse(&app);
    Ok(())
}

#[tauri::command]
pub async fn orb_start_conversation(app: AppHandle) -> Result<(), String> {
    if setup_blocks_orb(&app) {
        return Err("Finish Magican setup before starting an Orb conversation".to_string());
    }
    let before = current_snapshot(&app);
    let after = dispatch(
        &app,
        OrbAction::WakeHeard {
            phrase: "Ready".to_string(),
        },
    );
    if after.revision == before.revision || after.state != "heard" {
        Err(format!(
            "orb cannot start a conversation while {}",
            before.state
        ))
    } else {
        Ok(())
    }
}

#[cfg(any(feature = "native-wake", test))]
fn native_wake_needs_rearm(state: &str) -> bool {
    matches!(state, "off" | "ended" | "disarming")
}

/// Admit a phrase accepted by the process-owned detector. Native wake is also
/// a summon boundary: if the Orb is hidden/off, persistently re-arm it first,
/// then consume the same utterance as the ordinary `WakeHeard` transition.
/// This is intentionally async and bounded through `orb_rearm`; it never
/// re-enters the detector thread or recursively schedules itself.
#[cfg(feature = "native-wake")]
pub async fn accept_native_wake(app: AppHandle, phrase: String) {
    if setup_blocks_orb(&app)
        || !app
            .state::<crate::AppState>()
            .orb_wake_enabled
            .load(Ordering::Acquire)
    {
        return;
    }
    let cold_start = native_wake_needs_rearm(current_snapshot(&app).state);
    if cold_start {
        app.state::<crate::AppState>()
            .orb_wake_handoff
            .store(true, Ordering::Release);
        crate::voice_wake::suspend_native_wake(&app);
        if let Err(error) = orb_rearm(app.clone()).await {
            app.state::<crate::AppState>()
                .orb_wake_handoff
                .store(false, Ordering::Release);
            if app
                .state::<crate::AppState>()
                .orb_wake_enabled
                .load(Ordering::Acquire)
            {
                crate::voice_wake::resume_native_wake(&app);
            }
            warn!("Native wake could not summon the Orb: {error}");
            return;
        }
    }
    // A Settings save may disable wake while the cold-start arm is completing.
    // Respect the newest device-local setting rather than admitting stale audio.
    if !app
        .state::<crate::AppState>()
        .orb_wake_enabled
        .load(Ordering::Acquire)
    {
        if cold_start {
            app.state::<crate::AppState>()
                .orb_wake_handoff
                .store(false, Ordering::Release);
        }
        return;
    }
    let before = current_snapshot(&app);
    let after = dispatch(&app, OrbAction::WakeHeard { phrase });
    if cold_start {
        app.state::<crate::AppState>()
            .orb_wake_handoff
            .store(false, Ordering::Release);
    }
    if before.revision == after.revision {
        crate::voice_wake::resume_native_wake(&app);
        info!(
            state = before.state,
            "Native wake was ignored by the Orb lifecycle"
        );
    }
}

#[tauri::command]
pub async fn orb_disarm(app: AppHandle) -> Result<(), String> {
    {
        let state = app.state::<crate::AppState>();
        let mut config = state.config.lock().await;
        config.orb.enabled = false;
        crate::config::save_config(&config)?;
    }
    app.state::<crate::AppState>()
        .orb_enabled
        .store(false, Ordering::Release);
    dispatch(
        &app,
        OrbAction::Disarm {
            reason: OrbEndedReason::UserDisarm,
        },
    );
    crate::tray::refresh_menu(&app, None);
    Ok(())
}

#[tauri::command]
pub async fn orb_rearm(app: AppHandle) -> Result<(), String> {
    if setup_blocks_orb(&app) {
        return Err("Finish Magican setup before enabling the Orb".to_string());
    }
    let mut orb = {
        let state = app.state::<crate::AppState>();
        let mut config = state.config.lock().await;
        config.orb.enabled = true;
        crate::config::save_config(&config)?;
        config.orb.clone()
    };
    app.state::<crate::AppState>()
        .orb_enabled
        .store(true, Ordering::Release);
    if current_snapshot(&app).state == "disarming" {
        // Let the disarm choreography and sequence-scoped microphone teardown
        // finish before the detector can be armed again.
        tokio::time::sleep(Duration::from_millis(820)).await;
        if current_snapshot(&app).state == "disarming" {
            dispatch(&app, OrbAction::CompleteDisarm);
        }
        orb = app
            .state::<crate::AppState>()
            .config
            .lock()
            .await
            .orb
            .clone();
    }
    arm_from_config(&app, &orb);
    crate::tray::refresh_menu(&app, None);
    Ok(())
}

#[tauri::command]
pub async fn orb_pause_one_hour(app: AppHandle) -> Result<(), String> {
    let before = current_snapshot(&app);
    let after = dispatch(
        &app,
        OrbAction::Pause {
            duration_ms: PAUSE_ONE_HOUR_MS,
        },
    );
    if before.revision == after.revision || after.state != "paused" {
        return Err(format!("orb cannot pause while {}", before.state));
    }
    crate::voice_wake::suspend_native_wake(&app);
    crate::voice_note::request_end_orb_conversation(app.clone());
    crate::tray::refresh_menu(&app, None);
    Ok(())
}

#[tauri::command]
pub async fn orb_resume(app: AppHandle) -> Result<(), String> {
    let config = app
        .state::<crate::AppState>()
        .config
        .lock()
        .await
        .orb
        .clone();
    if !config.enabled {
        return Err("The Orb is resting; wake it instead".to_string());
    }
    if !config.armed_on_battery && power_source_is_battery() {
        return Err("the orb is paused by the battery policy".to_string());
    }
    let before = current_snapshot(&app);
    let after = dispatch(&app, OrbAction::Resume);
    if after.revision == before.revision || !matches!(after.state, "armed" | "voice_busy") {
        return Err(format!("orb cannot resume while {}", before.state));
    }
    crate::tray::refresh_menu(&app, None);
    Ok(())
}

#[tauri::command]
pub fn orb_set_reduced_motion(app: AppHandle, reduced: bool) -> Result<(), String> {
    if let Ok(mut runtime) = app.state::<crate::AppState>().orb_window.lock() {
        runtime.reduced_motion = reduced;
    }
    Ok(())
}

#[tauri::command]
pub fn orb_set_shortcut_recording(app: AppHandle, recording: bool) -> Result<(), String> {
    let shortcut = {
        let state = app.state::<crate::AppState>();
        let mut runtime = state
            .orb_window
            .lock()
            .map_err(|_| "orb window state is unavailable".to_string())?;
        if runtime.shortcut_recording == recording {
            return Ok(());
        }
        runtime.shortcut_recording = recording;
        runtime.registered_shortcut.clone()
    };
    let Some(shortcut) = shortcut else {
        return Ok(());
    };
    if recording {
        if app.global_shortcut().is_registered(shortcut.as_str()) {
            app.global_shortcut()
                .unregister(shortcut.as_str())
                .map_err(|error| format!("failed to suspend orb shortcut recording: {error}"))?;
        }
    } else if !app.global_shortcut().is_registered(shortcut.as_str()) {
        app.global_shortcut()
            .on_shortcut(shortcut.as_str(), handle_orb_shortcut)
            .map_err(|error| format!("failed to restore orb shortcut after recording: {error}"))?;
    }
    Ok(())
}

#[tauri::command]
pub fn orb_open_app(app: AppHandle) -> Result<(), String> {
    crate::tray::open_app(&app);
    collapse(&app);
    Ok(())
}

#[tauri::command]
pub fn orb_open_settings(app: AppHandle) -> Result<(), String> {
    crate::tray::open_settings_window(&app);
    collapse(&app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_report_requires_the_authoritative_power_source_header() {
        assert!(power_report_is_battery(
            "Now drawing from 'Battery Power'\n -InternalBattery-0"
        ));
        assert!(!power_report_is_battery(
            "Now drawing from 'AC Power'\n -InternalBattery-0"
        ));
        assert!(!power_report_is_battery(
            "Battery Power appears later\nAC Power"
        ));
    }

    #[test]
    fn optional_shortcut_vocabulary_is_explicit() {
        assert_eq!(validate_orb_shortcut("").unwrap(), None);
        assert_eq!(validate_orb_shortcut("disabled").unwrap(), None);
        assert!(validate_orb_shortcut("not a chord at all").is_err());
        assert!(validate_orb_shortcut(crate::config::DEFAULT_ORB_SHORTCUT)
            .unwrap()
            .is_some());
    }

    #[test]
    fn native_wake_rearms_only_cold_or_terminating_orb_states() {
        for state in ["off", "ended", "disarming"] {
            assert!(native_wake_needs_rearm(state), "{state}");
        }
        for state in [
            "armed",
            "heard",
            "connecting",
            "listening",
            "paused",
            "voice_busy",
        ] {
            assert!(!native_wake_needs_rearm(state), "{state}");
        }
    }

    #[test]
    fn native_wake_cold_start_rearms_before_admitting_the_phrase() {
        let source = include_str!("orb_window.rs");
        let handler = source
            .split_once("pub async fn accept_native_wake(")
            .expect("native wake handler")
            .1
            .split_once("#[tauri::command]\npub async fn orb_disarm")
            .expect("disarm follows native wake handler")
            .0;
        let rearm = handler
            .find("orb_rearm(app.clone()).await")
            .expect("cold rearm");
        let admission = handler
            .find("OrbAction::WakeHeard { phrase }")
            .expect("wake admission");
        assert!(rearm < admission);
        assert!(handler.contains("orb_wake_enabled"));
        assert!(handler.contains("orb_wake_handoff"));
        assert!(handler.contains("suspend_native_wake(&app)"));
    }

    #[test]
    fn native_sizes_keep_the_resting_surface_quiet_and_the_wake_surface_generous() {
        assert!(EDGE_PILL_HEIGHT_PT <= 56.0);
        assert!(EDGE_PEEK_WIDTH_PT < RESTING_MIN_WIDTH_PT);
        assert!(EDGE_PILL_HEIGHT_PT < EXPANDED_HEIGHT_PT);
        assert!(EXPANDED_WIDTH_PT >= 360.0);
        assert!(SPOTLIGHT_WIDTH_PT > EXPANDED_WIDTH_PT);
        assert!(SPOTLIGHT_HEIGHT_PT > EXPANDED_HEIGHT_PT);
    }

    #[test]
    fn wake_spotlight_settles_to_the_compact_top_presence() {
        let source = include_str!("orb_window.rs");
        let wake = source
            .split_once("pub fn present_wake(")
            .expect("wake presenter")
            .1
            .split_once("pub fn current_snapshot(")
            .expect("snapshot follows wake presenter")
            .0;

        assert!(wake.contains("animate_at(app, OrbPresentation::Spotlight, false)"));
        assert!(wake.contains("settle_to_resting(&app, false)"));
    }

    #[test]
    fn persisted_orb_placement_requires_a_complete_finite_coordinate_pair() {
        assert_eq!(
            placement_from_config(Some(-1.0), Some(2.0)),
            Some(OrbPlacement { x: 0.0, y: 1.0 })
        );
        assert_eq!(placement_from_config(Some(f64::NAN), Some(0.5)), None);
        assert_eq!(placement_from_config(Some(0.5), None), None);
    }

    #[test]
    fn startup_card_lists_the_configured_shortcuts_and_skips_what_is_off() {
        let hints = startup_hotkey_hints("Double Left Option", "Alt+Space", false, "hey assistant");
        assert_eq!(
            hints,
            vec![
                OrbHotkeyHint {
                    keys: "Hold Left ⌥".to_string(),
                    action: "Talk".to_string(),
                },
                OrbHotkeyHint {
                    keys: "Double-tap Left ⌥".to_string(),
                    action: "Quick Automate".to_string(),
                },
                OrbHotkeyHint {
                    keys: "⌥ Space".to_string(),
                    action: "Show the Orb".to_string(),
                },
            ]
        );
        let awake = startup_hotkey_hints("Disabled", "none", true, "hey magician");
        assert_eq!(
            awake,
            vec![OrbHotkeyHint {
                keys: "Say “hey magician”".to_string(),
                action: "Wake".to_string(),
            }]
        );
        assert!(startup_hotkey_hints("Disabled", "disabled", false, "hey assistant").is_empty());
        assert_eq!(display_shortcut("Shift+Alt+S").as_deref(), Some("⇧ ⌥ S"));
    }

    #[test]
    fn default_resting_tab_is_tucked_under_the_menu_bar_on_the_right_edge() {
        let position = PhysicalPosition::new(0, 0);
        let size = PhysicalSize::new(3_024, 1_964);
        let notch = NotchMetrics {
            top_inset_points: 32.0,
            notch_width_points: 150.0,
            has_notch: true,
        };

        let resting = rect_for_monitor(
            &position,
            &size,
            2.0,
            notch,
            OrbPresentation::Resting,
            None,
            false,
            false,
        );
        let revealed = rect_for_monitor(
            &position,
            &size,
            2.0,
            notch,
            OrbPresentation::Resting,
            None,
            true,
            false,
        );
        let expanded = rect_for_monitor(
            &position,
            &size,
            2.0,
            notch,
            OrbPresentation::Expanded,
            None,
            false,
            false,
        );

        assert_eq!(resting.y, scaled(32.0 + EDGE_BELOW_MENU_PT, 2.0) as i32);
        assert!(resting.y > position.y);
        assert_eq!(resting.width, scaled(EDGE_PEEK_WIDTH_PT, 2.0));
        assert_eq!(resting.height, scaled(EDGE_PILL_HEIGHT_PT, 2.0));
        assert_eq!(
            resting.x + resting.width as i32,
            position.x + size.width as i32
        );
        assert_eq!(revealed.width, scaled(RESTING_MIN_WIDTH_PT, 2.0));
        assert_eq!(
            revealed.x + revealed.width as i32,
            resting.x + resting.width as i32
        );
        assert!(revealed.x < resting.x);
        assert_eq!(expanded.y, position.y);
        assert_eq!(
            expanded.x + expanded.width as i32 / 2,
            size.width as i32 / 2
        );

        let notchless = NotchMetrics::default();
        let parked = rect_for_monitor(
            &position,
            &size,
            2.0,
            notchless,
            OrbPresentation::Resting,
            None,
            false,
            false,
        );
        assert_eq!(
            parked.y,
            scaled(MENU_BAR_FALLBACK_PT + EDGE_BELOW_MENU_PT, 2.0) as i32
        );
        assert_eq!(
            parked.x + parked.width as i32,
            position.x + size.width as i32
        );
        let spotlight = rect_for_monitor(
            &position,
            &size,
            2.0,
            notch,
            OrbPresentation::Spotlight,
            None,
            false,
            false,
        );
        assert_eq!(
            spotlight.x + spotlight.width as i32 / 2,
            size.width as i32 / 2
        );
        assert_eq!(
            spotlight.y + spotlight.height as i32 / 2,
            size.height as i32 / 2
        );
    }

    #[test]
    fn zero_auxiliary_rectangles_are_a_notchless_display_not_a_screen_wide_notch() {
        let notchless = notch_metrics_from_visible_top_areas(0.0, 1_512.0, 0.0, 0.0);
        assert!(!notchless.has_notch);
        assert_eq!(notchless.notch_width_points, 0.0);
        assert_eq!(notchless.top_inset_points, 0.0);

        let notched = notch_metrics_from_visible_top_areas(32.0, 1_512.0, 681.0, 681.0);
        assert!(notched.has_notch);
        assert_eq!(notched.notch_width_points, 150.0);
        assert_eq!(notched.top_inset_points, 32.0);
    }

    #[test]
    fn parked_positions_detach_while_spotlight_never_claims_the_notch() {
        let mut runtime = OrbWindowRuntime::default();
        assert!(presentation_is_docked(&runtime, OrbPresentation::Resting));
        assert!(presentation_is_docked(&runtime, OrbPresentation::Expanded));
        assert!(!presentation_is_docked(
            &runtime,
            OrbPresentation::Spotlight
        ));

        runtime.resting_placement = Some(OrbPlacement { x: 0.2, y: 0.3 });
        runtime.expanded_placement = Some(OrbPlacement { x: 0.7, y: 0.4 });
        assert!(!presentation_is_docked(&runtime, OrbPresentation::Resting));
        assert!(!presentation_is_docked(&runtime, OrbPresentation::Settling));
        assert!(!presentation_is_docked(&runtime, OrbPresentation::Expanded));
    }

    #[test]
    fn magnetic_dock_has_a_bounded_snap_zone() {
        let dock = OrbRect {
            x: 400,
            y: 0,
            width: 214,
            height: 82,
        };
        assert!(position_is_near_dock(
            &PhysicalPosition::new(450, 45),
            dock,
            1.0,
        ));
        assert!(!position_is_near_dock(
            &PhysicalPosition::new(457, 0),
            dock,
            1.0,
        ));
        assert!(position_is_near_dock(
            &PhysicalPosition::new(510, 0),
            dock,
            2.0,
        ));
    }
}
