//! Native window and runtime bridge for the notch orb.
//!
//! AppKit owns placement and focus behavior; the webview owns every pixel.  The
//! boundary is intentionally small: lifecycle snapshots, presentation modes,
//! captions, and audio envelopes cross it as Tauri events.

use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::{Deserialize, Serialize};
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

// The wide pill and the top panel are no longer sizes. The names stay so
// the geometry tests and the frontend contract can still see them.
#[allow(dead_code)]
const RESTING_MIN_WIDTH_PT: f64 = 214.0;
#[allow(dead_code)]
const EXPANDED_WIDTH_PT: f64 = 480.0;
#[allow(dead_code)]
const EXPANDED_HEIGHT_PT: f64 = 300.0;
const SPOTLIGHT_WIDTH_PT: f64 = 620.0;
const SPOTLIGHT_HEIGHT_PT: f64 = 500.0;
const WAKE_HOLD_MS: u64 = 1_250;
const SETTLE_DURATION_MS: u64 = 520;
#[allow(dead_code)]
const DOCK_SNAP_DISTANCE_PT: f64 = 56.0;
/// Side-notch ear: a narrow stem flush with the right bezel.
const EDGE_PEEK_WIDTH_PT: f64 = 40.0;
const EDGE_PILL_HEIGHT_PT: f64 = 64.0;
/// Space from one ear to the next: the Orb ear, the tray-mark ear and the
/// runs ear. Negative: each ear's 14pt curved ends overlap the next ear's, so
/// about 6pt of the bezel's neck separates their black bodies and the ears
/// read as one station rather than separate tabs (`--ear-gap` in the Orb page).
const TRAY_NOTCH_GAP_PT: f64 = -22.0;
/// Orb ear, the gap, and the tray ear. The tray ear alone is the station when the Orb is hidden.
const NOTCH_STACK_HEIGHT_PT: f64 = EDGE_PILL_HEIGHT_PT * 2.0 + TRAY_NOTCH_GAP_PT;
/// Keep the station off the menu bar and off the bottom corner of the screen.
const EDGE_SCREEN_MARGIN_PT: f64 = 8.0;
/// The ear's curved ends (`--notch-fillet` in `OrbSurface.svelte`).
const NOTCH_FILLET_PT: f64 = 14.0;
/// Runs ear under the tray ear: one row per coding run at work, as getone.one
/// shows a dot per agent along the screen edge.
const RUN_EAR_ROW_PT: f64 = 28.0;
/// Space inside the runs ear above the first row and below the last.
const RUN_EAR_PADDING_PT: f64 = 6.0;
/// Most rows the runs ear grows to. With more runs it stays this tall and
/// scrolls (`OrbSurface.svelte` draws the same rows).
const RUN_EAR_MAX_ROWS: usize = 4;
/// Hovering a run dot widens the closed station to the left for its label
/// pill (state and run title); the ear stays on the bezel.
const RUN_LABEL_WIDTH_PT: f64 = 300.0;
/// The coding-runs panel is told which item a later dot tap picked.
pub const NOTCH_RUN_SELECT_EVENT: &str = "notch://runs-select";
/// The cursor over the notch station, in window CSS pixels (`inside: false`
/// once it leaves). See [`ensure_notch_pointer_poll`].
pub const NOTCH_POINTER_EVENT: &str = "notch://pointer";
/// How often the runs ear's hover poll reads the cursor.
const NOTCH_POINTER_POLL_MS: u64 = 60;
/// Ask-user / HITL drawer that grows left from the ears. The right edge stays put.
const NOTCH_DRAWER_WIDTH_PT: f64 = 440.0;
const NOTCH_DRAWER_HEIGHT_PT: f64 = 560.0;
pub const NOTCH_ATTENTION_WINDOW_LABEL: &str = "notch-attention";
/// Transcript strip beside the orb ear. Twice the ear, wide enough for a few lines.
const VOICE_DRAWER_WIDTH_PT: f64 = 360.0;
const VOICE_DRAWER_HEIGHT_PT: f64 = EDGE_PILL_HEIGHT_PT * 2.0;
/// Close a talk-opened transcript strip after five quiet seconds.
const VOICE_DRAWER_IDLE_MS: u64 = 5_000;
/// Both docked drawers open on this spring.
const DRAWER_SPRING_MS: u64 = 260;
/// The panel slides fully off the screen edge, then the window lands on the ear.
/// This waits out the CSS leave so the window does not clip the panel.
const DRAWER_EXIT_MS: u64 = 280;
/// Older content-height reports. The drawer is a fixed window now; these only clamp the stored value.
const NOTCH_INBOX_MIN_HEIGHT_PT: f64 = 196.0;
const NOTCH_INBOX_MAX_HEIGHT_PT: f64 = 560.0;
/// Notchless displays have no safe-area inset; the menu bar is still this tall.
const MENU_BAR_FALLBACK_PT: f64 = 25.0;
/// Extra gap so the tab sits under the menu bar instead of on its items.
const EDGE_BELOW_MENU_PT: f64 = 18.0;
const EDGE_SLIDE_MS: u64 = 220;
const INTRO_HOLD_MS: u64 = 3_200;
pub const ORB_INTRO_EVENT: &str = "orb://intro";
pub const NOTCH_INBOX_EVENT: &str = "notch://inbox";
pub const NOTCH_ANSWER_EVENT: &str = "notch://answer";
pub const NOTCH_REPORT_EVENT: &str = "notch://report";

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
    /// No longer a place the Orb sits. Kept so a serialized mode still parses.
    #[allow(dead_code)]
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
    /// The question panel is open on the side notch.
    pub inbox_open: bool,
    /// Attention questions waiting in the notch.
    pub attention_count: u32,
    /// False when the ear is only an Attention notch and the Orb is hidden.
    pub voice_visible: bool,
    /// The short transcript strip beside the docked orb ear is open.
    pub voice_drawer_open: bool,
    /// Work items (coding runs, crews, Work tasks) the runs ear paints as
    /// dots, most urgent first.
    pub runs: Vec<NotchRunDot>,
    /// The open side panel shows the work list rather than Attention.
    pub runs_panel_open: bool,
    /// The dot id (work item id) the open runs panel has picked, if any.
    pub runs_panel_run: Option<String>,
}

/// One work item on the tray ear: what the dot shows and its tooltip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotchRunDot {
    /// The work item id (`run:…`, `crew:…`, `task:…`); a bare run id from an
    /// older overlay is a coding run.
    pub id: String,
    /// `needs_you | working | paused | failed | stopped | done`.
    pub state: String,
    pub title: String,
    /// The item's page in the main window (`web_path`).
    #[serde(default)]
    pub path: Option<String>,
}

/// Most dots the ear keeps. The overlay already sends no more than this.
const NOTCH_RUN_DOT_LIMIT: usize = 12;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotchOption {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotchInboxItem {
    pub id: String,
    pub correlation_id: String,
    pub source: String,
    pub input_type: String,
    pub prompt: String,
    #[serde(default)]
    pub hint: Option<String>,
    pub inline: String,
    #[serde(default)]
    pub options: Vec<NotchOption>,
    #[serde(default)]
    pub confirm_label: Option<String>,
    #[serde(default)]
    pub deny_label: Option<String>,
    #[serde(default)]
    pub execution_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotchAnswer {
    pub id: String,
    pub correlation_id: String,
    pub kind: String,
    #[serde(default)]
    pub choice_id: Option<String>,
    #[serde(default)]
    pub choice_ids: Option<Vec<String>>,
    #[serde(default)]
    pub text: Option<String>,
    /// Ask source (`user_request`, `approval`, …). Absent for a dismiss.
    #[serde(default)]
    pub source: Option<String>,
    /// The ask's input type. The inline `kind` is only the notch control.
    #[serde(default)]
    pub input_type: Option<String>,
    #[serde(default)]
    pub execution_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    /// Set by the host after it has posted. The overlay must not post again.
    #[serde(default)]
    pub posted: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotchInboxPayload {
    items: Vec<NotchInboxItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NotchReport {
    id: String,
    ok: bool,
    message: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
struct EdgeFrame {
    inbox_open: bool,
    /// The window is up only as the tray ear. The Orb ear is hidden.
    attention_only: bool,
    /// The short transcript strip is open beside the orb ear.
    voice_open: bool,
    /// 0 sits just under the menu bar. 1 sits at the bottom of the screen.
    /// `None` is the top home.
    dock_y: Option<f64>,
    /// Rows in the runs ear under the tray ear; 0 hides it.
    run_rows: usize,
    /// A run dot's label pill is out beside the ear.
    run_label_open: bool,
}

/// The side panel is one window. The tray ear opens it on Attention; a run
/// dot opens it on the work list with that item (the dot's id) picked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum NotchDrawerView {
    #[default]
    Attention,
    Runs {
        selected: Option<String>,
    },
}

/// The query pair that picks dot `id` on `/coding-runs`: a coding run keeps
/// `run=<run id>`; a crew or Work task is `item=<item id>`.
fn notch_panel_pick(id: &str) -> (&'static str, &str) {
    if let Some(run) = id.strip_prefix("run:") {
        return ("run", run);
    }
    if id.starts_with("crew:") || id.starts_with("task:") {
        return ("item", id);
    }
    ("run", id)
}

/// The runs ear's height, its gap above it included; 0 with no runs.
fn runs_ear_height_pt(rows: usize) -> f64 {
    if rows == 0 {
        return 0.0;
    }
    TRAY_NOTCH_GAP_PT
        + NOTCH_FILLET_PT * 2.0
        + RUN_EAR_PADDING_PT * 2.0
        + rows.min(RUN_EAR_MAX_ROWS) as f64 * RUN_EAR_ROW_PT
}

/// The closed station: the Orb ear (unless hidden), the tray ear and the runs ear.
fn station_height_pt(edge: &EdgeFrame) -> f64 {
    let ears = if edge.attention_only {
        EDGE_PILL_HEIGHT_PT
    } else {
        NOTCH_STACK_HEIGHT_PT
    };
    ears + runs_ear_height_pt(edge.run_rows)
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
    /// Native rect spring. A drag must not record frames from this motion.
    motion_generation: u64,
    motion_active: bool,
    motion_target: Option<OrbRect>,
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
    inbox_items: Vec<NotchInboxItem>,
    run_dots: Vec<NotchRunDot>,
    /// What the side panel shows while `inbox_open`.
    drawer_view: NotchDrawerView,
    /// The pointer is on a run dot: the station is wide enough for its label.
    run_label_open: bool,
    /// Canonical server total, independent of the loaded cursor page.
    attention_count: u32,
    inbox_open: bool,
    inbox_height_pt: f64,
    /// The voice control can be hidden independently of the always-available tray ear.
    voice_visible: bool,
    /// Vertical anchor while the ear is on the right bezel.
    dock_y: Option<f64>,
    /// Transcript strip beside the docked orb. Not the Attention drawer.
    voice_drawer_open: bool,
    /// A click closed the strip during this live turn. The next turn may open it.
    voice_drawer_hold_closed: bool,
    /// A tap opened the strip. It stays until the next tap.
    voice_drawer_manual: bool,
    voice_drawer_idle_generation: u64,
    /// Answers the notch has handed off. The overlay drains these, so a missed
    /// `notch://answer` event still posts.
    pending_answers: Vec<NotchAnswer>,
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
            motion_generation: 0,
            motion_active: false,
            motion_target: None,
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
            inbox_items: Vec::new(),
            run_dots: Vec::new(),
            drawer_view: NotchDrawerView::Attention,
            run_label_open: false,
            attention_count: 0,
            inbox_open: false,
            inbox_height_pt: NOTCH_INBOX_MIN_HEIGHT_PT,
            voice_visible: true,
            dock_y: None,
            voice_drawer_open: false,
            voice_drawer_hold_closed: false,
            voice_drawer_manual: false,
            voice_drawer_idle_generation: 0,
            pending_answers: Vec::new(),
        }
    }
}

impl OrbWindowRuntime {
    pub fn new(voice_visible: bool) -> Self {
        Self {
            voice_visible,
            ..Self::default()
        }
    }

    fn set_voice_visible(&mut self, visible: bool) {
        self.voice_visible = visible;
        if !visible {
            self.voice_drawer_open = false;
            self.voice_drawer_hold_closed = false;
            self.voice_drawer_manual = false;
            self.voice_drawer_idle_generation = self.voice_drawer_idle_generation.saturating_add(1);
        }
    }
}

fn edge_frame(runtime: &OrbWindowRuntime) -> EdgeFrame {
    EdgeFrame {
        inbox_open: runtime.inbox_open,
        attention_only: !runtime.voice_visible,
        voice_open: runtime.voice_drawer_open,
        dock_y: runtime.dock_y.filter(|value| value.is_finite()),
        run_rows: runtime.run_dots.len().min(RUN_EAR_MAX_ROWS),
        run_label_open: runtime.run_label_open && !runtime.run_dots.is_empty(),
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

/// Vertical range for a docked ear: just under the menu bar, down to the
/// bottom margin. The window stays fully on the screen.
fn dock_travel(
    monitor_y: i32,
    monitor_height: u32,
    window_height: u32,
    scale: f64,
    notch: NotchMetrics,
) -> (i32, i32) {
    let min_y = resting_edge_y(monitor_y, scale, notch);
    let bottom_margin = scaled(EDGE_SCREEN_MARGIN_PT, scale) as i32;
    let max_y = monitor_y
        .saturating_add_unsigned(monitor_height)
        .saturating_sub(window_height as i32)
        .saturating_sub(bottom_margin)
        .max(min_y);
    (min_y, max_y)
}

/// The ears stay at the saved height when the drawer opens. The drawer's top
/// is the closed station's top, then shifts up only far enough to stay on screen.
fn side_station_top(
    monitor_y: i32,
    monitor_height: u32,
    window_height: u32,
    station_height: u32,
    scale: f64,
    notch: NotchMetrics,
    dock_y: Option<f64>,
) -> i32 {
    let mut top = docked_resting_y(
        monitor_y,
        monitor_height,
        station_height,
        scale,
        notch,
        dock_y,
    );
    let margin = scaled(EDGE_SCREEN_MARGIN_PT, scale) as i32;
    let screen_bottom = monitor_y
        .saturating_add_unsigned(monitor_height)
        .saturating_sub(margin);
    let window_bottom = top.saturating_add_unsigned(window_height);
    if window_bottom > screen_bottom {
        top -= window_bottom - screen_bottom;
    }
    top.max(resting_edge_y(monitor_y, scale, notch))
}

/// `None` and `0` sit at the top home. `1` sits on the bottom margin.
fn docked_resting_y(
    monitor_y: i32,
    monitor_height: u32,
    window_height: u32,
    scale: f64,
    notch: NotchMetrics,
    dock_y: Option<f64>,
) -> i32 {
    let (min_y, max_y) = dock_travel(monitor_y, monitor_height, window_height, scale, notch);
    let Some(fraction) = dock_y.filter(|value| value.is_finite()) else {
        return min_y;
    };
    let available = (max_y - min_y).max(0) as f64;
    min_y + (available * fraction.clamp(0.0, 1.0)).round() as i32
}

fn dock_y_fraction(
    drop_top: i32,
    monitor_y: i32,
    monitor_height: u32,
    window_height: u32,
    scale: f64,
    notch: NotchMetrics,
) -> f64 {
    let (min_y, max_y) = dock_travel(monitor_y, monitor_height, window_height, scale, notch);
    let clamped = drop_top.clamp(min_y, max_y);
    let available = (max_y - min_y).max(1) as f64;
    ((clamped - min_y) as f64 / available).clamp(0.0, 1.0)
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
    let (notch, placement, edge_open, intro_open, edge) = app
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
            (
                state.notch,
                placement,
                state.edge_open,
                intro_open,
                edge_frame(&state),
            )
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
        edge,
    ))
}

fn rect_for_monitor(
    position: &PhysicalPosition<i32>,
    size: &PhysicalSize<u32>,
    scale: f64,
    notch: NotchMetrics,
    presentation: OrbPresentation,
    _placement: Option<OrbPlacement>,
    _edge_open: bool,
    intro: bool,
    edge: EdgeFrame,
) -> OrbRect {
    // Saved coordinates and the old widen-on-open flag are not sizes. The
    // ear stays on the right edge. The large Orb stays at the center.
    let logical_monitor_width = size.width as f64 / scale;
    let logical_monitor_height = size.height as f64 / scale;
    let (width_pt, height_pt) = match presentation {
        // The side station. The Orb ear and the tray ear share it. A waiting
        // question opens the drawer to the left. Settling is already on its
        // way back to the closed station.
        OrbPresentation::Hidden | OrbPresentation::Resting | OrbPresentation::Settling => {
            if intro {
                (SPOTLIGHT_WIDTH_PT, SPOTLIGHT_HEIGHT_PT)
            } else if edge.inbox_open
                && matches!(
                    presentation,
                    OrbPresentation::Hidden | OrbPresentation::Resting
                )
            {
                (
                    NOTCH_DRAWER_WIDTH_PT,
                    NOTCH_DRAWER_HEIGHT_PT.max(station_height_pt(&edge)),
                )
            } else if edge.voice_open
                && matches!(
                    presentation,
                    OrbPresentation::Hidden | OrbPresentation::Resting
                )
            {
                // The strip is twice the orb ear. The window is at least as
                // tall as the ears (taller than the overlapping pair) and keeps
                // the station's top, so opening it does not slide the station.
                let height_pt = if edge.attention_only && edge.run_rows == 0 {
                    VOICE_DRAWER_HEIGHT_PT
                } else {
                    station_height_pt(&edge).max(VOICE_DRAWER_HEIGHT_PT)
                };
                (VOICE_DRAWER_WIDTH_PT, height_pt)
            } else if edge.run_label_open && edge.run_rows > 0 {
                // Room to the left for the hovered dot's label; the ears
                // stay flush with the bezel, so nothing under the pointer moves.
                (RUN_LABEL_WIDTH_PT, station_height_pt(&edge))
            } else {
                (EDGE_PEEK_WIDTH_PT, station_height_pt(&edge))
            }
        },
        // Expanded is no longer a place the Orb can sit. A leftover caller
        // arrives at the same center as Spotlight.
        OrbPresentation::Expanded | OrbPresentation::Spotlight => {
            (SPOTLIGHT_WIDTH_PT, SPOTLIGHT_HEIGHT_PT)
        },
    };
    let width = scaled(width_pt.min(logical_monitor_width - 24.0), scale);
    let height = scaled(height_pt.min(logical_monitor_height - 24.0), scale);

    let centered_x = position.x + ((size.width.saturating_sub(width)) / 2) as i32;
    let centered_y = position.y + ((size.height.saturating_sub(height)) / 2) as i32;
    let default_x = if intro {
        centered_x
    } else {
        match presentation {
            OrbPresentation::Hidden | OrbPresentation::Resting | OrbPresentation::Settling => {
                right_edge_origin_x(position.x, size.width, width)
            },
            OrbPresentation::Expanded | OrbPresentation::Spotlight => centered_x,
        }
    };
    let default_y = if intro {
        centered_y
    } else {
        match presentation {
            // The large Orb is the center of the screen. The ear's height
            // along the right edge is `dock_y` (none is just under the menu bar).
            OrbPresentation::Expanded | OrbPresentation::Spotlight => centered_y,
            OrbPresentation::Hidden | OrbPresentation::Resting | OrbPresentation::Settling => {
                let station_pt = station_height_pt(&edge);
                let station_height = scaled(station_pt.min(logical_monitor_height - 24.0), scale);
                side_station_top(
                    position.y,
                    size.height,
                    height,
                    station_height,
                    scale,
                    notch,
                    edge.dock_y,
                )
            },
        }
    };
    OrbRect {
        x: default_x,
        y: default_y,
        width,
        height,
    }
}

fn placement_for_presentation(
    _runtime: &OrbWindowRuntime,
    _presentation: OrbPresentation,
) -> Option<OrbPlacement> {
    // A saved spot is not a size. The ear is the right edge. The large Orb
    // is the center. Neither follows resting_x/y or expanded_x/y.
    None
}

fn presentation_is_docked(runtime: &OrbWindowRuntime, presentation: OrbPresentation) -> bool {
    if matches!(presentation, OrbPresentation::Spotlight) {
        return false;
    }
    // An open question, or an Attention ear with the Orb hidden, is drawn on
    // the bezel even when a dragged spot is saved. The ear styles follow that.
    if matches!(
        presentation,
        OrbPresentation::Hidden | OrbPresentation::Resting
    ) && (runtime.inbox_open || !runtime.voice_visible)
    {
        return true;
    }
    placement_for_presentation(runtime, presentation).is_none()
}

/// Old two-axis snap, kept for the geometry tests. The live ear does not use
/// it: every resting drop is flush to the right edge.
#[cfg(test)]
fn window_is_near_dock(
    position: &PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    dock: OrbRect,
    scale: f64,
) -> bool {
    let threshold = scaled(DOCK_SNAP_DISTANCE_PT, scale) as i32;
    let window_right = position.x.saturating_add_unsigned(size.width);
    let dock_right = dock.x.saturating_add_unsigned(dock.width);
    (window_right - dock_right).abs() <= threshold && (position.y - dock.y).abs() <= threshold
}

/// Right-edge proximity, kept for the geometry tests. A live drop does not
/// consult it.
#[cfg(test)]
fn resting_window_is_on_right_edge(
    position: &PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    monitor_right: i32,
    scale: f64,
) -> bool {
    let threshold = scaled(DOCK_SNAP_DISTANCE_PT, scale) as i32;
    let window_right = position.x.saturating_add_unsigned(size.width);
    (window_right - monitor_right).abs() <= threshold
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

fn presentation_payload(
    _app: &AppHandle,
    runtime: &OrbWindowRuntime,
    mode: OrbPresentation,
    duration_ms: u64,
) -> OrbPresentationPayload {
    OrbPresentationPayload {
        mode,
        duration_ms,
        notch: runtime.notch.has_notch,
        docked: presentation_is_docked(runtime, mode),
        notch_width_points: runtime.notch.notch_width_points,
        notch_height_points: runtime.notch.top_inset_points,
        revision: runtime.animation_generation,
        inbox_open: runtime.inbox_open,
        attention_count: runtime.attention_count,
        voice_visible: runtime.voice_visible,
        voice_drawer_open: runtime.voice_drawer_open,
        runs: runtime.run_dots.clone(),
        runs_panel_open: runtime.inbox_open
            && matches!(runtime.drawer_view, NotchDrawerView::Runs { .. }),
        runs_panel_run: match &runtime.drawer_view {
            NotchDrawerView::Runs { selected } if runtime.inbox_open => selected.clone(),
            _ => None,
        },
    }
}

fn emit_current_presentation(app: &AppHandle) {
    let state = app.state::<crate::AppState>();
    let Ok(runtime) = state.orb_window.lock() else {
        return;
    };
    let mode = runtime.presentation;
    let _ = app.emit(
        ORB_PRESENTATION_EVENT,
        presentation_payload(app, &runtime, mode, 0),
    );
    drop(runtime);
    sync_notch_attention_window(app);
}

fn set_presentation_state(app: &AppHandle, presentation: OrbPresentation) -> u64 {
    let mut generation = 0;
    let mut payload = None;
    if let Ok(mut runtime) = app.state::<crate::AppState>().orb_window.lock() {
        runtime.presentation = presentation;
        runtime.expanded = matches!(
            presentation,
            OrbPresentation::Expanded | OrbPresentation::Spotlight | OrbPresentation::Settling
        );
        runtime.animation_generation = runtime.animation_generation.saturating_add(1);
        generation = runtime.animation_generation;
        let duration_ms = match presentation {
            OrbPresentation::Settling => SETTLE_DURATION_MS,
            OrbPresentation::Spotlight => WAKE_HOLD_MS,
            _ => 260,
        };
        payload = Some(presentation_payload(
            app,
            &runtime,
            presentation,
            duration_ms,
        ));
    }
    if let Some(payload) = payload {
        let _ = app.emit(ORB_PRESENTATION_EVENT, payload);
    }
    sync_notch_attention_window(app);
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
    if !place_rect_atomically(window, rect) {
        let _ = window.set_position(PhysicalPosition::new(rect.x, rect.y));
        let _ = window.set_size(PhysicalSize::new(rect.width, rect.height));
    }
    // A resize can bring the panel shadow back. The ear is the only shape.
    let _ = window.set_shadow(false);
}

/// Move and resize together. Separate calls park the ears off the bezel for a
/// frame: the position lands first, while the window is still the open width.
fn place_rect_atomically(window: &tauri::WebviewWindow, rect: OrbRect) -> bool {
    #[cfg(target_os = "macos")]
    {
        use objc2::class;
        use objc2::msg_send;
        use objc2::runtime::AnyObject;
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        let thread: *mut AnyObject = unsafe { msg_send![class!(NSThread), currentThread] };
        let on_main: bool = !thread.is_null() && unsafe { msg_send![thread, isMainThread] };
        if !on_main {
            return false;
        }
        let Ok(ns_window) = window.ns_window() else {
            return false;
        };
        let Ok(current_position) = window.outer_position() else {
            return false;
        };
        let ns_window = ns_window as *mut AnyObject;
        let scale = window.scale_factor().unwrap_or(1.0).max(1.0);
        let frame: NSRect = unsafe { msg_send![ns_window, frame] };
        let dx = (rect.x - current_position.x) as f64 / scale;
        let dy_down = (rect.y - current_position.y) as f64 / scale;
        let width = rect.width as f64 / scale;
        let height = rect.height as f64 / scale;
        // Tauri's origin is the top-left and grows downward. AppKit's origin
        // is the bottom-left, so a shorter window keeps its top by lifting
        // the bottom.
        let next = NSRect::new(
            NSPoint::new(
                frame.origin.x + dx,
                frame.origin.y + frame.size.height - dy_down - height,
            ),
            NSSize::new(width, height),
        );
        // Resizing the page clears the orb for a frame. On a shrink, keep the
        // page at its open size and let the window clip to the ears.
        unsafe {
            use objc2::runtime::AnyClass;
            let content: *mut AnyObject = msg_send![ns_window, contentView];
            let shrinking = width + 0.5 < frame.size.width || height + 0.5 < frame.size.height;
            let page = hosted_page(content);
            let page_before: NSRect = if page.is_null() {
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0))
            } else {
                msg_send![page, frame]
            };
            if shrinking {
                pin_page_top_right(content);
            } else if page_covers(content, width, height) || page_is_pinned(content) {
                // An opening spring already pinned the page at the open size.
                // Filling it here would scale that page with the window, and
                // a width-only strip would grow off the right of the screen.
                pin_page_top_right(content);
            } else {
                fill_page(content);
            }
            let _: () = msg_send![ns_window, setOpaque: false];
            if let Some(color_class) = AnyClass::get(c"NSColor") {
                let color: *mut AnyObject = msg_send![color_class, clearColor];
                let _: () = msg_send![ns_window, setBackgroundColor: color];
            }
            let transaction = AnyClass::get(c"CATransaction");
            if let Some(transaction) = transaction {
                let _: () = msg_send![transaction, begin];
                let _: () = msg_send![transaction, setDisableActions: true];
            }
            let _: () = msg_send![ns_window, setFrame: next, display: false];
            if !page.is_null() {
                let now: NSRect = msg_send![page, frame];
                let shrank = now.size.width + 1.0 < page_before.size.width
                    || now.size.height + 1.0 < page_before.size.height;
                let stretched = now.size.width > page_before.size.width + 1.0
                    || now.size.height > page_before.size.height + 1.0;
                // The open page stays the size it had before this frame. A
                // resize that stretches it would reflow the drawer under the ears.
                let keep_page = shrinking || page_is_pinned(content);
                if keep_page && (shrank || stretched) {
                    let bounds: NSRect = msg_send![content, bounds];
                    let pinned = NSRect::new(
                        NSPoint::new(
                            bounds.size.width - page_before.size.width,
                            bounds.size.height - page_before.size.height,
                        ),
                        page_before.size,
                    );
                    let _: () = msg_send![page, setFrame: pinned];
                }
                if shrinking {
                    info!(
                        page_w = page_before.size.width,
                        page_h = page_before.size.height,
                        after_w = now.size.width,
                        after_h = now.size.height,
                        restored = shrank || stretched,
                        "notch page clip"
                    );
                }
            }
            if let Some(transaction) = transaction {
                let _: () = msg_send![transaction, commit];
            }
        }
        true
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (window, rect);
        false
    }
}

/// `NSViewMinXMargin | NSViewMinYMargin`: the page stays its current size,
/// stuck to the top-right, while the window changes.
#[cfg(target_os = "macos")]
const PAGE_PIN_TOP_RIGHT: usize = 1 | 8;
/// `NSViewWidthSizable | NSViewHeightSizable`: the page tracks the window.
#[cfg(target_os = "macos")]
const PAGE_FILL: usize = 2 | 16;

#[cfg(target_os = "macos")]
fn hosted_page(content: *mut objc2::runtime::AnyObject) -> *mut objc2::runtime::AnyObject {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    if content.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let subviews: *mut AnyObject = msg_send![content, subviews];
        if subviews.is_null() {
            return std::ptr::null_mut();
        }
        let count: usize = msg_send![subviews, count];
        let mut best: *mut AnyObject = std::ptr::null_mut();
        let mut best_area = 0.0;
        for index in 0..count.min(6) {
            let subview: *mut AnyObject = msg_send![subviews, objectAtIndex: index];
            if subview.is_null() {
                continue;
            }
            let frame: objc2_foundation::NSRect = msg_send![subview, frame];
            let area = frame.size.width * frame.size.height;
            if area >= best_area {
                best = subview;
                best_area = area;
            }
        }
        best
    }
}

#[cfg(target_os = "macos")]
fn pin_page_top_right(content: *mut objc2::runtime::AnyObject) {
    use objc2::msg_send;
    let page = hosted_page(content);
    if page.is_null() {
        return;
    }
    unsafe {
        let _: () = msg_send![page, setAutoresizingMask: PAGE_PIN_TOP_RIGHT];
    }
}

#[cfg(target_os = "macos")]
fn page_covers(content: *mut objc2::runtime::AnyObject, width: f64, height: f64) -> bool {
    use objc2::msg_send;
    use objc2_foundation::NSRect;
    let page = hosted_page(content);
    if page.is_null() {
        return false;
    }
    let frame: NSRect = unsafe { msg_send![page, frame] };
    frame.size.width + 1.0 >= width && frame.size.height + 1.0 >= height
}

#[cfg(target_os = "macos")]
fn page_is_pinned(content: *mut objc2::runtime::AnyObject) -> bool {
    use objc2::msg_send;
    let page = hosted_page(content);
    if page.is_null() {
        return false;
    }
    let mask: usize = unsafe { msg_send![page, autoresizingMask] };
    (mask & (PAGE_PIN_TOP_RIGHT | PAGE_FILL)) == PAGE_PIN_TOP_RIGHT
}

/// Stick a page of `width` by `height` points to the content view's top-right.
/// A page larger than the window hangs outside that corner and the window
/// clips it, so the drawer is revealed instead of scaled.
#[cfg(target_os = "macos")]
fn pin_page_to_size(content: *mut objc2::runtime::AnyObject, width: f64, height: f64) {
    use objc2::msg_send;
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    let page = hosted_page(content);
    if page.is_null() || content.is_null() {
        return;
    }
    unsafe {
        let bounds: NSRect = msg_send![content, bounds];
        let pinned = NSRect::new(
            NSPoint::new(bounds.size.width - width, bounds.size.height - height),
            NSSize::new(width.max(1.0), height.max(1.0)),
        );
        let _: () = msg_send![page, setAutoresizingMask: PAGE_PIN_TOP_RIGHT];
        let _: () = msg_send![page, setFrame: pinned];
    }
}

/// Match the page to the content view so a later resize tracks the window.
#[cfg(target_os = "macos")]
fn fill_page(content: *mut objc2::runtime::AnyObject) {
    use objc2::msg_send;
    use objc2_foundation::NSRect;
    let page = hosted_page(content);
    if page.is_null() || content.is_null() {
        return;
    }
    unsafe {
        let bounds: NSRect = msg_send![content, bounds];
        let _: () = msg_send![page, setFrame: bounds];
        let _: () = msg_send![page, setAutoresizingMask: PAGE_FILL];
    }
}

/// Drawer springs reveal a page that is already the open size, even when the
/// window is still the ear. Center and settle motions track the window,
/// because that page is a different shape.
#[cfg(target_os = "macos")]
fn prepare_page_for_motion(window: &tauri::WebviewWindow, target: OrbRect, track_window: bool) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    if !on_main_thread() {
        return;
    }
    let Ok(ns_window) = window.ns_window() else {
        return;
    };
    let ns_window = ns_window as *mut AnyObject;
    let content: *mut AnyObject = unsafe { msg_send![ns_window, contentView] };
    let scale = window.scale_factor().unwrap_or(1.0).max(1.0);
    let width = target.width as f64 / scale;
    let height = target.height as f64 / scale;
    if track_window {
        fill_page(content);
        return;
    }
    // The center card leaves the page larger than a drawer. Shrinking that
    // page is a long layout, and the open then waits and snaps. A page that
    // already covers the drawer only needs the top-right pin.
    if page_covers(content, width, height) {
        pin_page_top_right(content);
        return;
    }
    let page = hosted_page(content);
    let (pin_w, pin_h) = if page.is_null() {
        (width, height)
    } else {
        let frame: objc2_foundation::NSRect = unsafe { msg_send![page, frame] };
        (width.max(frame.size.width), height.max(frame.size.height))
    };
    pin_page_to_size(content, pin_w, pin_h);
}

#[cfg(target_os = "macos")]
fn on_main_thread() -> bool {
    use objc2::class;
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    let thread: *mut AnyObject = unsafe { msg_send![class!(NSThread), currentThread] };
    !thread.is_null() && unsafe { msg_send![thread, isMainThread] }
}

#[cfg(not(target_os = "macos"))]
fn prepare_page_for_motion(_window: &tauri::WebviewWindow, _target: OrbRect, _track_window: bool) {}

fn configure_interaction(window: &tauri::WebviewWindow, interactive: bool) {
    let _ = window.set_ignore_cursor_events(!interactive);
    configure_focusability(window, interactive);
}

/// Updating the dock must not claim the keyboard. Tao's `show()` makes even a
/// non-activating NSPanel key; use the panel's non-key ordering on AppKit's thread.
/// Explicit summons request focus separately, after presentation or animation.
fn present_orb_window(app: &AppHandle, window: &tauri::WebviewWindow) {
    #[cfg(target_os = "macos")]
    {
        use tauri_nspanel::ManagerExt;

        let Ok(panel) = app.get_webview_panel(window.label()) else {
            warn!("Cannot present notch orb: native panel is unavailable");
            return;
        };
        if let Err(error) = app.run_on_main_thread(move || panel.order_front_regardless()) {
            warn!("Cannot present notch orb on the main thread: {error}");
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        let _ = window.show();
        let _ = window.unminimize();
    }
}

fn notch_attention_is_open(app: &AppHandle) -> bool {
    !setup_blocks_orb(app)
        && app
            .state::<crate::AppState>()
            .orb_window
            .lock()
            .is_ok_and(|runtime| {
                runtime.inbox_open
                    && !runtime.intro_open
                    && runtime.presentation == OrbPresentation::Resting
            })
}

/// The drawer is the same unified-UI workspace and prompt host as the web.
/// It has its own origin-bound Tauri webview, rather than copying form types or
/// exposing the native session to an iframe. Only explicit interaction makes
/// this auxiliary panel key; feed polling merely orders it without activation.
fn sync_notch_attention_window(app: &AppHandle) {
    if !notch_attention_is_open(app) {
        if let Some(window) = app.get_webview_window(NOTCH_ATTENTION_WINDOW_LABEL) {
            let _ = window.hide();
        }
        return;
    }
    let view = app
        .state::<crate::AppState>()
        .orb_window
        .lock()
        .map(|runtime| runtime.drawer_view.clone())
        .unwrap_or_default();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let url = {
            let state = app.state::<crate::AppState>();
            let config = state.config.lock().await;
            notch_panel_url(&crate::tray::app_url_at_path(&config, "/"), &view)
        };
        let owner = app.clone();
        if let Err(error) = app.run_on_main_thread(move || {
            // A close, setup transition or voice summon may win while the
            // config read is pending. Never resurrect that stale drawer.
            if !notch_attention_is_open(&owner) {
                return;
            }
            if let Err(error) = present_notch_attention_window(&owner, &url) {
                warn!("Cannot present notch Attention: {error}");
            }
        }) {
            warn!("Cannot schedule notch Attention: {error}");
        }
    });
}

/// The side panel's page for `view` on the UI at `base`: Attention, or the
/// work list with the tapped item picked.
fn notch_panel_url(base: &str, view: &NotchDrawerView) -> String {
    let base = base.trim_end_matches('/');
    match view {
        NotchDrawerView::Attention => {
            format!("{base}/attention?native_attention=1&notch_attention=1")
        },
        NotchDrawerView::Runs { selected } => {
            let page = format!("{base}/coding-runs?native_attention=1&notch_attention=1");
            match (selected, reqwest::Url::parse(&page)) {
                (Some(id), Ok(mut url)) => {
                    let (key, value) = notch_panel_pick(id);
                    url.query_pairs_mut().append_pair(key, value);
                    url.to_string()
                },
                _ => page,
            }
        },
    }
}

/// The panel already exists: show `url` in it. Switching between Attention and
/// the runs reloads the page; picking another item in the open runs panel is
/// only a selection, sent as an event so the list keeps its place. The event
/// carries the run id for a coding run and the item id for a crew or task.
fn retarget_notch_panel(app: &AppHandle, window: &tauri::WebviewWindow, url: &str) {
    let Ok(wanted) = url.parse::<tauri::Url>() else {
        return;
    };
    let Ok(current) = window.url() else {
        return;
    };
    if current.path() != wanted.path() {
        let _ = window.navigate(wanted);
        return;
    }
    if current.query() == wanted.query() {
        return;
    }
    let pick = |name: &str| {
        wanted
            .query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    let _ = app.emit_to(
        NOTCH_ATTENTION_WINDOW_LABEL,
        NOTCH_RUN_SELECT_EVENT,
        serde_json::json!({ "runId": pick("run"), "itemId": pick("item") }),
    );
}

fn present_notch_attention_window(app: &AppHandle, url: &str) -> Result<(), String> {
    let existed = app.get_webview_window(NOTCH_ATTENTION_WINDOW_LABEL).is_some();
    let station = rect_for(app, OrbPresentation::Resting).ok_or("no display for Attention")?;
    let scale = monitor_for_orb(app)
        .map(|m| m.scale_factor())
        .unwrap_or(1.0);
    // Match the original drawer's 14pt top/bottom inset, leaving the ears clear.
    let inset = (14.0 * scale).round() as u32;
    let rect = OrbRect {
        y: station.y.saturating_add(inset as i32),
        height: station.height.saturating_sub(inset.saturating_mul(2)),
        // Leave both station controls exposed on the right bezel.
        width: station
            .width
            .saturating_sub((EDGE_PEEK_WIDTH_PT * scale).round() as u32),
        ..station
    };
    if app
        .get_webview_window(NOTCH_ATTENTION_WINDOW_LABEL)
        .is_none()
    {
        let url: tauri::Url = url
            .parse()
            .map_err(|error| format!("invalid Attention URL: {error}"))?;
        crate::magician_auth::admit_attention_ui(app, NOTCH_ATTENTION_WINDOW_LABEL, &url)?;
        #[cfg(target_os = "macos")]
        {
            use tauri::{LogicalPosition, LogicalSize, Position, Size};
            use tauri_nspanel::{CollectionBehavior, PanelBuilder, PanelLevel, StyleMask};
            PanelBuilder::<_, MagicianOrbPanel>::new(app, NOTCH_ATTENTION_WINDOW_LABEL)
                .url(tauri::WebviewUrl::External(url))
                .title("Magican — Attention")
                .position(Position::Logical(LogicalPosition::new(
                    rect.x as f64 / scale,
                    rect.y as f64 / scale,
                )))
                .size(Size::Logical(LogicalSize::new(
                    rect.width as f64 / scale,
                    rect.height as f64 / scale,
                )))
                .level(PanelLevel::Status)
                .floating(true)
                .no_activate(true)
                .transparent(true)
                .opaque(false)
                .has_shadow(false)
                .hides_on_deactivate(false)
                .becomes_key_only_if_needed(true)
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
                        .focused(false)
                        .transparent(true)
                        .shadow(false)
                        .on_navigation(|url| {
                            crate::app_surface_origin_policy::admits_webview_navigation(
                                url.as_str(),
                            )
                        })
                })
                .build()
                .map_err(|error| format!("failed to create Attention panel: {error}"))?;
        }
        #[cfg(not(target_os = "macos"))]
        {
            tauri::WebviewWindowBuilder::new(
                app,
                NOTCH_ATTENTION_WINDOW_LABEL,
                tauri::WebviewUrl::External(url),
            )
            .title("Magican — Attention")
            .decorations(false)
            .resizable(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .visible(false)
            .focused(false)
            .transparent(true)
            .shadow(false)
            .on_navigation(|url| {
                crate::app_surface_origin_policy::admits_webview_navigation(url.as_str())
            })
            .build()
            .map_err(|error| format!("failed to create Attention panel: {error}"))?;
        }
        if let Some(window) = app.get_webview_window(NOTCH_ATTENTION_WINDOW_LABEL) {
            let owner = app.clone();
            window.on_window_event(move |event| {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = notch_set_inbox_open(owner.clone(), false);
                }
            });
        }
    }
    let window = app
        .get_webview_window(NOTCH_ATTENTION_WINDOW_LABEL)
        .ok_or("Attention panel unavailable")?;
    if existed {
        retarget_notch_panel(app, &window, url);
    }
    let _ = window.set_position(PhysicalPosition::new(rect.x, rect.y));
    let _ = window.set_size(PhysicalSize::new(rect.width, rect.height));
    configure_interaction(&window, true);
    present_orb_window(app, &window);
    Ok(())
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

#[derive(Clone, Copy)]
enum MotionCurve {
    /// Center settle. No overshoot, so the orb does not bounce past the ear.
    Settle,
    /// Docked drawers. A small overshoot, then a stop on the bezel.
    Spring,
}

/// Underdamped step. `t` is 0 at the start and 1 at rest. The peak sits a
/// little past the target, then the value returns to 1.
fn drawer_spring(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    const ZETA: f64 = 0.68;
    const WN: f64 = 12.0;
    let wd = WN * (1.0 - ZETA * ZETA).sqrt();
    let envelope = (-ZETA * WN * t).exp();
    1.0 - envelope * ((ZETA * WN / wd) * (wd * t).sin() + (wd * t).cos())
}

/// Opening may travel a little past the open size. Closing stops on the bezel:
/// the spring's overshoot would shrink the window past the ear and clip it.
fn axis_progress(curve: MotionCurve, linear: f64, shrinking: bool) -> f64 {
    let linear = linear.clamp(0.0, 1.0);
    match (curve, shrinking) {
        (MotionCurve::Spring, false) => drawer_spring(linear),
        _ => ease_out_quint(linear),
    }
}

/// One sample of a move and a resize. Width shares its curve with `x`, and
/// height shares its curve with `y`, so a shared right edge stays put.
fn motion_sample(
    start_position: PhysicalPosition<i32>,
    start_size: PhysicalSize<u32>,
    target: OrbRect,
    horizontal: f64,
    vertical: f64,
) -> OrbRect {
    let x = start_position.x as f64 + (target.x - start_position.x) as f64 * horizontal;
    let y = start_position.y as f64 + (target.y - start_position.y) as f64 * vertical;
    let width =
        start_size.width as f64 + (target.width as f64 - start_size.width as f64) * horizontal;
    let height =
        start_size.height as f64 + (target.height as f64 - start_size.height as f64) * vertical;
    OrbRect {
        x: x.round() as i32,
        y: y.round() as i32,
        width: width.round().max(1.0) as u32,
        height: height.round().max(1.0) as u32,
    }
}

fn begin_motion(app: &AppHandle, target: OrbRect) -> u64 {
    let state = app.state::<crate::AppState>();
    let Ok(mut runtime) = state.orb_window.lock() else {
        return 0;
    };
    runtime.motion_generation = runtime.motion_generation.saturating_add(1);
    runtime.motion_active = true;
    runtime.motion_target = Some(target);
    runtime.motion_generation
}

fn motion_is_current(app: &AppHandle, generation: u64) -> bool {
    app.state::<crate::AppState>()
        .orb_window
        .lock()
        .map(|runtime| runtime.motion_generation == generation)
        .unwrap_or(false)
}

fn end_motion(app: &AppHandle, generation: u64) {
    let state = app.state::<crate::AppState>();
    let Ok(mut runtime) = state.orb_window.lock() else {
        return;
    };
    if runtime.motion_generation == generation {
        runtime.motion_active = false;
        runtime.motion_target = None;
    }
}

fn cancel_motion(app: &AppHandle) {
    let state = app.state::<crate::AppState>();
    let Ok(mut runtime) = state.orb_window.lock() else {
        return;
    };
    runtime.motion_generation = runtime.motion_generation.saturating_add(1);
    runtime.motion_active = false;
    runtime.motion_target = None;
}

/// One queued turn of the main run loop. `run_on_main_thread` runs inline
/// when already on that thread, so a chain of those calls never draws.
#[cfg(target_os = "macos")]
fn dispatch_main_after(delay: Duration, work: impl FnOnce() + Send + 'static) {
    use block2::RcBlock;
    use std::sync::Mutex;
    extern "C" {
        // `dispatch_get_main_queue()` is a macro for the address of this object.
        static _dispatch_main_q: u8;
        fn dispatch_time(when: u64, delta: i64) -> u64;
        fn dispatch_after(when: u64, queue: *mut std::ffi::c_void, block: *mut std::ffi::c_void);
    }
    let work = Mutex::new(Some(work));
    let block = RcBlock::new(move || {
        if let Some(work) = work.lock().ok().and_then(|mut guard| guard.take()) {
            work();
        }
    });
    let nanos = i64::try_from(delay.as_nanos()).unwrap_or(i64::MAX);
    unsafe {
        let when = dispatch_time(0, nanos);
        let queue = std::ptr::addr_of!(_dispatch_main_q).cast_mut().cast();
        dispatch_after(when, queue, RcBlock::as_ptr(&block).cast());
    }
}

struct MotionRun {
    app: AppHandle,
    start_position: PhysicalPosition<i32>,
    start_size: PhysicalSize<u32>,
    target: OrbRect,
    motion: u64,
    generation: u64,
    focus: bool,
    terminal_presentation: Option<OrbPresentation>,
    /// Steps already drawn. The clock is the frame, not the moment the click
    /// landed: a busy main thread must not skip the slide and snap to the end.
    frame: u32,
    frames: u32,
    curve: MotionCurve,
    width_shrinks: bool,
    height_shrinks: bool,
}

fn motion_linear(run: &MotionRun) -> f64 {
    (run.frame as f64 / run.frames.max(1) as f64).min(1.0)
}

fn land_motion(run: &MotionRun) {
    if !motion_is_current(&run.app, run.motion) {
        return;
    }
    if !animation_is_current(&run.app, run.generation) {
        end_motion(&run.app, run.motion);
        return;
    }
    end_motion(&run.app, run.motion);
    let Some(window) = run.app.get_webview_window(ORB_WINDOW_LABEL) else {
        return;
    };
    apply_rect(&run.app, &window, run.target);
    if let Some(presentation) = run.terminal_presentation {
        set_presentation_state(&run.app, presentation);
    }
    if run.focus {
        let _ = window.set_focus();
    }
}

#[cfg(target_os = "macos")]
fn step_motion(mut run: MotionRun) {
    if !motion_is_current(&run.app, run.motion) {
        return;
    }
    if !animation_is_current(&run.app, run.generation) {
        end_motion(&run.app, run.motion);
        return;
    }
    let linear = motion_linear(&run);
    let horizontal = axis_progress(run.curve, linear, run.width_shrinks);
    let vertical = axis_progress(run.curve, linear, run.height_shrinks);
    let sample = motion_sample(
        run.start_position,
        run.start_size,
        run.target,
        horizontal,
        vertical,
    );
    if let Some(window) = run.app.get_webview_window(ORB_WINDOW_LABEL) {
        apply_rect(&run.app, &window, sample);
    }
    if linear >= 1.0 {
        land_motion(&run);
        return;
    }
    run.frame = run.frame.saturating_add(1);
    dispatch_main_after(Duration::from_millis(16), move || step_motion(run));
}

/// Read the window frame, pin the page, then start the spring.
///
/// A timer queued while the page is being laid out runs inside that layout.
/// The window server then shows only the last rect: a pause, then a snap.
#[cfg(target_os = "macos")]
fn begin_motion_on_main(mut run: MotionRun) {
    if !motion_is_current(&run.app, run.motion) {
        return;
    }
    if !animation_is_current(&run.app, run.generation) {
        end_motion(&run.app, run.motion);
        return;
    }
    if let Some(window) = run.app.get_webview_window(ORB_WINDOW_LABEL) {
        if let Ok(position) = window.outer_position() {
            run.start_position = position;
        }
        if let Ok(size) = window.outer_size() {
            run.start_size = size;
        }
        run.width_shrinks = run.target.width + 2 < run.start_size.width;
        run.height_shrinks = run.target.height + 2 < run.start_size.height;
        prepare_page_for_motion(
            &window,
            run.target,
            !matches!(run.curve, MotionCurve::Spring),
        );
        let _ = window.set_shadow(false);
        info!(
            start_w = run.start_size.width,
            start_h = run.start_size.height,
            target_w = run.target.width,
            target_h = run.target.height,
            "notch drawer spring"
        );
    }
    dispatch_main_after(Duration::from_millis(16), move || step_motion(run));
}

fn schedule_motion_frames(run: MotionRun) {
    #[cfg(target_os = "macos")]
    {
        // The next turn draws this frame. A burst of setFrames in one turn
        // collapses to the final rect, which is the pause-then-snap.
        dispatch_main_after(Duration::ZERO, move || begin_motion_on_main(run));
    }
    #[cfg(not(target_os = "macos"))]
    {
        tauri::async_runtime::spawn(async move {
            let mut run = run;
            loop {
                tokio::time::sleep(Duration::from_millis(16)).await;
                if !motion_is_current(&run.app, run.motion) {
                    return;
                }
                if !animation_is_current(&run.app, run.generation) {
                    end_motion(&run.app, run.motion);
                    return;
                }
                let linear = motion_linear(&run);
                let horizontal = axis_progress(run.curve, linear, run.width_shrinks);
                let vertical = axis_progress(run.curve, linear, run.height_shrinks);
                let sample = motion_sample(
                    run.start_position,
                    run.start_size,
                    run.target,
                    horizontal,
                    vertical,
                );
                if let Some(window) = run.app.get_webview_window(ORB_WINDOW_LABEL) {
                    apply_rect(&run.app, &window, sample);
                }
                if linear >= 1.0 {
                    break;
                }
                run.frame = run.frame.saturating_add(1);
            }
            land_motion(&run);
        });
    }
}

fn window_has_rect(window: &tauri::WebviewWindow, rect: OrbRect) -> bool {
    let Ok(position) = window.outer_position() else {
        return false;
    };
    // The web page stays the open size while the window clips to the ears.
    // The page's size is not the window.
    let Ok(size) = window.outer_size() else {
        return false;
    };
    (position.x - rect.x).abs() <= 2
        && (position.y - rect.y).abs() <= 2
        && (size.width as i32 - rect.width as i32).abs() <= 2
        && (size.height as i32 - rect.height as i32).abs() <= 2
}

fn animate_to(
    app: AppHandle,
    target: OrbRect,
    generation: u64,
    focus: bool,
    terminal_presentation: Option<OrbPresentation>,
    duration_ms: u64,
    curve: MotionCurve,
) {
    let Some(window) = app.get_webview_window(ORB_WINDOW_LABEL) else {
        return;
    };
    let motion = begin_motion(&app, target);
    if motion == 0 {
        return;
    }
    let start_position = window
        .outer_position()
        .unwrap_or(PhysicalPosition::new(target.x, target.y));
    // The web page stays large after the center card while the window is the
    // ear. Measuring the page made the open take the close path.
    let start_size = window
        .outer_size()
        .unwrap_or(PhysicalSize::new(target.width, target.height));
    remember_programmatic_rect(&app, target);
    let _ = window.set_shadow(false);
    // Width and its origin share one curve, and so do height and its origin.
    // Each frame is its own turn, so the window server can show it.
    let frames = (duration_ms.max(16) / 16).max(1) as u32;
    schedule_motion_frames(MotionRun {
        app,
        start_position,
        start_size,
        target,
        motion,
        generation,
        focus,
        terminal_presentation,
        frame: 1,
        frames,
        curve,
        width_shrinks: target.width < start_size.width,
        height_shrinks: target.height < start_size.height,
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
        let (presentation, programmatic_rect, notch, edge) = {
            let app_state = app.state::<crate::AppState>();
            let Ok(runtime) = app_state.orb_window.lock() else {
                return;
            };
            if runtime.move_revision != revision || runtime.intro_open || runtime.motion_active {
                return;
            }
            (
                runtime.presentation,
                runtime.last_programmatic_rect,
                runtime.notch,
                edge_frame(&runtime),
            )
        };
        // Spotlight, settling, and a leftover expanded frame must not
        // overwrite the ear's height. Only a resting drag is a placement.
        if presentation != OrbPresentation::Resting {
            return;
        }
        let Some(window) = app.get_webview_window(ORB_WINDOW_LABEL) else {
            return;
        };
        let Ok(position) = window.outer_position() else {
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
        let mut edge = edge;
        // The station only lives on the right edge. A drag along that edge
        // changes the height. A drag that leaves the edge snaps back flush
        // at the dropped height. It never grows into a floating pill. The
        // saved fraction uses the closed station, so an open drawer does not
        // move the ears when it closes.
        let mut station_edge = edge;
        station_edge.inbox_open = false;
        station_edge.voice_open = false;
        let probe = rect_for_monitor(
            monitor_position,
            monitor_size,
            monitor_scale,
            notch,
            presentation,
            None,
            false,
            false,
            station_edge,
        );
        let fraction = dock_y_fraction(
            position.y,
            monitor_position.y,
            monitor_size.height,
            probe.height,
            monitor_scale,
            notch,
        );
        edge.dock_y = (fraction > 0.0).then_some(fraction);
        let dock_rect = rect_for_monitor(
            monitor_position,
            monitor_size,
            monitor_scale,
            notch,
            presentation,
            None,
            false,
            false,
            edge,
        );
        {
            let app_state = app.state::<crate::AppState>();
            let Ok(mut runtime) = app_state.orb_window.lock() else {
                return;
            };
            if runtime.move_revision != revision {
                return;
            }
            runtime.resting_placement = None;
            runtime.dock_y = edge.dock_y;
            runtime.last_programmatic_rect = Some(dock_rect);
        }
        let config_to_save = {
            let state = app.state::<crate::AppState>();
            let mut config = state.config.lock().await;
            config.orb.resting_x = None;
            config.orb.resting_y = None;
            config.orb.dock_y = edge.dock_y;
            config.clone()
        };
        apply_rect(&app, &window, dock_rect);
        set_presentation_state(&app, presentation);
        if let Err(error) = crate::config::save_config(&config_to_save) {
            warn!(error = %error, "Failed to persist Ambient Orb position");
        } else {
            info!(
                ?presentation,
                dock_y = edge.dock_y,
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
        .inner_size(EDGE_PEEK_WIDTH_PT, NOTCH_STACK_HEIGHT_PT)
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
    present_orb_window(app, &window);
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
    present_orb_window(app, &window);
    configure_interaction(&window, true);
    animate_to(
        app.clone(),
        target,
        generation,
        focus,
        None,
        SETTLE_DURATION_MS,
        MotionCurve::Settle,
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
        MotionCurve::Settle,
    );
}

pub fn show_resting(app: &AppHandle) {
    show_at(app, OrbPresentation::Resting, false);
}

/// The center Orb is the conversation. A strip the first click already
/// opened does not stay open across that trip.
fn close_voice_strip(app: &AppHandle) {
    if let Ok(mut runtime) = app.state::<crate::AppState>().orb_window.lock() {
        runtime.voice_drawer_open = false;
        runtime.voice_drawer_hold_closed = false;
        runtime.voice_drawer_manual = false;
    }
}

pub fn expand(app: &AppHandle) {
    show_voice_control(app);
    close_voice_strip(app);
    animate_at(app, OrbPresentation::Spotlight, true);
}

/// Center surface without taking keyboard focus. Docked talk uses the
/// transcript strip instead, so nothing calls this now. The body stays so the
/// center-size contract can still see it.
#[allow(dead_code)]
fn expand_without_focus(app: &AppHandle) {
    show_voice_control(app);
    close_voice_strip(app);
    show_at(app, OrbPresentation::Spotlight, false);
}

pub fn collapse(app: &AppHandle) {
    settle_to_resting(app, false);
}

pub fn spotlight(app: &AppHandle) {
    show_voice_control(app);
    close_voice_strip(app);
    animate_at(app, OrbPresentation::Spotlight, true);
}

pub fn toggle(app: &AppHandle) {
    if setup_blocks_orb(app) {
        hide(app);
        return;
    }
    let (expanded, presentation) = app
        .state::<crate::AppState>()
        .orb_window
        .lock()
        .map(|runtime| (runtime.expanded, runtime.presentation))
        .unwrap_or((false, OrbPresentation::Hidden));
    // The chord shows the docked ear, and settles the large Orb. Growing
    // the ear belongs to a double-click.
    if !voice_control_is_visible(app) {
        show_voice_control(app);
    } else if expanded {
        collapse(app);
    } else if presentation != OrbPresentation::Resting {
        show_resting(app);
    }
}

pub fn hide(app: &AppHandle) {
    set_presentation_state(app, OrbPresentation::Hidden);
    if let Some(window) = app.get_webview_window(ORB_WINDOW_LABEL) {
        let _ = window.hide();
    }
}

pub fn voice_control_is_visible(app: &AppHandle) -> bool {
    !setup_blocks_orb(app)
        && app
            .state::<crate::AppState>()
            .orb_window
            .lock()
            .map(|runtime| runtime.voice_visible)
            .unwrap_or(false)
}

fn set_voice_visibility(app: &AppHandle, visible: bool) {
    if let Ok(mut runtime) = app.state::<crate::AppState>().orb_window.lock() {
        runtime.set_voice_visible(visible);
    }
    release_voice_window(app);
    crate::tray::refresh_menu(app, None);
}

/// An explicit talk or summon reveals the control without changing capture.
pub fn show_voice_control(app: &AppHandle) {
    if !setup_blocks_orb(app) && !voice_control_is_visible(app) {
        set_voice_visibility(app, true);
    }
}

/// The menu action changes only the voice surface; the tray ear stays available.
pub fn toggle_voice_visibility(app: &AppHandle) {
    if let Err(error) = require_setup_ready(app) {
        warn!("Cannot toggle the Orb: {error}");
        return;
    }
    set_voice_visibility(app, !voice_control_is_visible(app));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdleStation {
    Hidden,
    VoiceAndTray,
    TrayOnly,
}

/// Dock visibility belongs to the user's visibility choice, not the listening lease.
/// Ending capture must leave the enabled station available for the next hold.
fn idle_station(setup_blocked: bool, voice_visible: bool) -> IdleStation {
    if setup_blocked {
        IdleStation::Hidden
    } else if voice_visible {
        IdleStation::VoiceAndTray
    } else {
        IdleStation::TrayOnly
    }
}

/// Release voice capture without discarding the dock or a waiting question.
fn release_voice_window(app: &AppHandle) {
    let station = idle_station(setup_blocks_orb(app), voice_control_is_visible(app));
    match station {
        IdleStation::Hidden => {
            hide(app);
        },
        IdleStation::VoiceAndTray => show_resting(app),
        IdleStation::TrayOnly => show_at(app, OrbPresentation::Resting, false),
    }
}

fn reapply_resting_rect(app: &AppHandle) {
    let presentation = app
        .state::<crate::AppState>()
        .orb_window
        .lock()
        .map(|runtime| runtime.presentation)
        .unwrap_or(OrbPresentation::Hidden);
    if presentation != OrbPresentation::Resting {
        return;
    }
    let Some(window) = app.get_webview_window(ORB_WINDOW_LABEL) else {
        return;
    };
    let Some(rect) = rect_for(app, presentation) else {
        return;
    };
    let (reduced, animation_generation, same_motion) = {
        let state = app.state::<crate::AppState>();
        let Ok(runtime) = state.orb_window.lock() else {
            return;
        };
        (
            runtime.reduced_motion,
            runtime.animation_generation,
            runtime.motion_active && runtime.motion_target == Some(rect),
        )
    };
    if same_motion {
        return;
    }
    // Reduced motion, and a rect that is already the station, land at once.
    // A second publish of the same quiet list must not replay the spring.
    if reduced || window_has_rect(&window, rect) {
        cancel_motion(app);
        apply_rect(app, &window, rect);
        present_orb_window(app, &window);
        return;
    }
    // A closing panel keeps the open window until it has slid off the screen.
    // Shrinking now would cut the panel off at the ear and then drop it.
    // The page can be larger than the ear. Only the window frame says whether
    // this change is a close (slide, then one step back) or an open (spring).
    let shrinking = window
        .outer_size()
        .is_ok_and(|size| rect.width + 2 < size.width || rect.height + 2 < size.height);
    if shrinking {
        let motion = begin_motion(app, rect);
        if motion == 0 {
            return;
        }
        remember_programmatic_rect(app, rect);
        let _ = window.set_shadow(false);
        present_orb_window(app, &window);
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(DRAWER_EXIT_MS)).await;
            if !motion_is_current(&app, motion) {
                return;
            }
            if !animation_is_current(&app, animation_generation) {
                end_motion(&app, motion);
                return;
            }
            let app_main = app.clone();
            let queued = app.run_on_main_thread(move || {
                if !motion_is_current(&app_main, motion) {
                    return;
                }
                if !animation_is_current(&app_main, animation_generation) {
                    end_motion(&app_main, motion);
                    return;
                }
                end_motion(&app_main, motion);
                let Some(window) = app_main.get_webview_window(ORB_WINDOW_LABEL) else {
                    return;
                };
                apply_rect(&app_main, &window, rect);
            });
            if queued.is_err() {
                end_motion(&app, motion);
            }
        });
        return;
    }
    present_orb_window(app, &window);
    animate_to(
        app.clone(),
        rect,
        animation_generation,
        false,
        None,
        DRAWER_SPRING_MS,
        MotionCurve::Spring,
    );
}

fn setup_blocks_orb(app: &AppHandle) -> bool {
    app.state::<crate::AppState>()
        .orb_setup_blocked
        .load(Ordering::Acquire)
}

/// An explicit summon must explain the setup gate instead of looking like a
/// broken shortcut. Reuse the guided setup window without granting capabilities
/// or starting a conversation before setup completes.
pub(crate) fn require_setup_ready(app: &AppHandle) -> Result<(), String> {
    if !setup_blocks_orb(app) {
        return Ok(());
    }
    crate::setup::show_setup_window_mode(app, Some("capabilities"))?;
    Err("Finish Magican setup before enabling the Orb".to_string())
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
/// The enabled station remains available once that settle finishes.
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
            release_voice_window(&app);
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
    present_orb_window(app, &window);
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

fn conversation_is_live(snapshot: &OrbSnapshot) -> bool {
    matches!(
        snapshot.phase,
        Some(OrbPhase::Heard | OrbPhase::Listening | OrbPhase::Thinking | OrbPhase::Speaking)
    )
}

/// Open the short transcript strip on the docked ear. The center Orb is left
/// alone: a double-click is what puts the conversation there.
fn reveal_docked_voice_drawer(app: &AppHandle) {
    let presentation = {
        let state = app.state::<crate::AppState>();
        let Ok(mut runtime) = state.orb_window.lock() else {
            return;
        };
        if !runtime.voice_visible || runtime.voice_drawer_hold_closed {
            return;
        }
        if matches!(
            runtime.presentation,
            OrbPresentation::Spotlight | OrbPresentation::Settling | OrbPresentation::Expanded
        ) {
            return;
        }
        let already = runtime.voice_drawer_open && runtime.presentation == OrbPresentation::Resting;
        runtime.voice_drawer_open = true;
        // Talk opens the strip. A tap that already opened it keeps that choice.
        if !already {
            runtime.voice_drawer_manual = false;
        }
        runtime.voice_drawer_idle_generation =
            runtime.voice_drawer_idle_generation.saturating_add(1);
        if already {
            return;
        }
        runtime.presentation
    };
    emit_current_presentation(app);
    if presentation == OrbPresentation::Hidden {
        show_at(app, OrbPresentation::Resting, false);
    } else {
        reapply_resting_rect(app);
    }
}

fn arm_voice_drawer_idle(app: &AppHandle) {
    let generation = {
        let state = app.state::<crate::AppState>();
        let Ok(mut runtime) = state.orb_window.lock() else {
            return;
        };
        if !runtime.voice_drawer_open || runtime.voice_drawer_manual {
            return;
        }
        runtime.voice_drawer_idle_generation =
            runtime.voice_drawer_idle_generation.saturating_add(1);
        runtime.voice_drawer_idle_generation
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(VOICE_DRAWER_IDLE_MS)).await;
        if conversation_is_live(&current_snapshot(&app)) {
            return;
        }
        let close = {
            let state = app.state::<crate::AppState>();
            let Ok(mut runtime) = state.orb_window.lock() else {
                return;
            };
            if runtime.voice_drawer_idle_generation != generation
                || !runtime.voice_drawer_open
                || runtime.voice_drawer_manual
            {
                return;
            }
            runtime.voice_drawer_open = false;
            runtime.voice_drawer_hold_closed = false;
            runtime.voice_drawer_manual = false;
            true
        };
        if close {
            emit_current_presentation(&app);
            reapply_resting_rect(&app);
        }
    });
}

/// Talking from the dock opens the transcript strip. Five quiet seconds, with
/// nobody listening and nobody speaking, close a strip that talk opened. A
/// strip opened by a tap stays until the next tap.
fn sync_docked_voice_drawer(app: &AppHandle, before: &OrbSnapshot, after: &OrbSnapshot) {
    if setup_blocks_orb(app) {
        return;
    }
    if conversation_is_live(after) {
        let auto_expand = {
            let state = app.state::<crate::AppState>();
            let Ok(mut runtime) = state.orb_window.lock() else {
                return;
            };
            if !conversation_is_live(before) {
                runtime.voice_drawer_hold_closed = false;
            }
            runtime.voice_drawer_idle_generation =
                runtime.voice_drawer_idle_generation.saturating_add(1);
            runtime.auto_expand_on_wake
        };
        if auto_expand {
            present_wake(app);
        }
        return;
    }
    if after.state == "recoverable_error" && before.state != "recoverable_error" {
        let state = app.state::<crate::AppState>();
        if let Ok(mut runtime) = state.orb_window.lock() {
            runtime.voice_drawer_hold_closed = false;
        }
        reveal_docked_voice_drawer(app);
    }
    arm_voice_drawer_idle(app);
}

pub fn present_wake(app: &AppHandle) {
    // A wake stays on the docked ear. Captions live in the transcript strip.
    reveal_docked_voice_drawer(app);
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
        None => release_voice_window(app),
        Some(OrbPhase::Armed) if after.state == "recoverable_error" => {
            // The status line stays on the ear, in the transcript strip.
        },
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
                if !auto_expand {
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
        },
        Some(OrbPhase::Ended) if matches!(after.state, "paused" | "voice_busy") => {
            show_resting(app)
        },
        Some(OrbPhase::Ended) => {},
    }

    sync_docked_voice_drawer(app, before, after);

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
        info!(
            ?reason,
            revision = after.revision,
            "Orb voice lifecycle ended"
        );
        // A user disarm or bounded listening cap stops the voice lifecycle, not
        // the independently enabled wake spotter. Re-open that cold-start boundary
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
        let ended_revision = after.revision;
        let presentation_generation = app
            .state::<crate::AppState>()
            .orb_window
            .lock()
            .map(|runtime| runtime.animation_generation)
            .unwrap_or(0);
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1_350)).await;
            if terminal_settle_is_current(&current_snapshot(&app), ended_revision)
                && animation_is_current(&app, presentation_generation)
            {
                release_voice_window(&app);
            }
        });
    }
}

fn terminal_settle_is_current(snapshot: &OrbSnapshot, ended_revision: u64) -> bool {
    snapshot.state == "ended" && snapshot.revision == ended_revision
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
    let was_enabled = app
        .state::<crate::AppState>()
        .orb_enabled
        .swap(config.enabled, Ordering::AcqRel);
    if let Ok(mut runtime) = app.state::<crate::AppState>().orb_window.lock() {
        // Unrelated saves must not undo a session-local Show/Hide choice.
        if was_enabled != config.enabled {
            runtime.set_voice_visible(config.enabled);
        }
        runtime.auto_expand_on_wake = config.auto_expand_on_wake;
        runtime.resting_placement = placement_from_config(config.resting_x, config.resting_y);
        runtime.expanded_placement = placement_from_config(config.expanded_x, config.expanded_y);
        runtime.dock_y = config.dock_y.filter(|value| value.is_finite());
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
    if was_enabled != config.enabled {
        release_voice_window(app);
    }
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
            release_voice_window(app);
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
            release_voice_window(app);
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
    let mode = runtime.presentation;
    Ok(presentation_payload(&app, &runtime, mode, 0))
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
        config.orb.dock_y = None;
        crate::config::save_config(&config)?;
    }
    if let Ok(mut runtime) = app.state::<crate::AppState>().orb_window.lock() {
        runtime.resting_placement = None;
        runtime.expanded_placement = None;
        runtime.dock_y = None;
        runtime.edge_open = false;
    }
    settle_to_resting(&app, false);
    Ok(())
}

/// Kept so older callers still resolve. The ear no longer widens: both
/// states stay the docked side notch.
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
    animate_to(
        app,
        target,
        generation,
        false,
        None,
        EDGE_SLIDE_MS,
        MotionCurve::Settle,
    );
    Ok(())
}

#[tauri::command]
pub fn orb_collapse(app: AppHandle) -> Result<(), String> {
    collapse(&app);
    Ok(())
}

#[tauri::command]
pub async fn orb_start_conversation(app: AppHandle) -> Result<(), String> {
    require_setup_ready(&app)?;
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
    set_voice_visibility(&app, false);
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
    require_setup_ready(&app)?;
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
    show_voice_control(&app);
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

fn orb_lock(
    state: &crate::AppState,
) -> Result<std::sync::MutexGuard<'_, OrbWindowRuntime>, String> {
    state
        .orb_window
        .lock()
        .map_err(|_| "orb window state is unavailable".to_string())
}

/// The notify overlay publishes the Attention questions the side notch shows.
/// A newly arrived id opens the panel. Publishing the same ids again does not
/// reopen a panel the user closed. The last question leaving closes the
/// panel. An empty list, on its own, leaves a quiet drawer the user opened.
#[tauri::command]
pub fn publish_notch_inbox(
    app: AppHandle,
    items: Vec<NotchInboxItem>,
    total_count: Option<u32>,
) -> Result<(), String> {
    let (outgoing, attention_only, presentation, intro_open) = {
        let state = app.state::<crate::AppState>();
        let mut runtime = orb_lock(&state)?;
        let previous: std::collections::HashSet<String> = runtime
            .inbox_items
            .iter()
            .map(|item| item.id.clone())
            .collect();
        let count = total_count.unwrap_or(items.len().min(u32::MAX as usize) as u32);
        let grew = count > runtime.attention_count
            || items.iter().any(|item| !previous.contains(&item.id));
        let became_empty = count == 0 && runtime.attention_count > 0;
        runtime.attention_count = count;
        runtime.inbox_items = items;
        if became_empty {
            runtime.inbox_open = false;
        } else if grew {
            runtime.inbox_open = true;
        }
        (
            runtime.inbox_items.clone(),
            !runtime.voice_visible,
            runtime.presentation,
            runtime.intro_open,
        )
    };
    let _ = app.emit(
        NOTCH_INBOX_EVENT,
        NotchInboxPayload {
            items: outgoing.clone(),
        },
    );
    emit_current_presentation(&app);
    if intro_open {
        return Ok(());
    }
    if total_count.unwrap_or(outgoing.len().min(u32::MAX as usize) as u32) == 0 {
        if attention_only {
            release_voice_window(&app);
        } else if presentation == OrbPresentation::Resting {
            reapply_resting_rect(&app);
        }
        return Ok(());
    }
    if matches!(
        presentation,
        OrbPresentation::Expanded | OrbPresentation::Spotlight | OrbPresentation::Settling
    ) {
        return Ok(());
    }
    // A live voice phase keeps the Orb. The ear appears only once voice is gone
    // or the window was already an Attention-only notch.
    if current_snapshot(&app).phase.is_none() || attention_only {
        release_voice_window(&app);
        return Ok(());
    }
    if presentation == OrbPresentation::Resting {
        reapply_resting_rect(&app);
    }
    Ok(())
}

/// The notify overlay publishes the work items the runs ear paints as dots.
/// Dots never open the drawer; the ear grows or shrinks by a row as runs come
/// and go, and the station's top stays where it is.
#[tauri::command]
pub fn publish_notch_runs(app: AppHandle, runs: Vec<NotchRunDot>) -> Result<(), String> {
    let (rows_changed, resting) = {
        let state = app.state::<crate::AppState>();
        let mut runtime = orb_lock(&state)?;
        let runs: Vec<NotchRunDot> = runs.into_iter().take(NOTCH_RUN_DOT_LIMIT).collect();
        if runtime.run_dots == runs {
            return Ok(());
        }
        let rows = |dots: &[NotchRunDot]| dots.len().min(RUN_EAR_MAX_ROWS);
        let rows_changed = rows(&runtime.run_dots) != rows(&runs);
        if runs.is_empty() {
            runtime.run_label_open = false;
        }
        runtime.run_dots = runs;
        (rows_changed, runtime.presentation == OrbPresentation::Resting)
    };
    emit_current_presentation(&app);
    ensure_notch_pointer_poll(&app);
    // The runs ear grows and shrinks with the runs; the window follows it.
    if rows_changed && resting {
        reapply_resting_rect(&app);
    }
    Ok(())
}

/// The notch station never takes key status, and macOS sends a web view
/// mouse-moved events only while its app is active: hovering a run dot from
/// another app showed no label (it did while a button was held, because drags
/// are delivered). While the runs ear has dots, read the cursor every
/// [`NOTCH_POINTER_POLL_MS`] and send its position over the station to the
/// page ([`NOTCH_POINTER_EVENT`]), which hit-tests the dots itself. The poll
/// ends when the ear has no dots.
fn ensure_notch_pointer_poll(app: &AppHandle) {
    static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("notch-pointer-poll".to_owned())
        .spawn(move || {
            let mut last: Option<(i32, i32)> = None;
            loop {
                std::thread::sleep(Duration::from_millis(NOTCH_POINTER_POLL_MS));
                let has_dots = app
                    .state::<crate::AppState>()
                    .orb_window
                    .lock()
                    .map(|runtime| !runtime.run_dots.is_empty())
                    .unwrap_or(false);
                let Some(window) = app.get_webview_window(ORB_WINDOW_LABEL) else {
                    break;
                };
                if !has_dots {
                    if last.is_some() {
                        let _ = window.emit_to(
                            ORB_WINDOW_LABEL,
                            NOTCH_POINTER_EVENT,
                            serde_json::json!({ "inside": false }),
                        );
                    }
                    break;
                }
                #[cfg(target_os = "macos")]
                let Some(point) = cursor_over_notch_page(&app, &window) else {
                    continue;
                };
                #[cfg(not(target_os = "macos"))]
                let point = {
                    let (Ok(cursor), Ok(origin), Ok(size), Ok(scale)) = (
                        app.cursor_position(),
                        window.outer_position(),
                        window.outer_size(),
                        window.scale_factor(),
                    ) else {
                        continue;
                    };
                    let x = cursor.x - f64::from(origin.x);
                    let y = cursor.y - f64::from(origin.y);
                    let inside = x >= 0.0
                        && y >= 0.0
                        && x < f64::from(size.width)
                        && y < f64::from(size.height);
                    inside.then(|| ((x / scale).round() as i32, (y / scale).round() as i32))
                };
                if point == last {
                    continue;
                }
                last = point;
                let payload = match point {
                    Some((x, y)) => serde_json::json!({ "inside": true, "x": x, "y": y }),
                    None => serde_json::json!({ "inside": false }),
                };
                let _ = window.emit_to(ORB_WINDOW_LABEL, NOTCH_POINTER_EVENT, payload);
            }
            RUNNING.store(false, Ordering::SeqCst);
            // Dots that arrived as the poll was ending would otherwise wait
            // for the next change to restart it.
            let has_dots = app
                .state::<crate::AppState>()
                .orb_window
                .lock()
                .map(|runtime| !runtime.run_dots.is_empty())
                .unwrap_or(false);
            if has_dots {
                ensure_notch_pointer_poll(&app);
            }
        });
    if spawned.is_err() {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

/// The cursor in the notch page's own coordinates (CSS px), or `None` when it
/// is not over the visible page; the outer `None` means it could not be read
/// this tick. The page is a fixed-size view pinned top-right inside a window
/// that clips it (`pin_page_top_right`), so window-relative coordinates are off
/// by the page's offset whenever the window is narrower than the page — as it
/// is while a run label is open, which closed the label as soon as it opened.
/// AppKit converts the screen point into the page view itself.
#[cfg(target_os = "macos")]
fn cursor_over_notch_page(
    app: &AppHandle,
    window: &tauri::WebviewWindow,
) -> Option<Option<(i32, i32)>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let window = window.clone();
    app.run_on_main_thread(move || {
        let _ = sender.send(cursor_in_notch_page_on_main(&window));
    })
    .ok()?;
    receiver.recv_timeout(Duration::from_millis(250)).ok()
}

#[cfg(target_os = "macos")]
fn cursor_in_notch_page_on_main(window: &tauri::WebviewWindow) -> Option<(i32, i32)> {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::{NSPoint, NSRect};
    let ns_window = window.ns_window().ok()? as *mut AnyObject;
    if ns_window.is_null() {
        return None;
    }
    unsafe {
        let content: *mut AnyObject = msg_send![ns_window, contentView];
        let page = hosted_page(content);
        if content.is_null() || page.is_null() {
            return None;
        }
        let on_screen: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let in_window: NSPoint = msg_send![ns_window, convertPointFromScreen: on_screen];
        // Only the part of the page the window shows counts.
        let visible: NSRect = msg_send![content, bounds];
        if in_window.x < 0.0
            || in_window.y < 0.0
            || in_window.x >= visible.size.width
            || in_window.y >= visible.size.height
        {
            return None;
        }
        let nil: *mut AnyObject = std::ptr::null_mut();
        let in_page: NSPoint = msg_send![page, convertPoint: in_window, fromView: nil];
        let bounds: NSRect = msg_send![page, bounds];
        let flipped: bool = msg_send![page, isFlipped];
        let x = in_page.x - bounds.origin.x;
        let y = if flipped {
            in_page.y - bounds.origin.y
        } else {
            bounds.size.height - (in_page.y - bounds.origin.y)
        };
        if x < 0.0 || y < 0.0 || x >= bounds.size.width || y >= bounds.size.height {
            return None;
        }
        Some((x.round() as i32, y.round() as i32))
    }
}

/// The pointer reached a run dot (`open`) or left the runs ear. The closed
/// station widens to the left so the dot's label pill can slide out, as
/// getone.one labels the agent under the pointer.
#[tauri::command]
pub fn notch_set_run_label(app: AppHandle, open: bool) -> Result<(), String> {
    let changed = {
        let state = app.state::<crate::AppState>();
        let mut runtime = orb_lock(&state)?;
        let open = open && !runtime.inbox_open && !runtime.voice_drawer_open;
        let changed = runtime.run_label_open != open;
        runtime.run_label_open = open;
        changed && runtime.presentation == OrbPresentation::Resting
    };
    if changed {
        reapply_resting_rect(&app);
    }
    Ok(())
}

/// A run dot was tapped: open the side panel on the work list with that item
/// picked (`None` opens the list, as the overflow row does). `item_id` is the
/// dot's work item id; `run_id` is the older name, still taken.
#[tauri::command]
pub fn notch_show_runs(
    app: AppHandle,
    run_id: Option<String>,
    item_id: Option<String>,
) -> Result<(), String> {
    let presentation = {
        let state = app.state::<crate::AppState>();
        let mut runtime = orb_lock(&state)?;
        runtime.inbox_open = true;
        runtime.run_label_open = false;
        runtime.drawer_view = NotchDrawerView::Runs {
            selected: item_id
                .or(run_id)
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty()),
        };
        runtime.presentation
    };
    emit_current_presentation(&app);
    if presentation == OrbPresentation::Resting {
        reapply_resting_rect(&app);
    }
    Ok(())
}

#[tauri::command]
pub fn get_notch_inbox(app: AppHandle) -> Result<NotchInboxPayload, String> {
    let state = app.state::<crate::AppState>();
    let runtime = orb_lock(&state)?;
    Ok(NotchInboxPayload {
        items: runtime.inbox_items.clone(),
    })
}

#[tauri::command]
pub fn orb_toggle_voice_drawer(app: AppHandle) -> Result<(), String> {
    let (open, presentation) = {
        let state = app.state::<crate::AppState>();
        let mut runtime = orb_lock(&state)?;
        if matches!(
            runtime.presentation,
            OrbPresentation::Spotlight | OrbPresentation::Settling | OrbPresentation::Expanded
        ) {
            return Ok(());
        }
        let open = !runtime.voice_drawer_open;
        runtime.voice_drawer_open = open;
        runtime.voice_drawer_hold_closed = !open;
        // A tap owns the strip until the next tap. Talk does not time it out.
        runtime.voice_drawer_manual = open;
        runtime.voice_drawer_idle_generation =
            runtime.voice_drawer_idle_generation.saturating_add(1);
        (open, runtime.presentation)
    };
    emit_current_presentation(&app);
    if presentation == OrbPresentation::Hidden && open {
        show_at(&app, OrbPresentation::Resting, false);
    } else {
        reapply_resting_rect(&app);
    }
    Ok(())
}

#[tauri::command]
pub fn notch_set_inbox_open(app: AppHandle, open: bool) -> Result<(), String> {
    let presentation = {
        let state = app.state::<crate::AppState>();
        let mut runtime = orb_lock(&state)?;
        // The tray ear opens this drawer with nothing waiting. The panel then
        // says so, instead of refusing the tap. It opens on Attention; a run
        // dot opens it on the runs (`notch_show_runs`).
        runtime.inbox_open = open;
        if open {
            runtime.drawer_view = NotchDrawerView::Attention;
            runtime.run_label_open = false;
        }
        runtime.presentation
    };
    emit_current_presentation(&app);
    if presentation == OrbPresentation::Resting {
        reapply_resting_rect(&app);
    }
    Ok(())
}

#[tauri::command]
pub fn notch_set_inbox_height(app: AppHandle, height: f64) -> Result<(), String> {
    if !height.is_finite() {
        return Ok(());
    }
    let apply = {
        let state = app.state::<crate::AppState>();
        let mut runtime = orb_lock(&state)?;
        let next = height.clamp(NOTCH_INBOX_MIN_HEIGHT_PT, NOTCH_INBOX_MAX_HEIGHT_PT);
        let changed = (runtime.inbox_height_pt - next).abs() > 0.5;
        runtime.inbox_height_pt = next;
        changed && runtime.inbox_open && runtime.presentation == OrbPresentation::Resting
    };
    if apply {
        reapply_resting_rect(&app);
    }
    Ok(())
}

fn notch_nonempty(value: &Option<String>) -> Option<&str> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn notch_input_type(answer: &NotchAnswer) -> &str {
    notch_nonempty(&answer.input_type).unwrap_or(answer.kind.as_str())
}

/// The canonical respond body for an inline notch answer.
/// `None` means this kind is not posted from the host (dismiss, open, or empty).
fn notch_post_body(answer: &NotchAnswer) -> Option<serde_json::Value> {
    let source = notch_nonempty(&answer.source)?;
    let input_type = notch_input_type(answer);
    let value = match input_type {
        "text" => {
            let text = answer.text.as_deref().unwrap_or("").trim();
            if text.is_empty() {
                return None;
            }
            serde_json::json!({ "type": "text", "value": text })
        },
        "guidance" => {
            let text = answer.text.as_deref().unwrap_or("").trim();
            if text.is_empty() {
                return None;
            }
            serde_json::json!({ "type": "guidance", "advice": text })
        },
        "confirmation" => {
            let choice = answer.choice_id.as_deref().unwrap_or("").trim();
            let confirmed = matches!(choice, "confirm" | "approve" | "approve_run" | "yes");
            serde_json::json!({ "type": "confirmation", "confirmed": confirmed })
        },
        "multi_choice" => {
            let ids: Vec<&str> = answer
                .choice_ids
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|id| id.trim())
                .filter(|id| !id.is_empty())
                .collect();
            if ids.is_empty() {
                return None;
            }
            serde_json::json!({ "type": "multi_choice", "selected_ids": ids })
        },
        "choice" | "tool_authorization" | "sandbox_override" | "diff_approval" => {
            let choice = answer.choice_id.as_deref().unwrap_or("").trim();
            if choice.is_empty() {
                return None;
            }
            serde_json::json!({ "type": "choice", "selected_id": choice })
        },
        _ => return None,
    };
    let mut body = serde_json::json!({
        "source": source,
        "input_type": input_type,
        "value": value,
        "channel": "web",
    });
    if let Some(execution_id) = notch_nonempty(&answer.execution_id) {
        body["execution_id"] = serde_json::Value::String(execution_id.to_string());
    }
    if let Some(task_id) = notch_nonempty(&answer.task_id) {
        body["task_id"] = serde_json::Value::String(task_id.to_string());
    }
    Some(body)
}

fn notch_respond_url(origin: &str, correlation_id: &str) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(origin)
        .map_err(|_| "The Magician server address is invalid.".to_string())?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| "The Magician server address is invalid.".to_string())?;
        segments.pop_if_empty();
        segments.extend(["api", "magician", "v2", "hitl", correlation_id, "respond"]);
    }
    Ok(url)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotchPostClass {
    Posted,
    /// The ask is already resolved, or the server has no such question.
    Gone,
    Failed,
}

fn classify_notch_post(status: u16, body: &str) -> NotchPostClass {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    let reason = parsed
        .as_ref()
        .and_then(|value| value.get("reason"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if reason == "already_resolved" || reason == "mcp_oauth_flow_not_live" {
        return NotchPostClass::Gone;
    }
    if (200..300).contains(&status) {
        if parsed
            .as_ref()
            .and_then(|value| value.get("status"))
            .and_then(|value| value.as_str())
            == Some("reask_required")
        {
            return NotchPostClass::Failed;
        }
        if parsed.as_ref().and_then(|value| value.get("resumed"))
            == Some(&serde_json::Value::Bool(false))
        {
            return NotchPostClass::Failed;
        }
        if parsed.as_ref().and_then(|value| value.get("accepted"))
            == Some(&serde_json::Value::Bool(false))
        {
            return NotchPostClass::Failed;
        }
        return NotchPostClass::Posted;
    }
    // A missing or already-consumed ask must leave the notch. Keeping it
    // is what made an answered question sit there until the tray restarted.
    if status == 404 || status == 410 {
        return NotchPostClass::Gone;
    }
    NotchPostClass::Failed
}

enum NotchHostPost {
    Skip,
    Posted,
    Gone,
    Failed(String),
}

async fn engine_origin(app: &AppHandle) -> Result<String, String> {
    let state = app.state::<crate::AppState>();
    let config = state.config.lock().await;
    Ok(config.engine_base_url())
}

async fn post_notch_answer(app: &AppHandle, answer: &NotchAnswer) -> NotchHostPost {
    let Some(body) = notch_post_body(answer) else {
        return NotchHostPost::Skip;
    };
    let origin = match engine_origin(app).await {
        Ok(origin) => origin,
        Err(error) => return NotchHostPost::Failed(error),
    };
    let url = match notch_respond_url(&origin, answer.correlation_id.trim()) {
        Ok(url) => url,
        Err(error) => return NotchHostPost::Failed(error),
    };
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return NotchHostPost::Failed(format!("The answer did not send. {error}"));
        },
    };
    let response = match crate::magician_auth::authorize(client.post(url).json(&body))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            warn!("notch answer did not reach the server: {error}");
            return NotchHostPost::Failed("The answer did not send.".to_string());
        },
    };
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    info!(
        "notch answer status={status} correlation={}",
        answer.correlation_id
    );
    match classify_notch_post(status, &text) {
        NotchPostClass::Posted => NotchHostPost::Posted,
        NotchPostClass::Gone => NotchHostPost::Gone,
        NotchPostClass::Failed => NotchHostPost::Failed("The answer did not send.".to_string()),
    }
}

fn queue_notch_answer(app: &AppHandle, answer: NotchAnswer) -> Result<(), String> {
    {
        let state = app.state::<crate::AppState>();
        let mut runtime = orb_lock(&state)?;
        runtime.pending_answers.push(answer.clone());
    }
    app.emit(NOTCH_ANSWER_EVENT, answer)
        .map_err(|error| format!("failed to send the notch answer: {error}"))
}

fn report_notch_answer(app: &AppHandle, id: &str, ok: bool, message: Option<String>) {
    let _ = app.emit(
        NOTCH_REPORT_EVENT,
        NotchReport {
            id: id.to_string(),
            ok,
            message,
        },
    );
}

/// The orb renders the question. The host posts the answer on its own
/// connection, then the notify overlay drops the card. Dismiss and Open
/// stay with the overlay. A missed event is still drained.
#[tauri::command]
pub async fn notch_answer(app: AppHandle, mut answer: NotchAnswer) -> Result<(), String> {
    match post_notch_answer(&app, &answer).await {
        NotchHostPost::Posted | NotchHostPost::Gone => {
            answer.posted = true;
            report_notch_answer(&app, &answer.id, true, None);
        },
        NotchHostPost::Failed(message) => {
            report_notch_answer(&app, &answer.id, false, Some(message));
        },
        NotchHostPost::Skip => {},
    }
    queue_notch_answer(&app, answer)
}

#[tauri::command]
pub fn notch_report(
    app: AppHandle,
    id: String,
    ok: bool,
    message: Option<String>,
) -> Result<(), String> {
    app.emit(NOTCH_REPORT_EVENT, NotchReport { id, ok, message })
        .map_err(|error| format!("failed to report the notch answer: {error}"))
}

/// Take every queued notch answer. The overlay calls this from the answer
/// event and from a short timer.
#[tauri::command]
pub fn drain_notch_answers(app: AppHandle) -> Result<Vec<NotchAnswer>, String> {
    let state = app.state::<crate::AppState>();
    let mut runtime = orb_lock(&state)?;
    Ok(std::mem::take(&mut runtime.pending_answers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listening_cap_stops_capture_without_removing_the_enabled_station() {
        for parked in [false, true] {
            let mut machine = OrbMachine::default();
            machine.apply(OrbAction::Arm, 1_000);
            if parked {
                machine.apply(
                    OrbAction::WakeHeard {
                        phrase: "test".into(),
                    },
                    2_000,
                );
                machine.apply(OrbAction::Connected, 2_001);
                machine.apply(OrbAction::HoldReady, 3_000);
            }
            let deadline = machine.snapshot().leash_deadline_ms.unwrap();
            assert!(!machine.apply(OrbAction::Tick, deadline - 1));
            assert!(machine.apply(OrbAction::Tick, deadline));
            assert!(matches!(
                machine.state(),
                OrbState::Disarming {
                    reason: OrbEndedReason::CapReached
                }
            ));
            machine.apply(OrbAction::CompleteDisarm, deadline + 760);
            let ended = machine.snapshot();
            assert_eq!(ended.state, "ended");
            assert!(!ended.window_open, "the capture/keep-awake lease must end");
            assert!(ended.leash_deadline_ms.is_none());
            assert_eq!(idle_station(false, true), IdleStation::VoiceAndTray);
            assert!(terminal_settle_is_current(&ended, ended.revision));
            assert!(!machine.apply(OrbAction::Tick, deadline + 24 * 60 * 60 * 1_000));
            assert!(
                !machine.snapshot().window_open,
                "dock visibility must not rearm capture"
            );
        }
    }

    #[test]
    fn voice_failures_leave_the_enabled_dock_available_for_recovery() {
        for reason in [
            OrbEndedReason::SessionFailed,
            OrbEndedReason::MicrophoneLost,
            OrbEndedReason::PowerPolicy,
        ] {
            let mut machine = OrbMachine::default();
            machine.apply(OrbAction::Arm, 0);
            machine.apply(OrbAction::Disarm { reason }, 1_000);
            machine.apply(OrbAction::CompleteDisarm, 1_760);
            assert_eq!(machine.snapshot().phase, Some(OrbPhase::Ended));
            assert!(!machine.snapshot().window_open);
            assert_eq!(idle_station(false, true), IdleStation::VoiceAndTray);
        }
    }

    #[test]
    fn explicit_rest_and_setup_gate_preserve_their_visibility_rules() {
        // The tray ear is available even when the Orb is off and nothing waits.
        assert_eq!(idle_station(false, false), IdleStation::TrayOnly);
        assert_eq!(idle_station(false, true), IdleStation::VoiceAndTray);
        for visible in [false, true] {
            assert_eq!(idle_station(true, visible), IdleStation::Hidden);
        }
    }

    #[test]
    fn hiding_voice_preserves_the_tray_drawer_and_invalidates_the_voice_strip_timer() {
        let mut runtime = OrbWindowRuntime::new(true);
        runtime.inbox_open = true;
        runtime.voice_drawer_open = true;
        runtime.voice_drawer_manual = true;
        let generation = runtime.voice_drawer_idle_generation;
        runtime.set_voice_visible(false);
        let edge = edge_frame(&runtime);
        assert!(edge.attention_only);
        assert!(
            edge.inbox_open,
            "hiding voice must not close a question drawer"
        );
        assert!(!edge.voice_open);
        assert!(runtime.voice_drawer_idle_generation > generation);
        runtime.set_voice_visible(true);
        assert!(!edge_frame(&runtime).attention_only);
        assert!(runtime.inbox_open);
    }

    #[test]
    fn terminal_settle_cannot_apply_to_a_rearmed_or_newer_ended_lifecycle() {
        let mut machine = OrbMachine::default();
        machine.apply(OrbAction::Arm, 0);
        machine.apply(
            OrbAction::Disarm {
                reason: OrbEndedReason::SessionFailed,
            },
            10,
        );
        machine.apply(OrbAction::CompleteDisarm, 770);
        let old_revision = machine.snapshot().revision;
        assert!(terminal_settle_is_current(
            &machine.snapshot(),
            old_revision
        ));
        machine.apply(OrbAction::Arm, 800);
        assert!(!terminal_settle_is_current(
            &machine.snapshot(),
            old_revision
        ));
        machine.apply(
            OrbAction::Disarm {
                reason: OrbEndedReason::UserDisarm,
            },
            810,
        );
        machine.apply(OrbAction::CompleteDisarm, 1_570);
        assert_eq!(machine.snapshot().state, "ended");
        assert!(!terminal_settle_is_current(
            &machine.snapshot(),
            old_revision
        ));
    }

    fn sample_answer(kind: &str) -> NotchAnswer {
        NotchAnswer {
            id: "card".to_string(),
            correlation_id: "pause/id with spaces".to_string(),
            kind: kind.to_string(),
            choice_id: None,
            choice_ids: None,
            text: None,
            source: Some("user_request".to_string()),
            input_type: Some(kind.to_string()),
            execution_id: None,
            task_id: None,
            posted: false,
        }
    }

    #[test]
    fn notch_text_answer_posts_the_canonical_body() {
        let mut answer = sample_answer("text");
        answer.text = Some("  ok  ".to_string());
        answer.execution_id = Some("exec-1".to_string());
        let body = notch_post_body(&answer).expect("text body");
        assert_eq!(body["source"], "user_request");
        assert_eq!(body["input_type"], "text");
        assert_eq!(body["value"]["type"], "text");
        assert_eq!(body["value"]["value"], "ok");
        assert_eq!(body["channel"], "web");
        assert_eq!(body["execution_id"], "exec-1");
        assert!(notch_post_body(&sample_answer("text")).is_none());
        assert!(notch_post_body(&sample_answer("dismiss")).is_none());
        let url = notch_respond_url("http://127.0.0.1:3002", &answer.correlation_id).unwrap();
        assert_eq!(
            url.as_str(),
            "http://127.0.0.1:3002/api/magician/v2/hitl/pause%2Fid%20with%20spaces/respond"
        );
    }

    #[test]
    fn notch_post_treats_a_missing_or_settled_ask_as_gone() {
        assert_eq!(
            classify_notch_post(200, r#"{"resolved":true,"debug":true}"#),
            NotchPostClass::Posted
        );
        assert_eq!(
            classify_notch_post(409, r#"{"reason":"already_resolved"}"#),
            NotchPostClass::Gone
        );
        assert_eq!(classify_notch_post(404, ""), NotchPostClass::Gone);
        assert_eq!(classify_notch_post(410, ""), NotchPostClass::Gone);
        assert_eq!(
            classify_notch_post(200, r#"{"accepted":false,"reason":"nope"}"#),
            NotchPostClass::Failed
        );
        assert_eq!(classify_notch_post(500, ""), NotchPostClass::Failed);
    }

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
    fn the_runs_ear_grows_the_station_one_row_per_run_up_to_its_cap() {
        assert_eq!(runs_ear_height_pt(0), 0.0);
        let one = runs_ear_height_pt(1);
        let full = runs_ear_height_pt(RUN_EAR_MAX_ROWS);
        assert_eq!(full - one, (RUN_EAR_MAX_ROWS - 1) as f64 * RUN_EAR_ROW_PT);
        assert_eq!(runs_ear_height_pt(RUN_EAR_MAX_ROWS + 5), full);
        let with_runs = EdgeFrame {
            run_rows: 2,
            ..EdgeFrame::default()
        };
        assert_eq!(
            station_height_pt(&with_runs),
            NOTCH_STACK_HEIGHT_PT + runs_ear_height_pt(2)
        );
        let tray_only = EdgeFrame {
            attention_only: true,
            ..EdgeFrame::default()
        };
        assert_eq!(station_height_pt(&tray_only), EDGE_PILL_HEIGHT_PT);
    }

    #[test]
    fn the_side_panel_opens_on_attention_or_on_the_runs_with_the_tapped_run() {
        let runs = |id: &str| NotchDrawerView::Runs {
            selected: Some(id.to_string()),
        };
        assert_eq!(
            notch_panel_url("http://127.0.0.1:5173/", &NotchDrawerView::Attention),
            "http://127.0.0.1:5173/attention?native_attention=1&notch_attention=1"
        );
        assert_eq!(
            notch_panel_url(
                "http://127.0.0.1:5173",
                &NotchDrawerView::Runs {
                    selected: Some("task_a&b".to_string())
                }
            ),
            "http://127.0.0.1:5173/coding-runs?native_attention=1&notch_attention=1&run=task_a%26b"
        );
        assert_eq!(
            notch_panel_url("http://127.0.0.1:5173", &NotchDrawerView::Runs { selected: None }),
            "http://127.0.0.1:5173/coding-runs?native_attention=1&notch_attention=1"
        );
        // Work item dots: a coding run keeps `run=`, a crew or task is `item=`.
        assert_eq!(
            notch_panel_url("http://127.0.0.1:5173", &runs("run:task_a")),
            "http://127.0.0.1:5173/coding-runs?native_attention=1&notch_attention=1&run=task_a"
        );
        assert_eq!(
            notch_panel_url("http://127.0.0.1:5173", &runs("crew:crew_1")),
            "http://127.0.0.1:5173/coding-runs?native_attention=1&notch_attention=1&item=crew%3Acrew_1"
        );
        assert_eq!(
            notch_panel_url("http://127.0.0.1:5173", &runs("task:task_9")),
            "http://127.0.0.1:5173/coding-runs?native_attention=1&notch_attention=1&item=task%3Atask_9"
        );
    }

    #[test]
    fn a_run_dot_without_a_path_still_parses() {
        let dot: NotchRunDot =
            serde_json::from_str(r#"{"id":"task_a","state":"working","title":"Fix"}"#).unwrap();
        assert_eq!(dot.path, None);
        let dot: NotchRunDot = serde_json::from_str(
            r#"{"id":"crew:c","state":"needs_you","title":"Ship","path":"/vibe?project=p&crew=c"}"#,
        )
        .unwrap();
        assert_eq!(dot.path.as_deref(), Some("/vibe?project=p&crew=c"));
    }

    #[test]
    fn native_sizes_keep_the_resting_surface_quiet_and_the_wake_surface_generous() {
        assert!(EDGE_PILL_HEIGHT_PT <= 80.0);
        assert!(EDGE_PILL_HEIGHT_PT >= 64.0);
        assert!(NOTCH_STACK_HEIGHT_PT > EDGE_PILL_HEIGHT_PT);
        assert!(NOTCH_STACK_HEIGHT_PT < 160.0);
        assert!(NOTCH_DRAWER_WIDTH_PT >= 420.0);
        assert!(NOTCH_DRAWER_HEIGHT_PT >= 520.0);
        assert!(NOTCH_DRAWER_HEIGHT_PT <= 640.0);
        assert_eq!(VOICE_DRAWER_HEIGHT_PT, EDGE_PILL_HEIGHT_PT * 2.0);
        assert!(VOICE_DRAWER_WIDTH_PT >= 300.0);
        assert!(VOICE_DRAWER_WIDTH_PT < NOTCH_DRAWER_WIDTH_PT);
        assert!(VOICE_DRAWER_HEIGHT_PT < NOTCH_DRAWER_HEIGHT_PT);
        assert_eq!(VOICE_DRAWER_IDLE_MS, 5_000);
        assert_eq!(DRAWER_SPRING_MS, 260);
        assert!(DRAWER_EXIT_MS >= DRAWER_SPRING_MS);
        assert!(DRAWER_EXIT_MS - DRAWER_SPRING_MS <= 40);
        assert!(EDGE_PEEK_WIDTH_PT < RESTING_MIN_WIDTH_PT);
        assert!(EDGE_PILL_HEIGHT_PT < EXPANDED_HEIGHT_PT);
        assert!(EXPANDED_WIDTH_PT >= 360.0);
        assert!(SPOTLIGHT_WIDTH_PT > EXPANDED_WIDTH_PT);
        assert!(SPOTLIGHT_HEIGHT_PT > EXPANDED_HEIGHT_PT);
    }

    #[test]
    fn wake_stays_on_the_ear_and_opens_the_transcript_strip() {
        let source = include_str!("orb_window.rs");
        let wake = source
            .split_once("pub fn present_wake(")
            .expect("wake presenter")
            .1
            .split_once("pub fn current_snapshot(")
            .expect("snapshot follows wake presenter")
            .0;

        assert!(wake.contains("reveal_docked_voice_drawer"));
        assert!(!wake.contains("OrbPresentation::Spotlight"));
        let talk = source
            .split_once("fn sync_docked_voice_drawer(")
            .expect("docked talk sync")
            .1
            .split_once("pub fn present_wake(")
            .expect("wake follows the sync")
            .0;
        assert!(talk.contains("present_wake"));
        assert!(!talk.contains("expand_without_focus"));
        let idle = source
            .split_once("fn arm_voice_drawer_idle(")
            .expect("idle close")
            .1
            .split_once("fn sync_docked_voice_drawer(")
            .expect("sync follows idle")
            .0;
        assert!(idle.contains("VOICE_DRAWER_IDLE_MS"));
        assert!(idle.contains("voice_drawer_manual"));
        let toggle = source
            .split_once("pub fn orb_toggle_voice_drawer(")
            .expect("voice drawer toggle")
            .1
            .split_once("pub fn notch_set_inbox_open(")
            .expect("inbox toggle follows the voice toggle")
            .0;
        assert!(toggle.contains("voice_drawer_manual = open"));
        assert!(!toggle.contains("arm_voice_drawer_idle"));
    }

    #[test]
    fn closing_drawer_stops_on_the_bezel() {
        for step in 0..=40 {
            let t = step as f64 / 40.0;
            let closing = axis_progress(MotionCurve::Spring, t, true);
            assert!(closing <= 1.0, "closing {closing} at {t}");
            assert!((closing - ease_out_quint(t)).abs() < 1e-9);
        }
        let mut peak = 0.0_f64;
        for step in 0..=40 {
            peak = peak.max(axis_progress(
                MotionCurve::Spring,
                step as f64 / 40.0,
                false,
            ));
        }
        assert!(peak > 1.02, "opening peak {peak}");
        assert!(peak < 1.12, "opening peak {peak}");
        let settled = axis_progress(MotionCurve::Settle, 0.5, false);
        assert!((settled - ease_out_quint(0.5)).abs() < 1e-9);
    }

    #[test]
    fn drawer_spring_settles_with_a_small_overshoot() {
        assert!(drawer_spring(0.0).abs() < 0.001);
        assert!((drawer_spring(1.0) - 1.0).abs() < 0.01);
        let mut peak = 0.0_f64;
        for step in 0..=40 {
            peak = peak.max(drawer_spring(step as f64 / 40.0));
        }
        assert!(peak > 1.02, "peak {peak}");
        assert!(peak < 1.12, "peak {peak}");
    }

    #[test]
    fn opening_sample_keeps_a_shared_right_edge() {
        let voice_start = PhysicalPosition::new(1640, 320);
        let voice_size = PhysicalSize::new(40, 136);
        let voice = OrbRect {
            x: 1320,
            y: 320,
            width: 360,
            height: 136,
        };
        let attention = OrbRect {
            x: 1240,
            y: 320,
            width: 440,
            height: 560,
        };
        let attention_size = PhysicalSize::new(40, 136);
        for target in [voice, attention] {
            let start_size = if target.width == voice.width {
                voice_size
            } else {
                attention_size
            };
            let right = voice_start.x + start_size.width as i32;
            assert_eq!(right, target.x + target.width as i32);
            for step in 0..=20 {
                let progress = drawer_spring(step as f64 / 20.0);
                let sample = motion_sample(voice_start, start_size, target, progress, progress);
                let sample_right = sample.x + sample.width as i32;
                assert!(
                    (sample_right - right).abs() <= 1,
                    "right {sample_right} at step {step}"
                );
            }
        }
    }

    #[test]
    fn drawer_open_frames_move_and_resize_together() {
        let source = include_str!("orb_window.rs");
        let animate = source
            .split_once("fn animate_to(")
            .expect("animate")
            .1
            .split_once("fn placement_from_config(")
            .expect("placement follows animate")
            .0;
        assert!(!animate.contains("set_position"));
        assert!(!animate.contains("set_size"));
        assert!(animate.contains("outer_size"));
        assert!(!animate.contains("inner_size"));
        assert!(animate.contains("schedule_motion_frames"));
        assert!(source.contains("fn dispatch_main_after("));
        assert!(source.contains("dispatch_after"));
        let prepare = source
            .split_once("fn prepare_page_for_motion(window: &tauri::WebviewWindow, target: OrbRect, track_window: bool) {")
            .expect("prepare page")
            .1
            .split_once("fn on_main_thread()")
            .expect("main-thread check follows prepare")
            .0;
        assert!(prepare.contains("if track_window"));
        assert!(prepare.contains("pin_page_to_size"));
        let center = source
            .split_once("pub fn spotlight(")
            .expect("spotlight")
            .1
            .split_once("pub fn toggle(")
            .expect("toggle follows spotlight")
            .0;
        assert!(center.contains("close_voice_strip"));
    }

    #[test]
    fn resting_drawer_motion_does_not_save_a_drag() {
        let source = include_str!("orb_window.rs");
        let save = source
            .split_once("fn schedule_orb_placement_save(")
            .expect("placement save")
            .1
            .split_once("pub fn initialize_orb_window(")
            .expect("init follows placement save")
            .0;
        assert!(save.contains("motion_active"));
        let reapply = source
            .split_once("fn reapply_resting_rect(")
            .expect("resting reapply")
            .1
            .split_once("fn setup_blocks_orb(")
            .expect("setup gate follows reapply")
            .0;
        assert!(reapply.contains("MotionCurve::Spring"));
        assert!(reapply.contains("DRAWER_SPRING_MS"));
        assert!(reapply.contains("shrinking"));
        assert!(reapply.contains("outer_size"));
        assert!(!reapply.contains("inner_size"));
        assert!(reapply.contains("DRAWER_EXIT_MS"));
        let toggle = source
            .split_once("pub fn notch_set_inbox_open(")
            .expect("inbox toggle")
            .1
            .split_once("pub fn notch_set_inbox_height(")
            .expect("height follows inbox toggle")
            .0;
        assert!(toggle.contains("runtime.inbox_open = open"));
        assert!(!toggle.contains("is_empty"));
    }

    #[test]
    fn expand_opens_the_center_orb_not_a_top_panel() {
        let source = include_str!("orb_window.rs");
        let expand = source
            .split_once("pub fn expand(")
            .expect("expand")
            .1
            .split_once("fn expand_without_focus(")
            .expect("quiet expand follows")
            .0;
        assert!(expand.contains("OrbPresentation::Spotlight"));
        assert!(!expand.contains("OrbPresentation::Expanded"));
        let quiet = source
            .split_once("fn expand_without_focus(")
            .expect("quiet expand")
            .1
            .split_once("pub fn collapse(")
            .expect("collapse follows")
            .0;
        assert!(quiet.contains("OrbPresentation::Spotlight"));
        assert!(!quiet.contains("OrbPresentation::Expanded"));
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
            EdgeFrame::default(),
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
            EdgeFrame::default(),
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
            EdgeFrame::default(),
        );

        assert_eq!(resting.y, scaled(32.0 + EDGE_BELOW_MENU_PT, 2.0) as i32);
        assert!(resting.y > position.y);
        assert_eq!(resting.width, scaled(EDGE_PEEK_WIDTH_PT, 2.0));
        assert_eq!(resting.height, scaled(NOTCH_STACK_HEIGHT_PT, 2.0));
        assert_eq!(
            resting.x + resting.width as i32,
            position.x + size.width as i32
        );
        // Opening the old edge flag, or a saved spot, does not grow a pill.
        assert_eq!(revealed.width, resting.width);
        assert_eq!(revealed.height, resting.height);
        assert_eq!(revealed.x, resting.x);
        assert_eq!(revealed.y, resting.y);
        let saved = rect_for_monitor(
            &position,
            &size,
            2.0,
            notch,
            OrbPresentation::Resting,
            Some(OrbPlacement { x: 0.2, y: 0.4 }),
            true,
            false,
            EdgeFrame::default(),
        );
        assert_eq!(saved.x, resting.x);
        assert_eq!(saved.y, resting.y);
        assert_eq!(saved.width, resting.width);
        assert_eq!(saved.height, resting.height);
        // A leftover Expanded frame is the center Orb, not a top panel.
        assert_eq!(expanded.width, scaled(SPOTLIGHT_WIDTH_PT, 2.0));
        assert_eq!(expanded.height, scaled(SPOTLIGHT_HEIGHT_PT, 2.0));
        assert_eq!(
            expanded.x + expanded.width as i32 / 2,
            size.width as i32 / 2
        );
        assert_eq!(
            expanded.y + expanded.height as i32 / 2,
            size.height as i32 / 2
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
            EdgeFrame::default(),
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
            EdgeFrame::default(),
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
    fn open_inbox_anchors_to_the_right_edge_and_ignores_a_saved_spot() {
        let position = PhysicalPosition::new(0, 0);
        let size = PhysicalSize::new(3_024, 1_964);
        let notch = NotchMetrics {
            top_inset_points: 32.0,
            notch_width_points: 150.0,
            has_notch: true,
        };
        let parked = Some(OrbPlacement { x: 0.2, y: 0.4 });
        let closed = rect_for_monitor(
            &position,
            &size,
            2.0,
            notch,
            OrbPresentation::Resting,
            parked,
            false,
            false,
            EdgeFrame::default(),
        );
        let open = rect_for_monitor(
            &position,
            &size,
            2.0,
            notch,
            OrbPresentation::Resting,
            parked,
            false,
            false,
            EdgeFrame {
                inbox_open: true,
                attention_only: false,
                voice_open: false,
                dock_y: None,
                run_rows: 0,
                run_label_open: false,
            },
        );
        assert_eq!(
            closed.x + closed.width as i32,
            position.x + size.width as i32
        );
        assert_eq!(closed.width, scaled(EDGE_PEEK_WIDTH_PT, 2.0));
        assert_eq!(closed.height, scaled(NOTCH_STACK_HEIGHT_PT, 2.0));
        assert_eq!(open.x + open.width as i32, position.x + size.width as i32);
        assert_eq!(open.width, scaled(NOTCH_DRAWER_WIDTH_PT, 2.0));
        assert_eq!(open.height, scaled(NOTCH_DRAWER_HEIGHT_PT, 2.0));
        let home = rect_for_monitor(
            &position,
            &size,
            2.0,
            notch,
            OrbPresentation::Resting,
            None,
            false,
            false,
            EdgeFrame {
                inbox_open: false,
                attention_only: false,
                voice_open: false,
                dock_y: None,
                run_rows: 0,
                run_label_open: false,
            },
        );
        // The drawer grows left from the station. Opening it keeps the ears
        // at the saved height. A saved spot does not move them.
        assert!(open.x < home.x);
        assert_eq!(open.y, home.y);
        assert_eq!(open.x + open.width as i32, home.x + home.width as i32);

        let tray_only = rect_for_monitor(
            &position,
            &size,
            2.0,
            notch,
            OrbPresentation::Resting,
            parked,
            false,
            false,
            EdgeFrame {
                inbox_open: false,
                attention_only: true,
                voice_open: false,
                dock_y: None,
                run_rows: 0,
                run_label_open: false,
            },
        );
        assert_eq!(tray_only.width, scaled(EDGE_PEEK_WIDTH_PT, 2.0));
        assert_eq!(tray_only.height, scaled(EDGE_PILL_HEIGHT_PT, 2.0));
        assert_eq!(
            tray_only.x + tray_only.width as i32,
            position.x + size.width as i32
        );
        assert_eq!(tray_only.y, home.y);

        let voice = rect_for_monitor(
            &position,
            &size,
            2.0,
            notch,
            OrbPresentation::Resting,
            parked,
            false,
            false,
            EdgeFrame {
                inbox_open: false,
                attention_only: false,
                voice_open: true,
                dock_y: None,
                run_rows: 0,
                run_label_open: false,
            },
        );
        assert_eq!(voice.width, scaled(VOICE_DRAWER_WIDTH_PT, 2.0));
        // The overlapping ears (106pt) are shorter than the 128pt strip, so
        // the window grows down to fit it; its top stays on the station.
        assert_eq!(
            voice.height,
            scaled(NOTCH_STACK_HEIGHT_PT.max(VOICE_DRAWER_HEIGHT_PT), 2.0)
        );
        assert!(voice.height >= home.height);
        assert_eq!(voice.y, home.y);
        assert!(voice.x < home.x);
        assert_eq!(voice.x + voice.width as i32, home.x + home.width as i32);

        let wake = rect_for_monitor(
            &position,
            &size,
            2.0,
            notch,
            OrbPresentation::Spotlight,
            None,
            false,
            false,
            EdgeFrame {
                inbox_open: true,
                attention_only: true,
                voice_open: true,
                dock_y: None,
                run_rows: 0,
                run_label_open: false,
            },
        );
        assert_eq!(wake.width, scaled(SPOTLIGHT_WIDTH_PT, 2.0));
        assert_eq!(wake.height, scaled(SPOTLIGHT_HEIGHT_PT, 2.0));
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
    fn saved_spots_do_not_undock_the_ear_and_spotlight_never_claims_the_notch() {
        let mut runtime = OrbWindowRuntime::default();
        assert!(presentation_is_docked(&runtime, OrbPresentation::Resting));
        assert!(presentation_is_docked(&runtime, OrbPresentation::Expanded));
        assert!(!presentation_is_docked(
            &runtime,
            OrbPresentation::Spotlight
        ));

        runtime.resting_placement = Some(OrbPlacement { x: 0.2, y: 0.3 });
        runtime.expanded_placement = Some(OrbPlacement { x: 0.7, y: 0.4 });
        assert!(presentation_is_docked(&runtime, OrbPresentation::Resting));
        assert!(presentation_is_docked(&runtime, OrbPresentation::Settling));
        assert!(presentation_is_docked(&runtime, OrbPresentation::Expanded));

        runtime.voice_visible = false;
        assert!(presentation_is_docked(&runtime, OrbPresentation::Resting));
        runtime.voice_visible = true;
        runtime.inbox_open = true;
        assert!(presentation_is_docked(&runtime, OrbPresentation::Resting));
        assert!(!presentation_is_docked(
            &runtime,
            OrbPresentation::Spotlight
        ));
    }

    #[test]
    fn magnetic_dock_has_a_bounded_snap_zone() {
        let dock = OrbRect {
            x: 400,
            y: 0,
            width: 214,
            height: 82,
        };
        let same = PhysicalSize::new(214, 82);
        assert!(window_is_near_dock(
            &PhysicalPosition::new(450, 45),
            same,
            dock,
            1.0,
        ));
        assert!(!window_is_near_dock(
            &PhysicalPosition::new(457, 0),
            same,
            dock,
            1.0,
        ));
        assert!(window_is_near_dock(
            &PhysicalPosition::new(510, 0),
            same,
            dock,
            2.0,
        ));

        // The parked pill is wider than the ear. A drop whose right edge meets
        // the ear still docks, even though the origins are far apart. The
        // vertical miss below stays outside this zone: Expanded still requires
        // the top. Resting docks at any height via resting_window_is_on_right_edge.
        let ear = OrbRect {
            x: 1_000,
            y: 40,
            width: scaled(EDGE_PEEK_WIDTH_PT, 1.0),
            height: scaled(EDGE_PILL_HEIGHT_PT, 1.0),
        };
        let pill = PhysicalSize::new(scaled(RESTING_MIN_WIDTH_PT, 1.0), ear.height);
        let flush_right = ear.x + ear.width as i32 - pill.width as i32;
        assert!(window_is_near_dock(
            &PhysicalPosition::new(flush_right, ear.y),
            pill,
            ear,
            1.0,
        ));
        assert!(!window_is_near_dock(
            &PhysicalPosition::new(flush_right - 57, ear.y),
            pill,
            ear,
            1.0,
        ));
        assert!(!window_is_near_dock(
            &PhysicalPosition::new(flush_right, ear.y + 57),
            pill,
            ear,
            1.0,
        ));
    }

    #[test]
    fn resting_notch_docks_anywhere_on_the_right_edge() {
        let position = PhysicalPosition::new(0, 0);
        let size = PhysicalSize::new(3_024, 1_964);
        let notch = NotchMetrics {
            top_inset_points: 32.0,
            notch_width_points: 150.0,
            has_notch: true,
        };
        let home = rect_for_monitor(
            &position,
            &size,
            2.0,
            notch,
            OrbPresentation::Resting,
            None,
            false,
            false,
            EdgeFrame::default(),
        );
        let lowered = rect_for_monitor(
            &position,
            &size,
            2.0,
            notch,
            OrbPresentation::Resting,
            None,
            false,
            false,
            EdgeFrame {
                dock_y: Some(1.0),
                ..EdgeFrame::default()
            },
        );
        assert!(lowered.y > home.y);
        assert_eq!(lowered.width, home.width);
        assert_eq!(lowered.height, home.height);
        assert_eq!(
            lowered.x + lowered.width as i32,
            position.x + size.width as i32
        );
        let expected_bottom = docked_resting_y(
            position.y,
            size.height,
            lowered.height,
            2.0,
            notch,
            Some(1.0),
        );
        assert_eq!(lowered.y, expected_bottom);
        assert_eq!(
            dock_y_fraction(
                expected_bottom,
                position.y,
                size.height,
                lowered.height,
                2.0,
                notch
            ),
            1.0
        );
        assert_eq!(
            dock_y_fraction(home.y, position.y, size.height, home.height, 2.0, notch),
            0.0
        );
        // A drop past the bottom margin clamps back onto the edge.
        assert_eq!(
            dock_y_fraction(
                expected_bottom + 400,
                position.y,
                size.height,
                lowered.height,
                2.0,
                notch
            ),
            1.0
        );

        let monitor_right = position.x + size.width as i32;
        let pill = PhysicalSize::new(scaled(RESTING_MIN_WIDTH_PT, 1.0), home.height);
        let flush_right = monitor_right - pill.width as i32;
        assert!(resting_window_is_on_right_edge(
            &PhysicalPosition::new(flush_right, home.y + 400),
            pill,
            monitor_right,
            1.0,
        ));
        assert!(!resting_window_is_on_right_edge(
            &PhysicalPosition::new(flush_right - 57, home.y + 400),
            pill,
            monitor_right,
            1.0,
        ));
    }
}
